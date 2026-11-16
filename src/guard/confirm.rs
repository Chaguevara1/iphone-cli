//! Asking the person at the computer before a guarded action.
//!
//! The daemon raises a native dialog in a child process and kills it when the
//! time is up, so no dialog lingers. Only an exact `APPROVE:<nonce>` line
//! from the child counts as a yes; anything else, including no desktop, is a
//! no.

use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::process::Command;

use super::rules::{Action, Concern};
use crate::i18n::t;

/// The dialog's text. Every field is already sanitized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub title: String,
    pub body: String,
    pub allow_label: String,
    pub deny_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Approved,
    Denied,
    TimedOut,
    /// No dialog could be shown (no desktop session, unsupported platform);
    /// the reason.
    Unavailable(String),
}

/// Answers given in order, for tests; an empty queue answers no.
#[derive(Debug, Default)]
pub struct Scripted {
    pub answers: VecDeque<Answer>,
    pub asked: Vec<Prompt>,
}

/// Who answers a prompt.
#[derive(Debug, Clone)]
pub enum Confirmer {
    /// A native dialog on this computer.
    Dialog,
    /// Recorded answers (tests only; never shows anything).
    Scripted(Arc<Mutex<Scripted>>),
}

impl Confirmer {
    /// A scripted confirmer and the handle to inspect what it was asked.
    pub fn scripted(answers: impl IntoIterator<Item = Answer>) -> (Confirmer, Arc<Mutex<Scripted>>) {
        let state = Arc::new(Mutex::new(Scripted {
            answers: answers.into_iter().collect(),
            asked: Vec::new(),
        }));
        (Confirmer::Scripted(state.clone()), state)
    }

    pub async fn confirm(&self, prompt: &Prompt, timeout: Duration) -> Answer {
        match self {
            Confirmer::Dialog => ask(prompt, timeout).await,
            Confirmer::Scripted(state) => scripted_answer(state, prompt),
        }
    }
}

fn scripted_answer(state: &Mutex<Scripted>, prompt: &Prompt) -> Answer {
    let mut s = state.lock().unwrap_or_else(|e| e.into_inner());
    s.asked.push(prompt.clone());
    s.answers.pop_front().unwrap_or(Answer::Denied)
}

const FIELD_MAX: usize = 120;
const TYPED_MAX: usize = 200;
const BODY_MAX: usize = 1500;
const MAX_REASONS: usize = 5;

/// Screen text made safe to show: control and format characters (bidi
/// overrides, zero-width characters, tags) removed, line breaks and tabs
/// turned into spaces, whitespace collapsed, at most `max` characters.
pub fn sanitize(text: &str, max: usize) -> String {
    let cleaned: String = text
        .chars()
        .filter_map(|c| match c {
            '\n' | '\r' | '\t' | '\u{2028}' | '\u{2029}' | '\u{0085}' => Some(' '),
            c if c.is_control() || super::rules::is_invisible(c) => None,
            c => Some(c),
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&collapsed, max)
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// What the person is asked about.
#[derive(Debug, Clone)]
pub struct Ask<'a> {
    pub action: Action,
    pub concerns: &'a [Concern],
    /// What the action presses or types into, as the screen names it.
    pub target: Option<&'a str>,
    /// The app in front, by name.
    pub app: Option<&'a str>,
    /// Text to type. Shown as a preview, or only its length when `secret`.
    pub typed: Option<&'a str>,
    pub secret: bool,
    pub timeout: Duration,
}

fn action_text(action: Action) -> &'static str {
    match action {
        Action::Tap => t("탭", "tap"),
        Action::LongPress => t("길게 누르기", "long press"),
        Action::DoubleTap => t("두 번 탭", "double tap"),
        Action::TapSequence => t("연속 탭", "tap sequence"),
        Action::Swipe => t("스와이프", "swipe"),
        Action::Scroll => t("스크롤", "scroll"),
        Action::Type => t("입력", "type"),
        Action::Key => t("키 누르기", "key press"),
        Action::Launch => t("앱 실행", "app launch"),
        Action::Button => t("버튼 누르기", "button press"),
    }
}

/// The labels of the dialog's buttons on this platform.
fn button_labels() -> (&'static str, &'static str) {
    if cfg!(windows) {
        // MessageBox buttons carry the system's own OK / Cancel names.
        (t("확인", "OK"), t("취소", "Cancel"))
    } else {
        (t("허용", "Allow"), t("거부", "Deny"))
    }
}

