//! The guard's checks on a session: what each action would press, the app
//! it goes to, and the decision (ask, refuse, or let it through).
//!
//! The screen judged is always the one passed in, never a cached reading.

use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use super::{
    confirm::{Answer, Ask, build_prompt},
    rules::{Action, Concern},
};
use crate::{
    actions::brief,
    ax::tree::{BuildOptions, Element, Screen},
    config::GuardMode,
    device::session::{Fresh, Session, plain_path, reuse},
    i18n::t,
    reply::{Fail, Res},
};

/// How far an approved target may move before it counts as another one.
const MOVED_TOO_FAR: f64 = 8.0;
/// The lock button may be pressed twice in this window; five fast presses
/// start Emergency SOS.
const LOCK_WINDOW: Duration = Duration::from_secs(10);
const LOCK_PRESSES: usize = 2;
const LOCK_PAGE: u64 = 0x0C;
const LOCK_USAGE: u64 = 0x30;
/// Parents looked at above a text or image target for the button it is in.
const ANCESTORS: usize = 3;
/// Calls that may wait on the dialog.
const ASKING: &[&str] = &["tap", "swipe", "type", "key", "launch", "batch", "script"];

/// What the guard remembers between actions of one session.
#[derive(Debug, Default)]
pub struct State {
    /// pid → bundle id (None: not an installed app), cleared on reset.
    pub bundles: HashMap<i64, Option<String>>,
    /// The last no or unanswered dialog, for the cooldown.
    pub refused_at: Option<Instant>,
    /// Recent presses of the lock button.
    pub lock_presses: VecDeque<Instant>,
}

/// What the person approved, to check it is still what will be pressed.
#[derive(Debug, Clone)]
pub struct Approval {
    pub asked: bool,
    prints: Vec<Option<Print>>,
}

#[derive(Debug, Clone, PartialEq)]
struct Print {
    pid: i64,
    role: String,
    label: Option<String>,
    center: Option<(f64, f64)>,
}

impl Print {
    fn of(e: &Element) -> Self {
        Print { pid: e.pid, role: e.role.clone(), label: e.label.clone(), center: e.frame.map(|f| f.center()) }
    }

    fn same_as(&self, e: &Element) -> bool {
        let now = Print::of(e);
        let close = match (self.center, now.center) {
            (Some((x1, y1)), Some((x2, y2))) => ((x1 - x2).powi(2) + (y1 - y2).powi(2)).sqrt() <= MOVED_TOO_FAR,
            (None, None) => true,
            _ => false,
        };
        self.pid == now.pid && self.role == now.role && self.label == now.label && close
    }
}

impl Approval {
    /// The approved targets, located again, are the same elements in about
    /// the same place; otherwise nothing is sent.
    pub fn still_valid(&self, _screen: &Screen, points: &[(f64, f64, Option<Element>)]) -> Res<()> {
        let same = self.prints.len() == points.len()
            && self.prints.iter().zip(points).all(|(was, (_, _, now))| match (was, now) {
                (Some(p), Some(e)) => p.same_as(e),
                (None, None) => true,
                _ => false,
            });
        if same {
            return Ok(());
        }
        Err(Fail::not_sent("target_changed", "the screen changed after the approval; nothing was sent").hint(t(
            "승인한 뒤 화면이 바뀌어 아무것도 보내지 않았습니다. 화면을 다시 읽고, 같은 동작이 필요하면 사용자에게 다시 확인받으세요.",
            "The screen changed after the approval, so nothing was sent. Read the screen again; if the action is still wanted, it will ask again.",
        )))
    }
}

/// Everything `decide` needs about one action.
struct Request<'a> {
    action: Action,
    concerns: Vec<Concern>,
    /// The app the input goes to, when the app lists need it.
    bundle: Option<String>,
    target: Option<String>,
    app_name: Option<String>,
    typed: Option<&'a str>,
    secret: bool,
    /// For blocked tap results: the element.
    element: Option<Value>,
}

