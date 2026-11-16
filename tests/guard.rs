//! The guard on made-up screens. No phone, no dialog: the session uses a
//! UDID that does not exist and a scripted confirmer, never the real dialog.

#![allow(clippy::result_large_err)]

use std::sync::{Arc, Mutex, Once};

use iphone_cli::{
    actions::resolve_one,
    ax::tree::{Element, Screen},
    config::{Config, GuardConfig, GuardMode},
    device::session::Session,
    guard::{Action, Answer, Confirmer, Scripted},
    reply::Outcome,
    selector::Selector,
};

const NO_SUCH_PHONE: &str = "00000000-TESTONLY-NOPHONE";

fn isolate() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("iph-guard-{}", uuid::Uuid::new_v4()));
        // The guard's event log of these tests goes to a throwaway folder.
        unsafe {
            std::env::set_var("IPH_STATE_DIR", &dir);
            std::env::set_var("IPH_CONFIG_DIR", dir.join("config"));
        }
    });
}

fn checkout() -> Screen {
    let mut screen: Screen = serde_json::from_str(include_str!("fixtures/checkout-screen.json")).unwrap();
    for e in &mut screen.elements {
        e.pid = 4242;
    }
    screen
}

fn chat() -> Screen {
    serde_json::from_str(include_str!("fixtures/guard-chat.json")).unwrap()
}

fn alert() -> Screen {
    serde_json::from_str(include_str!("fixtures/guard-alert.json")).unwrap()
}

/// A session with `guard` and a confirmer that gives `answers` in order.
fn session_with(guard: GuardConfig, answers: Vec<Answer>) -> (Session, Arc<Mutex<Scripted>>) {
    isolate();
    let config = Config { guard, ..Default::default() };
    let mut s = Session::new(NO_SUCH_PHONE.into(), config);
    let (confirmer, asked) = Confirmer::scripted(answers);
    s.confirmer = confirmer;
    (s, asked)
}

fn session(answers: Vec<Answer>) -> (Session, Arc<Mutex<Scripted>>) {
    session_with(GuardConfig::default(), answers)
}

fn asked(state: &Arc<Mutex<Scripted>>) -> usize {
    state.lock().unwrap().asked.len()
}

/// Where a tap on `selector` goes: the element's center, and the element.
fn at(screen: &Screen, selector: &str) -> Vec<(f64, f64, Option<Element>)> {
    let el = resolve_one(&Selector::parse(selector).unwrap(), screen).unwrap();
    let (x, y) = el.frame.unwrap().center();
    vec![(x, y, Some(el.clone()))]
}

#[tokio::test]
async fn a_pay_button_asks_and_a_no_blocks_it() {
    let (mut s, state) = session(vec![Answer::Denied]);
    let screen = checkout();
    let points = at(&screen, "label~=결제하기");
    let fail = s.guard_targets(&screen, &points, Action::Tap).await.unwrap_err();
    assert_eq!(fail.outcome, Outcome::Blocked);
    assert_eq!(fail.code, "confirm_denied");
    assert!(fail.hint.is_some());
    assert_eq!(fail.extra["element"]["label"], "3,000원 결제하기");
    assert_eq!(fail.extra["guard"]["action"], "tap");
    assert_eq!(fail.extra["guard"]["mode"], "human");
    assert!(fail.extra["guard"]["categories"].as_array().unwrap().iter().any(|c| c == "money"));
    let state = state.lock().unwrap();
    assert_eq!(state.asked.len(), 1);
    assert!(state.asked[0].body.contains("3,000원 결제하기"), "{}", state.asked[0].body);
}