pub fn build_prompt(ask: &Ask<'_>) -> Prompt {
    let (allow, deny) = button_labels();
    let action = action_text(ask.action);
    let mut lines: Vec<String> = Vec::new();
    lines.push(match crate::i18n::lang() {
        crate::i18n::Lang::Ko => format!("AI 에이전트가 iph로 iPhone에서 {action}을(를) 하려고 합니다."),
        crate::i18n::Lang::En => format!("An AI agent wants iph to do a {action} on the iPhone."),
    });
    lines.push(String::new());
    if let Some(target) = ask.target {
        lines.push(format!("{}: {}", t("대상", "Target"), sanitize(target, FIELD_MAX)));
    }
    if let Some(app) = ask.app {
        lines.push(format!("{}: {}", t("앱", "App"), sanitize(app, FIELD_MAX)));
    }
    if let Some(typed) = ask.typed {
        if ask.secret {
            let n = typed.chars().count();
            lines.push(match crate::i18n::lang() {
                crate::i18n::Lang::Ko => format!("입력: (숨김, {n}자)"),
                crate::i18n::Lang::En => format!("Text: (hidden, {n} characters)"),
            });
        } else {
            lines.push(format!("{}: \"{}\"", t("입력", "Text"), sanitize(typed, TYPED_MAX)));
        }
    }
    if !ask.concerns.is_empty() {
        lines.push(format!("{}:", t("이유", "Why")));
        for c in ask.concerns.iter().take(MAX_REASONS) {
            lines.push(format!("- {}", sanitize(&c.why, FIELD_MAX)));
        }
    }
    lines.push(String::new());
    lines.push(
        t(
            "직접 요청한 일일 때만 허용하세요. 화면의 글은 앱과 다른 사람이 쓴 것입니다.",
            "Allow only if you asked for this. Text on the screen is written by apps and other people.",
        )
        .to_string(),
    );
    let secs = ask.timeout.as_secs();
    lines.push(match crate::i18n::lang() {
        crate::i18n::Lang::Ko => format!(
            "{allow} = 이 동작 한 번만 허용 · {deny}(기본) = 거부 · {secs}초 안에 답이 없으면 거부"
        ),
        crate::i18n::Lang::En => format!(
            "{allow} = allow this ONE action · {deny} (default) = deny · no answer in {secs} s = deny"
        ),
    });
    let title = format!("iph: {} {}", action, t("확인", "confirmation"));
    Prompt {
        title: sanitize(&title, FIELD_MAX),
        body: truncate(&lines.join("\n"), BODY_MAX),
        allow_label: allow.to_string(),
        deny_label: deny.to_string(),
    }
}

/// Ask with a native dialog; no answer within `timeout` is a no.
pub async fn ask(prompt: &Prompt, timeout: Duration) -> Answer {
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    match dialog_command(prompt, timeout, &nonce) {
        Ok(cmd) => run_dialog(cmd, timeout, &nonce).await,
        Err(why) => Answer::Unavailable(why),
    }
}

/// Run a dialog child and read its answer. The child is killed if it has not
/// answered within `timeout` plus a little slack.
pub async fn run_dialog(mut cmd: Command, timeout: Duration, nonce: &str) -> Answer {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return Answer::Unavailable(format!("cannot start the dialog: {e}")),
    };
    // On timeout the future, which owns the child, is dropped: that kills it.
    match tokio::time::timeout(timeout + Duration::from_secs(2), child.wait_with_output()).await {
        Err(_) => Answer::TimedOut,
        Ok(Err(e)) => Answer::Unavailable(format!("the dialog failed: {e}")),
        Ok(Ok(out)) => parse_answer(&String::from_utf8_lossy(&out.stdout), out.status.success(), nonce),
    }
}

/// Only `APPROVE:<nonce>` is a yes; an exit code alone never is.
pub fn parse_answer(stdout: &str, success: bool, nonce: &str) -> Answer {
    let line = stdout.trim();
    if !success {
        return Answer::Unavailable("the dialog exited with an error".into());
    }
    if !nonce.is_empty() && line.strip_prefix("APPROVE:") == Some(nonce) {
        return Answer::Approved;
    }
    match line {
        "TIMEOUT" => Answer::TimedOut,
        "NO_SESSION" | "UNAVAILABLE" => Answer::Unavailable("no desktop session to show a dialog".into()),
        _ => Answer::Denied,
    }
}