impl<'a> Request<'a> {
    fn new(action: Action, concerns: Vec<Concern>, bundle: Option<String>) -> Self {
        Request { action, concerns, bundle, target: None, app_name: None, typed: None, secret: false, element: None }
    }
}

fn blocked_hint() -> &'static str {
    t(
        "iph 가드가 막았습니다. 다시 시도하거나 다른 방법으로 우회하지 말고, 무엇이 막혔는지 사용자에게 알리세요.",
        "Blocked by iph's guard. Do not retry or work around it; tell the user what was blocked.",
    )
}

fn name_of(e: &Element) -> Option<String> {
    e.label.clone().or_else(|| e.identifier.clone())
}

fn app_name(screen: &Screen, pid: Option<i64>) -> Option<String> {
    let pid = pid?;
    screen.apps.iter().find(|a| a.pid == pid).and_then(|a| a.name.clone())
}

/// The concerns, one per (category, rule).
fn dedupe(concerns: Vec<Concern>) -> Vec<Concern> {
    let mut out: Vec<Concern> = Vec::new();
    for c in concerns {
        if !out.iter().any(|o| o.category == c.category && o.rule == c.rule) {
            out.push(c);
        }
    }
    out
}

impl Session {
    /// Bundle id of the app running as `pid`, if it is an installed app.
    pub async fn bundle_for_pid(&mut self, pid: i64) -> Option<String> {
        if let Some(known) = self.guard_state.bundles.get(&pid) {
            return known.clone();
        }
        let mut svc = self.app_service().await.ok()?;
        let processes = tokio::time::timeout(Duration::from_secs(10), svc.list_processes())
            .await
            .ok()?
            .ok()?;
        let exe = processes
            .iter()
            .find(|p| p.pid as i64 == pid)
            .and_then(|p| p.executable_url.as_ref())
            .map(|u| plain_path(&u.relative))?;
        let apps = self.list_apps(false).await.ok()?;
        let found = apps
            .iter()
            .filter(|a| exe.starts_with(&format!("{}/", plain_path(&a.path))))
            .max_by_key(|a| a.path.len())
            .map(|a| a.bundle_identifier.clone());
        self.guard_state.bundles.insert(pid, found.clone());
        found
    }

    /// Seed the pid → bundle id cache (tests, and what is already known).
    pub fn remember_bundle(&mut self, pid: i64, bundle: Option<String>) {
        self.guard_state.bundles.insert(pid, bundle);
    }

    /// The bundle id input to `pid` goes to, looked up only when an app list
    /// is set.
    async fn bundle_of(&mut self, pid: Option<i64>) -> Option<String> {
        if !self.guard_rules.has_app_lists() {
            return None;
        }
        self.bundle_for_pid(pid?).await
    }

    /// How much longer a request may take because it may wait on the dialog.
    pub fn guard_extra_time(&self, cmd: &str) -> Duration {
        if self.guard_rules.mode() == GuardMode::Human && ASKING.contains(&cmd) {
            self.guard_rules.confirm_timeout() + Duration::from_secs(5)
        } else {
            Duration::ZERO
        }
    }

    fn blocked(&self, req: &Request<'_>, code: &str, message: impl Into<String>) -> Fail {
        let categories: Vec<&str> = req.concerns.iter().map(|c| c.category.as_str()).collect();
        let rules: Vec<&str> = req.concerns.iter().map(|c| c.rule.as_str()).collect();
        let mut f = Fail::blocked(code, message).hint(blocked_hint()).with(
            "guard",
            json!({
                "action": req.action.as_str(),
                "mode": self.guard_rules.mode().as_str(),
                "categories": categories,
                "rules": rules,
            }),
        );
        if let Some(e) = &req.element {
            f = f.with("element", e.clone());
        }
        f
    }