#[tokio::test]
async fn an_approval_holds_only_for_the_same_target() {
    let (mut s, state) = session(vec![Answer::Approved]);
    let screen = checkout();
    let points = at(&screen, "label~=\"Pay ₩\"");
    let approval = s.guard_targets(&screen, &points, Action::Tap).await.unwrap().expect("approved");
    assert!(approval.asked);
    assert_eq!(asked(&state), 1);
    approval.still_valid(&screen, &points).unwrap();

    // Another label in the same place.
    let mut relabelled = points.clone();
    relabelled[0].2.as_mut().unwrap().label = Some("Pay ₩300,000".into());
    assert_eq!(approval.still_valid(&screen, &relabelled).unwrap_err().code, "target_changed");
    // The same button, moved.
    let mut moved = points.clone();
    moved[0].2.as_mut().unwrap().frame.as_mut().unwrap().y += 20.0;
    let fail = approval.still_valid(&screen, &moved).unwrap_err();
    assert_eq!((fail.code.as_str(), fail.outcome), ("target_changed", Outcome::NotSent));
    // Gone.
    assert!(approval.still_valid(&screen, &[]).is_err());
}

#[tokio::test]
async fn after_a_no_the_next_guarded_action_is_refused_without_asking() {
    let (mut s, state) = session(vec![Answer::Denied, Answer::Approved]);
    let screen = checkout();
    let points = at(&screen, "label~=결제하기");
    assert_eq!(s.guard_targets(&screen, &points, Action::Tap).await.unwrap_err().code, "confirm_denied");
    let again = s.guard_targets(&screen, &points, Action::Tap).await.unwrap_err();
    assert_eq!((again.code.as_str(), again.outcome), ("confirm_cooldown", Outcome::Blocked));
    assert_eq!(asked(&state), 1);
    // Ordinary taps are not held up by the cooldown.
    assert!(s.guard_targets(&screen, &at(&screen, "취소"), Action::Tap).await.unwrap().is_none());
}

#[tokio::test]
async fn no_answer_and_no_dialog_both_block() {
    let screen = checkout();
    let points = at(&screen, "label~=결제하기");
    let (mut s, _) = session(vec![Answer::TimedOut]);
    assert_eq!(s.guard_targets(&screen, &points, Action::Tap).await.unwrap_err().code, "confirm_timeout");
    // A timeout counts as a no for the cooldown.
    assert_eq!(s.guard_targets(&screen, &points, Action::Tap).await.unwrap_err().code, "confirm_cooldown");

    let (mut s, _) = session(vec![Answer::Unavailable("no desktop".into())]);
    assert_eq!(s.guard_targets(&screen, &points, Action::Tap).await.unwrap_err().code, "confirm_unavailable");
}

#[tokio::test]
async fn deny_refuses_and_off_lets_through() {
    let screen = checkout();
    let points = at(&screen, "label~=결제하기");
    let (mut s, state) = session_with(GuardConfig { mode: GuardMode::Deny, ..Default::default() }, vec![Answer::Approved]);
    let fail = s.guard_targets(&screen, &points, Action::Tap).await.unwrap_err();
    assert_eq!((fail.code.as_str(), fail.outcome), ("guard_denied", Outcome::Blocked));
    assert_eq!(asked(&state), 0);

    let (mut s, state) = session_with(GuardConfig { mode: GuardMode::Off, ..Default::default() }, vec![]);
    assert!(s.guard_targets(&screen, &points, Action::Tap).await.unwrap().is_none());
    assert!(s.guard_targets(&screen, &points, Action::TapSequence).await.unwrap().is_none());
    assert_eq!(asked(&state), 0);
}

#[tokio::test]
async fn ordinary_buttons_pass_without_asking() {
    let (mut s, state) = session(vec![]);
    let screen = checkout();
    for selector in ["취소", "뒤로", "Payment methods"] {
        assert!(s.guard_targets(&screen, &at(&screen, selector), Action::Tap).await.unwrap().is_none(), "{selector}");
    }
    // The documented limit: an unlabelled icon is not caught.
    let icon = screen.by_id("icon0001").unwrap().clone();
    let (x, y) = icon.frame.unwrap().center();
    assert!(s.guard_targets(&screen, &[(x, y, Some(icon))], Action::Tap).await.unwrap().is_none());
    assert_eq!(asked(&state), 0);
}

#[tokio::test]
async fn a_tap_list_with_a_guarded_target_is_refused() {
    let (mut s, state) = session(vec![Answer::Approved]);
    let screen = checkout();
    let mut points = at(&screen, "취소");
    points.extend(at(&screen, "label~=결제하기"));
    let fail = s.guard_targets(&screen, &points, Action::TapSequence).await.unwrap_err();
    assert_eq!((fail.code.as_str(), fail.outcome), ("guarded_in_sequence", Outcome::Blocked));
    assert_eq!(asked(&state), 0);
}