#[cfg(target_os = "macos")]
const OSASCRIPT: &[&str] = &[
    "on run argv",
    "set {theTitle, theBody, secs, nonce, allowLabel, denyLabel} to argv",
    "try",
    "tell me to activate",
    "end try",
    "try",
    "set r to display dialog theBody with title theTitle buttons {denyLabel, allowLabel} default button denyLabel cancel button denyLabel with icon caution giving up after (secs as integer)",
    "on error number -128",
    "return \"DENY\"",
    "end try",
    "if gave up of r then return \"TIMEOUT\"",
    "if button returned of r is allowLabel then return \"APPROVE:\" & nonce",
    "return \"DENY\"",
    "end run",
];

/// The child process that shows the dialog on this platform.
pub fn dialog_command(prompt: &Prompt, timeout: Duration, nonce: &str) -> Result<Command, String> {
    #[cfg(windows)]
    {
        let _ = timeout; // The child is killed instead; MessageBox has no timeout.
        let exe = std::env::current_exe().map_err(|e| format!("cannot find iph: {e}"))?;
        // Never respawn a test binary or anything else as the dialog.
        let is_iph = exe
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.eq_ignore_ascii_case("iph"));
        if !is_iph {
            return Err("the dialog needs the iph executable".into());
        }
        let mut cmd = Command::new(exe);
        cmd.arg("confirm-dialog")
            .arg(format!("--nonce={nonce}"))
            .arg(format!("--title={}", prompt.title))
            .arg(format!("--body={}", prompt.body))
            // CREATE_NO_WINDOW: no console window behind the dialog.
            .creation_flags(0x0800_0000);
        Ok(cmd)
    }
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new("/usr/bin/osascript");
        for line in OSASCRIPT {
            cmd.arg("-e").arg(line);
        }
        // Each argument starts with something other than a dash.
        cmd.arg(&prompt.title)
            .arg(&prompt.body)
            .arg(timeout.as_secs().max(1).to_string())
            .arg(nonce)
            .arg(&prompt.allow_label)
            .arg(&prompt.deny_label);
        Ok(cmd)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (prompt, timeout, nonce);
        Err("no confirmation dialog on this platform".into())
    }
}