    /// Log a decision: mode, action, categories and rule words only. No
    /// labels, no bundle ids.
    fn record(&self, req: &Request<'_>, decision: &str) {
        let categories: Vec<&str> = req.concerns.iter().map(|c| c.category.as_str()).collect();
        let rules: Vec<&str> = req.concerns.iter().map(|c| c.rule.as_str()).collect();
        crate::events::record(
            "guard",
            decision,
            0,
            json!({
                "mode": self.guard_rules.mode().as_str(),
                "action": req.action.as_str(),
                "categories": categories,
                "rules": rules,
            }),
        );
    }

    /// The decision. `Ok(true)`: the person approved. `Ok(false)`: nothing
    /// to ask about. `Err`: blocked.
    async fn decide(&mut self, mut req: Request<'_>) -> Res<bool> {
        // 1. iph's own fixed actions.
        if self.guard_suspended {
            return Ok(false);
        }
        // 2. App lists, in every mode.
        if self.guard_rules.has_app_lists()
            && let Err((code, message)) = self.guard_rules.app_policy(req.bundle.as_deref())
        {
            req.concerns.clear();
            self.record(&req, code);
            return Err(self.blocked(&req, code, message));
        }
        // 3. Nothing more when the guard is off.
        let mode = self.guard_rules.mode();
        if mode == GuardMode::Off {
            return Ok(false);
        }
        // 4. Only the categories that are guarded.
        req.concerns = dedupe(self.guard_rules.filter(std::mem::take(&mut req.concerns)));
        if req.concerns.is_empty() {
            return Ok(false);
        }
        // 5. A tap list is never asked about: one approval cannot cover a row.
        if req.action == Action::TapSequence {
            self.record(&req, "guarded_in_sequence");
            return Err(self.blocked(
                &req,
                "guarded_in_sequence",
                "a tap list contains a guarded target; tap it on its own",
            ));
        }
        // 6. deny: refuse.
        if mode == GuardMode::Deny {
            self.record(&req, "guard_denied");
            return Err(self.blocked(&req, "guard_denied", "the guard refuses this action ([guard] mode = \"deny\")"));
        }
        // 7. human: ask, unless the last answer was a no just now.
        let cooldown = self.guard_rules.deny_cooldown();
        if self.guard_state.refused_at.is_some_and(|at| at.elapsed() < cooldown) {
            self.record(&req, "confirm_cooldown");
            return Err(self.blocked(
                &req,
                "confirm_cooldown",
                format!("the user refused a guarded action less than {} s ago", cooldown.as_secs()),
            ));
        }
        let timeout = self.guard_rules.confirm_timeout();
        let prompt = build_prompt(&Ask {
            action: req.action,
            concerns: &req.concerns,
            target: req.target.as_deref(),
            app: req.app_name.as_deref(),
            typed: req.typed,
            secret: req.secret,
            timeout,
        });
        let answer = self.confirmer.clone().confirm(&prompt, timeout).await;
        match answer {
            Answer::Approved => {
                self.record(&req, "approved");
                Ok(true)
            }
            Answer::Denied => {
                self.guard_state.refused_at = Some(Instant::now());
                self.record(&req, "confirm_denied");
                Err(self.blocked(&req, "confirm_denied", "the user refused this action"))
            }
            Answer::TimedOut => {
                self.guard_state.refused_at = Some(Instant::now());
                self.record(&req, "confirm_timeout");
                Err(self.blocked(
                    &req,
                    "confirm_timeout",
                    format!("nobody answered the confirmation within {} s", timeout.as_secs()),
                ))
            }
            Answer::Unavailable(why) => {
                self.record(&req, "confirm_unavailable");
                Err(self.blocked(&req, "confirm_unavailable", format!("no confirmation could be asked: {why}")))
            }
        }
    }