#[tokio::test]
async fn a_swipe_that_starts_on_a_pay_button_asks() {
    let (mut s, state) = session(vec![Answer::Denied]);
    let screen = checkout();
    let pay = at(&screen, "label~=결제하기");
    let fail = s.guard_swipe(&screen, (pay[0].0, pay[0].1)).await.unwrap_err();
    assert_eq!(fail.code, "confirm_denied");
    assert_eq!(asked(&state), 1);
    // A swipe through text is not guarded (and is not held by the cooldown).
    let item = at(&screen, "label~=연필");
    s.guard_swipe(&screen, (item[0].0, item[0].1)).await.unwrap();
    assert_eq!(asked(&state), 1);
}

#[tokio::test]
async fn only_apps_and_block_apps_hold_in_every_mode() {
    let screen = checkout();
    let cancel = at(&screen, "취소");
    for mode in [GuardMode::Human, GuardMode::Deny, GuardMode::Off] {
        let cfg = GuardConfig { mode, only_apps: vec!["com.apple.Preferences".into()], ..Default::default() };
        let (mut s, state) = session_with(cfg, vec![]);
        s.remember_bundle(4242, Some("com.example.shop".into()));
        let fail = s.guard_targets(&screen, &cancel, Action::Tap).await.unwrap_err();
        assert_eq!((fail.code.as_str(), fail.outcome), ("app_not_allowed", Outcome::Blocked), "{mode:?}");
        assert_eq!(s.guard_launch("com.example.shop").await.unwrap_err().code, "app_not_allowed");
        s.guard_launch("com.apple.Preferences").await.unwrap();
        // An app that cannot be identified is refused too.
        s.remember_bundle(4242, None);
        assert_eq!(s.guard_targets(&screen, &cancel, Action::Tap).await.unwrap_err().code, "app_not_allowed");
        assert_eq!(asked(&state), 0);
    }

    let cfg = GuardConfig { only_apps: vec!["com.example.shop".into()], ..Default::default() };
    let (mut s, _) = session_with(cfg, vec![]);
    s.remember_bundle(4242, Some("com.example.shop".into()));
    assert!(s.guard_targets(&screen, &cancel, Action::Tap).await.unwrap().is_none());

    let cfg = GuardConfig { mode: GuardMode::Off, block_apps: vec!["com.example.shop".into()], ..Default::default() };
    let (mut s, _) = session_with(cfg, vec![]);
    s.remember_bundle(4242, Some("com.example.shop".into()));
    assert_eq!(s.guard_targets(&screen, &cancel, Action::Tap).await.unwrap_err().code, "app_blocked");
    assert_eq!(s.guard_launch("com.example.shop").await.unwrap_err().code, "app_blocked");
}

#[tokio::test]
async fn confirm_apps_ask_before_any_tap() {
    let cfg = GuardConfig { confirm_apps: vec!["com.example.shop".into()], ..Default::default() };
    let (mut s, state) = session_with(cfg, vec![Answer::Approved]);
    s.remember_bundle(4242, Some("com.example.shop".into()));
    let screen = checkout();
    assert!(s.guard_targets(&screen, &at(&screen, "취소"), Action::Tap).await.unwrap().is_some());
    assert_eq!(asked(&state), 1);
}

#[tokio::test]
async fn a_suspended_guard_does_not_judge() {
    let cfg = GuardConfig { only_apps: vec!["com.apple.Preferences".into()], ..Default::default() };
    let (mut s, state) = session_with(cfg, vec![]);
    s.guard_suspended = true;
    let screen = checkout();
    assert!(s.guard_targets(&screen, &at(&screen, "label~=결제하기"), Action::Tap).await.unwrap().is_none());
    s.guard_launch("com.example.shop").await.unwrap();
    assert_eq!(asked(&state), 0);
}