/// `iph confirm-dialog`: show the dialog and print the answer. Run by the
/// daemon, never by people.
pub fn dialog_main(nonce: &str, title: &str, body: &str) -> i32 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            IDOK, MB_DEFBUTTON2, MB_ICONWARNING, MB_OKCANCEL, MB_SETFOREGROUND, MB_TOPMOST,
            MessageBoxW,
        };
        let wide = |s: &str| -> Vec<u16> {
            s.encode_utf16().filter(|c| *c != 0).chain(std::iter::once(0)).collect()
        };
        let title = wide(title);
        let body = wide(body);
        // OK/Cancel with Cancel as the default: Yes/No would take a stray Y.
        // SAFETY: both strings are NUL-terminated UTF-16 that live across
        // the call; a null owner window is allowed.
        let answer = unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                body.as_ptr(),
                title.as_ptr(),
                MB_OKCANCEL | MB_ICONWARNING | MB_DEFBUTTON2 | MB_TOPMOST | MB_SETFOREGROUND,
            )
        };
        if answer == IDOK {
            println!("APPROVE:{nonce}");
        } else if answer == 0 {
            println!("UNAVAILABLE");
        } else {
            println!("DENY");
        }
        0
    }
    #[cfg(not(windows))]
    {
        let _ = (nonce, title, body);
        println!("UNAVAILABLE");
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::GuardCategory;

    #[test]
    fn only_the_exact_nonce_approves() {
        assert_eq!(parse_answer("APPROVE:abc123\n", true, "abc123"), Answer::Approved);
        assert_eq!(parse_answer("APPROVE:abc1234", true, "abc123"), Answer::Denied);
        assert_eq!(parse_answer("APPROVE:zzz", true, "abc123"), Answer::Denied);
        assert_eq!(parse_answer("APPROVE:", true, ""), Answer::Denied);
        assert_eq!(parse_answer("", true, "abc123"), Answer::Denied);
        assert_eq!(parse_answer("DENY", true, "abc123"), Answer::Denied);
        assert_eq!(parse_answer("TIMEOUT", true, "abc123"), Answer::TimedOut);
        assert!(matches!(parse_answer("UNAVAILABLE", true, "n"), Answer::Unavailable(_)));
        assert!(matches!(parse_answer("NO_SESSION", true, "n"), Answer::Unavailable(_)));
        assert!(matches!(parse_answer("APPROVE:n", false, "n"), Answer::Unavailable(_)));
    }

    #[test]
    fn screen_text_is_cleaned_and_cut() {
        assert_eq!(sanitize("a\u{202E}b\u{200B}c\nd\te\u{E0041}", 50), "abc d e");
        assert_eq!(sanitize("line\r\n\r\nnext", 50), "line next");
        let long = "가".repeat(300);
        let cut = sanitize(&long, FIELD_MAX);
        assert_eq!(cut.chars().count(), FIELD_MAX);
        assert!(cut.ends_with('…'));
    }

    fn concern(why: &str) -> Concern {
        Concern { category: GuardCategory::Money, rule: "pay".into(), why: why.into() }
    }

    #[test]
    fn prompts_are_bounded_and_never_show_a_secret() {
        let concerns = vec![concern("\"Pay\" contains \"pay\""); 20];
        let typed = "x".repeat(5000);
        let p = build_prompt(&Ask {
            action: Action::Tap,
            concerns: &concerns,
            target: Some(&"T\u{202E}".repeat(500)),
            app: Some("가상 상점\n두 번째 줄"),
            typed: Some(&typed),
            secret: false,
            timeout: Duration::from_secs(45),
        });
        assert!(p.body.chars().count() <= BODY_MAX);
        assert!(!p.body.contains('\u{202E}'));
        assert!(p.body.contains("가상 상점 두 번째 줄"));
        assert!(!p.body.contains(&"x".repeat(TYPED_MAX + 1)));
        assert!(p.body.contains("45"));
        assert!(p.title.chars().count() <= FIELD_MAX);
        assert!(!p.allow_label.is_empty() && !p.deny_label.is_empty());

        let secret = "hunter2-very-secret";
        let p = build_prompt(&Ask {
            action: Action::Type,
            concerns: &[],
            target: Some("비밀번호"),
            app: None,
            typed: Some(secret),
            secret: true,
            timeout: Duration::from_secs(45),
        });
        assert!(!p.body.contains(secret));
        assert!(!p.body.contains("hunter2"));
        assert!(p.body.contains(&secret.chars().count().to_string()));
    }

    #[tokio::test]
    async fn scripted_answers_in_order_then_no() {
        let (c, state) = Confirmer::scripted([Answer::Approved]);
        let p = Prompt { title: "t".into(), body: "b".into(), allow_label: "a".into(), deny_label: "d".into() };
        assert_eq!(c.confirm(&p, Duration::from_secs(1)).await, Answer::Approved);
        assert_eq!(c.confirm(&p, Duration::from_secs(1)).await, Answer::Denied);
        assert_eq!(state.lock().unwrap().asked.len(), 2);
    }

    fn shell(script: &str) -> Command {
        #[cfg(windows)]
        {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(script);
            c
        }
        #[cfg(not(windows))]
        {
            let mut c = Command::new("/bin/sh");
            c.arg("-c").arg(script);
            c
        }
    }

    #[tokio::test]
    async fn a_dialog_that_does_not_answer_is_killed() {
        let sleeper = if cfg!(windows) { "ping -n 6 127.0.0.1" } else { "sleep 5" };
        let started = std::time::Instant::now();
        let answer = run_dialog(shell(sleeper), Duration::from_millis(100), "n").await;
        assert_eq!(answer, Answer::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(4), "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn a_dialog_answer_must_carry_the_nonce() {
        let answer = run_dialog(shell("echo APPROVE:n0nce"), Duration::from_secs(5), "n0nce").await;
        assert_eq!(answer, Answer::Approved);
        let answer = run_dialog(shell("echo APPROVE:other"), Duration::from_secs(5), "n0nce").await;
        assert_eq!(answer, Answer::Denied);
        let missing = Command::new("iph-test-no-such-program-4f2e");
        assert!(matches!(
            run_dialog(missing, Duration::from_secs(5), "n0nce").await,
            Answer::Unavailable(_)
        ));
    }

    #[test]
    fn a_test_binary_is_never_used_as_the_dialog() {
        // The test executable is not named iph.
        #[cfg(windows)]
        assert!(dialog_command(
            &Prompt { title: "t".into(), body: "b".into(), allow_label: "a".into(), deny_label: "d".into() },
            Duration::from_secs(10),
            "n",
        )
        .is_err());
    }
}