    /// Concerns about pressing `el`: its words, a generic confirm in its
    /// context, and the button around it when it is text or an image.
    fn element_concerns(&self, screen: &Screen, el: &Element, out: &mut Vec<Concern>) {
        out.extend(self.guard_rules.press_concerns(el));
        out.extend(self.guard_rules.context_concern(screen, el));
        if matches!(el.role.as_str(), "text" | "image") {
            let mut parent = el.parent.as_deref();
            for _ in 0..ANCESTORS {
                let Some(p) = parent.and_then(|id| screen.by_id(id)) else { break };
                if matches!(p.role.as_str(), "button" | "link") {
                    out.extend(self.guard_rules.press_concerns(p));
                    out.extend(self.guard_rules.context_concern(screen, p));
                    break;
                }
                parent = p.parent.as_deref();
            }
        }
    }

    /// Taps (and tap lists) on located targets. `Some` when the person was
    /// asked and approved: check it with `Approval::still_valid` after
    /// locating again.
    pub async fn guard_targets(
        &mut self,
        screen: &Screen,
        points: &[(f64, f64, Option<Element>)],
        action: Action,
    ) -> Res<Option<Approval>> {
        if self.guard_suspended {
            return Ok(None);
        }
        let mut concerns = Vec::new();
        for (x, y, el) in points {
            if let Some(el) = el {
                self.element_concerns(screen, el, &mut concerns);
            }
            // What the finger lands on may be a child of the target.
            if let Some(hit) = screen.element_at(*x, *y)
                && el.as_ref().is_none_or(|e| e.id != hit.id)
            {
                self.element_concerns(screen, hit, &mut concerns);
            }
        }
        let first = points.iter().find_map(|(x, y, el)| el.clone().or_else(|| screen.element_at(*x, *y).cloned()));
        let pid = first.as_ref().map(|e| e.pid).or_else(|| screen.apps.first().map(|a| a.pid));
        let bundle = self.bundle_of(pid).await;
        if let Some(c) = self.guard_rules.app_concern(bundle.as_deref(), action) {
            concerns.push(c);
        }
        let mut req = Request::new(action, concerns, bundle);
        req.target = first.as_ref().and_then(name_of);
        req.app_name = app_name(screen, pid);
        req.element = first.as_ref().map(brief);
        if !self.decide(req).await? {
            return Ok(None);
        }
        Ok(Some(Approval {
            asked: true,
            prints: points.iter().map(|(_, _, el)| el.as_ref().map(Print::of)).collect(),
        }))
    }

    /// A swipe starting at `from`: guarded when it starts on a control.
    pub async fn guard_swipe(&mut self, screen: &Screen, from: (f64, f64)) -> Res<()> {
        if self.guard_suspended {
            return Ok(());
        }
        let start = screen.element_at(from.0, from.1).cloned();
        let mut concerns = Vec::new();
        if let Some(el) = &start
            && matches!(el.role.as_str(), "button" | "link" | "adjustable" | "switch" | "other")
        {
            concerns.extend(self.guard_rules.press_concerns(el));
        }
        let pid = start.as_ref().map(|e| e.pid).or_else(|| screen.apps.first().map(|a| a.pid));
        let bundle = self.bundle_of(pid).await;
        if let Some(c) = self.guard_rules.app_concern(bundle.as_deref(), Action::Swipe) {
            concerns.push(c);
        }
        let mut req = Request::new(Action::Swipe, concerns, bundle);
        req.target = start.as_ref().and_then(name_of);
        req.app_name = app_name(screen, pid);
        self.decide(req).await.map(|_| ())
    }

    /// A key press on `screen`.
    pub async fn guard_key(&mut self, screen: &Screen, usage: u64, modifiers: &[u64]) -> Res<()> {
        if self.guard_suspended {
            return Ok(());
        }
        let mut concerns = self.guard_rules.key_concerns(usage, modifiers, screen);
        let focused = screen.elements.iter().find(|e| e.focused);
        let pid = focused.map(|e| e.pid).or_else(|| screen.apps.first().map(|a| a.pid));
        let target = focused.and_then(name_of);
        let bundle = self.bundle_of(pid).await;
        if let Some(c) = self.guard_rules.app_concern(bundle.as_deref(), Action::Key) {
            concerns.push(c);
        }
        let mut req = Request::new(Action::Key, concerns, bundle);
        req.target = target;
        req.app_name = app_name(screen, pid);
        self.decide(req).await.map(|_| ())
    }