#[tokio::test]
async fn the_lock_button_is_limited_to_two_presses_in_ten_seconds() {
    let (mut s, _) = session(vec![]);
    for _ in 0..2 {
        s.guard_button(0x0C, 0x30).unwrap();
        s.guard_button_sent(0x0C, 0x30);
    }
    let fail = s.guard_button(0x0C, 0x30).unwrap_err();
    assert_eq!((fail.code.as_str(), fail.outcome), ("button_rate_limited", Outcome::Blocked));
    // Home is not limited.
    for _ in 0..5 {
        s.guard_button(0x0C, 0x40).unwrap();
        s.guard_button_sent(0x0C, 0x40);
    }
    let (mut off, _) = session_with(GuardConfig { mode: GuardMode::Off, ..Default::default() }, vec![]);
    for _ in 0..3 {
        off.guard_button(0x0C, 0x30).unwrap();
        off.guard_button_sent(0x0C, 0x30);
    }
}

#[tokio::test]
async fn return_in_a_chat_and_typing_with_submit_ask() {
    let screen = chat();
    let (mut s, state) = session(vec![Answer::Denied]);
    let fail = s.guard_key(&screen, 0x28, &[]).await.unwrap_err();
    assert_eq!(fail.code, "confirm_denied");
    assert!(fail.extra["guard"]["categories"].as_array().unwrap().iter().any(|c| c == "send"));
    assert_eq!(asked(&state), 1);

    let field = screen.by_id("field002").unwrap().clone();
    let (mut s, state) = session(vec![Answer::Approved]);
    // Typing alone is fine; with Return it asks, and the prompt shows the text.
    s.guard_type(&screen, &field, "네 보낼게", false).await.unwrap();
    assert_eq!(asked(&state), 0);
    s.guard_type(&screen, &field, "네 보낼게", true).await.unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.asked.len(), 1);
    assert!(state.asked[0].body.contains("네 보낼게"));
}

#[tokio::test]
async fn a_password_is_never_shown_in_the_prompt() {
    let mut screen = chat();
    let field = screen.elements.iter_mut().find(|e| e.id == "field002").unwrap();
    field.role = "secure_text_field".into();
    let field = field.clone();
    let (mut s, state) = session(vec![Answer::Approved]);
    s.guard_type(&screen, &field, "hunter2-secret", false).await.unwrap();
    let state = state.lock().unwrap();
    assert_eq!(state.asked.len(), 1);
    assert!(!state.asked[0].body.contains("hunter2"));
}

#[tokio::test]
async fn ok_on_a_transfer_alert_asks() {
    let screen = alert();
    let (mut s, state) = session(vec![Answer::Denied]);
    let fail = s.guard_targets(&screen, &at(&screen, "확인"), Action::Tap).await.unwrap_err();
    assert_eq!(fail.code, "confirm_denied");
    assert!(fail.extra["guard"]["categories"].as_array().unwrap().iter().any(|c| c == "money"));
    assert_eq!(asked(&state), 1);
    // Cancel on the same alert passes.
    let (mut s, _) = session(vec![]);
    assert!(s.guard_targets(&screen, &at(&screen, "취소"), Action::Tap).await.unwrap().is_none());
}

#[tokio::test]
async fn blind_input_asks() {
    let (mut s, state) = session(vec![Answer::Denied]);
    let fail = s.guard_blind(Action::Tap, Some((100.0, 200.0))).await.unwrap_err();
    assert_eq!(fail.code, "confirm_denied");
    assert_eq!(fail.extra["guard"]["categories"][0], "blind");
    assert_eq!(asked(&state), 1);
}

#[test]
fn requests_that_may_ask_get_more_time() {
    isolate();
    let s = Session::new(NO_SUCH_PHONE.into(), Config::default());
    assert_eq!(s.guard_extra_time("tap").as_secs(), 50);
    assert_eq!(s.guard_extra_time("script").as_secs(), 50);
    assert_eq!(s.guard_extra_time("read").as_secs(), 0);
    let off = Session::new(
        NO_SUCH_PHONE.into(),
        Config { guard: GuardConfig { mode: GuardMode::Off, ..Default::default() }, ..Default::default() },
    );
    assert_eq!(off.guard_extra_time("tap").as_secs(), 0);
}