    /// Typing `text` into `field`.
    pub async fn guard_type(&mut self, screen: &Screen, field: &Element, text: &str, submit: bool) -> Res<()> {
        if self.guard_suspended {
            return Ok(());
        }
        let mut concerns = self.guard_rules.type_concerns(field, submit);
        let bundle = self.bundle_of(Some(field.pid)).await;
        if let Some(c) = self.guard_rules.app_concern(bundle.as_deref(), Action::Type) {
            concerns.push(c);
        }
        let mut req = Request::new(Action::Type, concerns, bundle);
        req.target = name_of(field);
        req.app_name = app_name(screen, Some(field.pid));
        req.typed = Some(text);
        req.secret = field.role == "secure_text_field";
        self.decide(req).await.map(|_| ())
    }

    /// Launching `bundle`.
    pub async fn guard_launch(&mut self, bundle: &str) -> Res<()> {
        if self.guard_suspended {
            return Ok(());
        }
        let concerns: Vec<Concern> =
            self.guard_rules.app_concern(Some(bundle), Action::Launch).into_iter().collect();
        let mut req = Request::new(Action::Launch, concerns, Some(bundle.to_string()));
        req.target = Some(bundle.to_string());
        self.decide(req).await.map(|_| ())
    }

    /// Input sent without a reading of the screen (`tap_unread`, a swipe on
    /// an unread screen): what it presses is unknown.
    pub async fn guard_blind(&mut self, action: Action, at: Option<(f64, f64)>) -> Res<()> {
        if self.guard_suspended {
            return Ok(());
        }
        let mut req = Request::new(action, vec![self.guard_rules.blind_concern()], None);
        req.target = at.map(|(x, y)| format!("{x:.0},{y:.0}"));
        self.decide(req).await.map(|_| ())
    }

    /// Scrolls and list reading: only the app lists, which need the app in
    /// front (read only when a list is set).
    pub async fn guard_app_input(&mut self) -> Res<()> {
        if self.guard_suspended || !self.guard_rules.has_app_lists() {
            return Ok(());
        }
        let pid = match self.screen(Fresh::MaxAge(reuse()), false, BuildOptions::default()).await {
            Ok((_, screen)) => screen.apps.first().map(|a| a.pid),
            Err(_) => None,
        };
        let bundle = self.bundle_of(pid).await;
        self.decide(Request::new(Action::Scroll, Vec::new(), bundle)).await.map(|_| ())
    }

    /// A hardware button. Buttons are exempt from the app lists (they are
    /// how to leave an app); the lock button is limited to two presses in
    /// ten seconds.
    pub fn guard_button(&mut self, page: u64, usage: u64) -> Res<()> {
        if self.guard_suspended || self.guard_rules.mode() == GuardMode::Off {
            return Ok(());
        }
        if (page, usage) != (LOCK_PAGE, LOCK_USAGE) {
            return Ok(());
        }
        self.guard_state.lock_presses.retain(|at| at.elapsed() < LOCK_WINDOW);
        if self.guard_state.lock_presses.len() < LOCK_PRESSES {
            return Ok(());
        }
        let req = Request::new(
            Action::Button,
            vec![Concern { category: crate::config::GuardCategory::Shortcut, rule: "lock_rate".into(), why: String::new() }],
            None,
        );
        self.record(&req, "button_rate_limited");
        Err(self.blocked(
            &req,
            "button_rate_limited",
            "the lock button was pressed twice in the last 10 s; fast presses start Emergency SOS",
        ))
    }

    /// Count a lock button press that went out.
    pub fn guard_button_sent(&mut self, page: u64, usage: u64) {
        if (page, usage) == (LOCK_PAGE, LOCK_USAGE) {
            self.guard_state.lock_presses.retain(|at| at.elapsed() < LOCK_WINDOW);
            self.guard_state.lock_presses.push_back(Instant::now());
        }
    }
}
