//! What the guard looks for. Pure functions over a screen reading; no I/O.
//!
//! Words are matched against an element's label and its identifier split
//! into words (`btnSendMessage` → "btn send message"), never its value (what
//! a field holds) or hint. This reads labels: a button with no label, or one
//! labelled with other words, is not caught.

use std::collections::HashSet;

use serde::Serialize;
use unicode_normalization::UnicodeNormalization;

use crate::{
    ax::tree::{Element, Screen},
    config::{GuardCategory, GuardConfig, GuardMode},
    i18n::{self, Lang},
    keys,
};

/// One reason an action needs the guard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Concern {
    pub category: GuardCategory,
    /// The list word or rule name that matched. Safe to log.
    pub rule: String,
    /// For the person deciding. May quote screen text: never log it.
    pub why: String,
}

/// The input an action sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Tap,
    LongPress,
    DoubleTap,
    TapSequence,
    Swipe,
    Scroll,
    Type,
    Key,
    Launch,
    Button,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Tap => "tap",
            Action::LongPress => "long_press",
            Action::DoubleTap => "double_tap",
            Action::TapSequence => "tap_sequence",
            Action::Swipe => "swipe",
            Action::Scroll => "scroll",
            Action::Type => "type",
            Action::Key => "key",
            Action::Launch => "launch",
            Action::Button => "button",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum How {
    /// Latin: a whole word. Other scripts: anywhere in the text.
    Anywhere,
    /// A Korean noun: matches unless another word is glued to it
    /// ("결제 내역", "송금받기"), except the verb endings 하/해 ("결제하기").
    NounEnd,
}

#[derive(Debug, Clone)]
struct Word {
    text: String,
    category: GuardCategory,
    how: How,
}

use GuardCategory as C;
use How::{Anywhere as A, NounEnd as N};

const DEFAULT_WORDS: &[(GuardCategory, How, &[&str])] = &[
    (
        C::Money,
        N,
        &[
            "결제", "송금", "이체", "주문", "구매", "충전", "출금", "환전", "매수", "매도", "구독",
            "후원", "기부", "대출", "납부",
        ],
    ),
    (
        C::Money,
        A,
        &[
            "구매확정", "구매 확정", "선물하기", "pay", "purchase", "buy", "checkout", "check out",
            "place order", "order now", "transfer", "send money", "donate", "subscribe", "top up",
            "withdraw", "sell", "trade",
        ],
    ),
    (
        C::Send,
        A,
        &[
            "보내기", "올리기", "send", "post", "publish", "share", "reply", "submit", "upload",
            "tweet",
        ],
    ),
    (C::Send, N, &["전송", "발송", "게시", "등록", "공유", "답장", "제출", "업로드"]),
    (C::Delete, N, &["삭제", "초기화", "탈퇴", "해지"]),
    (
        C::Delete,
        A,
        &[
            "지우기", "비우기", "구독 취소", "구독취소", "delete", "remove", "erase", "reset",
            "deactivate", "unsubscribe", "empty trash", "clear all", "close account",
        ],
    ),
    (C::Permission, N, &["허용", "신뢰", "설치", "수락"]),
    (C::Permission, A, &["allow", "trust", "install", "accept"]),
];

/// Phrases removed before matching: they contain a word but do nothing risky.
const DEFAULT_EXCEPT: &[&str] = &[
    "apple pay",
    "payment history",
    "payment methods",
    "payment method",
    "purchase history",
    "order history",
    "don't allow",
    "do not allow",
    "don't trust",
    "not now",
    "신뢰하지 않음",
    "허용하지 않음",
    "허용 안 함",
];

/// Send words that a Return key in a text field is not taken to press.
const WEAK_SEND: &[&str] = &["share", "공유", "upload", "업로드", "올리기"];

/// Labels of a button that only confirms what the screen around it asks.
const GENERIC_CONFIRM: &[&str] = &[
    "확인", "예", "네", "완료", "다음", "계속", "동의", "승인", "진행", "ok", "okay", "confirm",
    "continue", "next", "done", "yes", "agree", "proceed", "approve",
];

/// Words in the text around a generic confirm button that make it risky.
const CONTEXT_TERMS: &[(&str, GuardCategory)] = &[
    ("결제", C::Money),
    ("송금", C::Money),
    ("이체", C::Money),
    ("구매", C::Money),
    ("주문", C::Money),
    ("출금", C::Money),
    ("삭제", C::Delete),
    ("탈퇴", C::Delete),
    ("해지", C::Delete),
    ("전송", C::Send),
    ("보내", C::Send),
    ("허용", C::Permission),
    ("pay", C::Money),
    ("payment", C::Money),
    ("purchase", C::Money),
    ("transfer", C::Money),
    ("order", C::Money),
    ("delete", C::Delete),
    ("remove", C::Delete),
    ("send", C::Send),
    ("install", C::Permission),
];

/// Roles whose label names a field or a heading, not something a tap does.
const NO_WORD_ROLES: &[&str] = &[
    "text_field",
    "search_field",
    "secure_text_field",
    "header",
    "application",
];

const FIELD_ROLES: &[&str] = &["text_field", "search_field", "secure_text_field"];

/// Characters that change nothing visible: zero-width characters (ZWJ and
/// ZWNJ too), bidi controls, Unicode tags and the soft hyphen.
pub(crate) fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'
            | '\u{1160}'
            | '\u{17B4}'
            | '\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

/// The form labels and words are compared in: invisible characters removed,
/// NFKC (full-width letters become plain ones), lowercase, typographic
/// apostrophes made plain, runs of whitespace one space.
pub fn normalize(text: &str) -> String {
    let visible: String = text.chars().filter(|c| !is_invisible(*c)).collect();
    let folded: String = visible
        .as_str()
        .nfkc()
        .filter(|c| !is_invisible(*c))
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{02BC}' => '\'',
            c => c,
        })
        .collect::<String>()
        .to_lowercase();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `btnSendMessage`, `send_button`, `PAYButton` → words separated by spaces.
fn split_identifier(id: &str) -> String {
    let chars: Vec<char> = id.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            out.push(' ');
            continue;
        }
        if i > 0 {
            let prev = chars[i - 1];
            let next = chars.get(i + 1).copied();
            let camel = c.is_uppercase() && (prev.is_lowercase() || prev.is_ascii_digit());
            let acronym_end = c.is_uppercase()
                && prev.is_uppercase()
                && next.is_some_and(|n| n.is_lowercase());
            let digit_edge = c.is_ascii_digit() != prev.is_ascii_digit() && prev.is_alphanumeric();
            if camel || acronym_end || digit_edge {
                out.push(' ');
            }
        }
        out.push(c);
    }
    out
}

fn ascii_boundary(c: Option<char>) -> bool {
    c.is_none_or(|c| !c.is_ascii_alphanumeric())
}

/// Latin words must stand alone ("pay" is not in "display").
fn contains_ascii_word(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    let mut from = 0;
    while let Some(pos) = text[from..].find(word) {
        let start = from + pos;
        let end = start + word.len();
        if ascii_boundary(text[..start].chars().next_back()) && ascii_boundary(text[end..].chars().next()) {
            return true;
        }
        // The word starts with an ASCII byte, so the next byte starts a char.
        from = start + 1;
    }
    false
}

fn is_hangul_syllable(c: char) -> bool {
    ('\u{AC00}'..='\u{D7A3}').contains(&c)
}

/// A Korean noun followed (spaces skipped) by nothing, punctuation, a
/// number, or the verb endings 하/해, but not by another syllable.
fn contains_noun(text: &str, word: &str) -> bool {
    if word.is_empty() {
        return false;
    }
    text.match_indices(word).any(|(i, w)| {
        let next = text[i + w.len()..].chars().find(|c| *c != ' ');
        next.is_none_or(|c| !is_hangul_syllable(c) || c == '하' || c == '해')
    })
}

fn word_matches(text: &str, word: &Word) -> bool {
    if word.text.is_ascii() {
        contains_ascii_word(text, &word.text)
    } else if word.how == How::NounEnd {
        contains_noun(text, &word.text)
    } else {
        text.contains(word.text.as_str())
    }
}

/// `text` with every occurrence of `phrase` replaced by a space. Latin
/// phrases only where they stand alone.
fn remove_phrase(text: &str, phrase: &str) -> String {
    if phrase.is_empty() {
        return text.to_string();
    }
    if !phrase.is_ascii() {
        return text.replace(phrase, " ");
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut from = 0;
    while let Some(pos) = text[from..].find(phrase) {
        let start = from + pos;
        let end = start + phrase.len();
        if ascii_boundary(text[..start].chars().next_back()) && ascii_boundary(text[end..].chars().next()) {
            out.push_str(&text[copied..start]);
            out.push(' ');
            copied = end;
            from = end;
        } else {
            from = start + 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// Text in the person's language.
fn say(ko: String, en: String) -> String {
    match i18n::lang() {
        Lang::Ko => ko,
        Lang::En => en,
    }
}

/// How an element is named in a reason: its label, else its identifier,
/// else its role.
fn name_of(e: &Element) -> &str {
    e.label
        .as_deref()
        .or(e.identifier.as_deref())
        .unwrap_or(e.role.as_str())
}

fn is_command_modifier(m: u64) -> bool {
    matches!(m, keys::MOD_COMMAND | keys::MOD_CONTROL | keys::MOD_OPTION)
}

/// The guard's rules, built once from `[guard]`.
#[derive(Debug, Clone)]
pub struct Rules {
    mode: GuardMode,
    categories: HashSet<GuardCategory>,
    /// Longest first, so a report names the most specific word.
    words: Vec<Word>,
    /// Longest first.
    except: Vec<String>,
    only_apps: Vec<String>,
    confirm_apps: Vec<String>,
    block_apps: Vec<String>,
    confirm_timeout: std::time::Duration,
    deny_cooldown: std::time::Duration,
}

impl Rules {
    pub fn new(cfg: &GuardConfig) -> Self {
        let removed: HashSet<String> = cfg.remove.iter().map(|w| normalize(w)).collect();
        let mut words: Vec<Word> = DEFAULT_WORDS
            .iter()
            .flat_map(|(category, how, list)| {
                list.iter().map(move |w| Word { text: normalize(w), category: *category, how: *how })
            })
            .filter(|w| !removed.contains(&w.text))
            .chain(cfg.add.iter().map(|w| Word {
                text: normalize(w),
                category: GuardCategory::Custom,
                how: How::Anywhere,
            }))
            .filter(|w| !w.text.is_empty())
            .collect();
        words.sort_by_key(|w| std::cmp::Reverse(w.text.chars().count()));
        let mut seen = HashSet::new();
        words.retain(|w| seen.insert(w.text.clone()));

        let mut except: Vec<String> = DEFAULT_EXCEPT
            .iter()
            .map(|p| normalize(p))
            .chain(cfg.except.iter().map(|p| normalize(p)))
            .filter(|p| !p.is_empty())
            .collect();
        except.sort_by_key(|p| std::cmp::Reverse(p.chars().count()));
        except.dedup();

        fn apps(list: &[String]) -> Vec<String> {
            list.iter().map(|b| b.trim().to_string()).filter(|b| !b.is_empty()).collect()
        }
        Self {
            mode: cfg.mode,
            categories: cfg.categories.iter().copied().collect(),
            words,
            except,
            only_apps: apps(&cfg.only_apps),
            confirm_apps: apps(&cfg.confirm_apps),
            block_apps: apps(&cfg.block_apps),
            confirm_timeout: cfg.confirm_timeout(),
            deny_cooldown: cfg.deny_cooldown(),
        }
    }

    pub fn mode(&self) -> GuardMode {
        self.mode
    }

    pub fn confirm_timeout(&self) -> std::time::Duration {
        self.confirm_timeout
    }

    pub fn deny_cooldown(&self) -> std::time::Duration {
        self.deny_cooldown
    }

    /// Whether concerns of this category are guarded. `custom` always is.
    pub fn enabled(&self, category: GuardCategory) -> bool {
        category == GuardCategory::Custom || self.categories.contains(&category)
    }

    /// Only the concerns whose category is guarded.
    pub fn filter(&self, concerns: Vec<Concern>) -> Vec<Concern> {
        concerns.into_iter().filter(|c| self.enabled(c.category)).collect()
    }

    /// Whether any app list is set (only then is the app in front looked up).
    pub fn has_app_lists(&self) -> bool {
        !self.only_apps.is_empty() || !self.confirm_apps.is_empty() || !self.block_apps.is_empty()
    }

    /// Normalized text with the exception phrases taken out.
    fn prepare(&self, text: &str) -> String {
        let mut t = normalize(text);
        for phrase in &self.except {
            if t.contains(phrase.as_str()) {
                t = remove_phrase(&t, phrase);
            }
        }
        t
    }

    /// Label, and identifier split into words, ready to match.
    fn haystacks(&self, e: &Element) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(label) = e.label.as_deref() {
            out.push(self.prepare(label));
        }
        if let Some(id) = e.identifier.as_deref() {
            out.push(self.prepare(&split_identifier(id)));
        }
        out
    }

    /// The first (longest) word per category found in the element, among
    /// the words `keep` accepts.
    fn word_concerns(&self, e: &Element, keep: impl Fn(&Word) -> bool) -> Vec<(GuardCategory, String)> {
        let hay = self.haystacks(e);
        let mut found: Vec<(GuardCategory, String)> = Vec::new();
        for w in self.words.iter().filter(|w| keep(w)) {
            if found.iter().any(|(c, _)| *c == w.category) {
                continue;
            }
            if hay.iter().any(|h| word_matches(h, w)) {
                found.push((w.category, w.text.clone()));
            }
        }
        found
    }

    /// Words in what a tap would press.
    pub fn press_concerns(&self, e: &Element) -> Vec<Concern> {
        if NO_WORD_ROLES.contains(&e.role.as_str()) {
            return Vec::new();
        }
        let name = name_of(e);
        self.word_concerns(e, |_| true)
            .into_iter()
            .map(|(category, word)| Concern {
                category,
                why: say(
                    format!("\"{name}\"에 \"{word}\"이(가) 있음"),
                    format!("\"{name}\" contains \"{word}\""),
                ),
                rule: word,
            })
            .collect()
    }

    /// A generic confirm button ("확인", "OK") on a screen whose text asks
    /// about a payment, deletion, sending or permission.
    pub fn context_concern(&self, screen: &Screen, target: &Element) -> Vec<Concern> {
        let Some(label) = target.label.as_deref() else { return Vec::new() };
        if !is_generic_confirm(&normalize(label)) {
            return Vec::new();
        }
        let mut out: Vec<Concern> = Vec::new();
        for e in &screen.elements {
            if e.pid != target.pid
                || !e.on_screen
                || e.id == target.id
                || !matches!(e.role.as_str(), "text" | "header" | "other")
            {
                continue;
            }
            let Some(text) = e.label.as_deref() else { continue };
            let text = self.prepare(text);
            for (term, category) in CONTEXT_TERMS {
                if out.iter().any(|c| c.category == *category) {
                    continue;
                }
                let hit = if term.is_ascii() {
                    contains_ascii_word(&text, term)
                } else {
                    text.contains(term)
                };
                if hit {
                    let near = e.label.as_deref().unwrap_or_default();
                    out.push(Concern {
                        category: *category,
                        rule: format!("confirm_in_context:{term}"),
                        why: say(
                            format!("\"{label}\" 버튼이 \"{near}\"을(를) 확정할 수 있음"),
                            format!("\"{label}\" may confirm \"{near}\""),
                        ),
                    });
                }
            }
        }
        out
    }

    /// A key press: shortcuts, Return that may press a risky button, and
    /// Backspace/Delete with no field to edit.
    pub fn key_concerns(&self, usage: u64, modifiers: &[u64], screen: &Screen) -> Vec<Concern> {
        let mut mods: Vec<u64> = modifiers.to_vec();
        mods.sort_unstable();
        mods.dedup();
        if mods.iter().any(|m| is_command_modifier(*m)) {
            let cmd_only = mods == [keys::MOD_COMMAND];
            let cmd_shift = mods == [keys::MOD_SHIFT, keys::MOD_COMMAND];
            // cmd+a, cmd+c, cmd+x, cmd+v, cmd+z, cmd+shift+z.
            let editing = (cmd_only && [0x04, 0x06, 0x1B, 0x19, 0x1D].contains(&usage))
                || (cmd_shift && usage == 0x1D);
            if editing {
                return Vec::new();
            }
            return vec![Concern {
                category: GuardCategory::Shortcut,
                rule: "shortcut".into(),
                why: say(
                    "키보드 단축키는 앱에서 어떤 동작이든 할 수 있음".into(),
                    "a keyboard shortcut can do anything in the app".into(),
                ),
            }];
        }
        let focused = screen.elements.iter().find(|e| e.focused);
        if usage == keys::KEY_RETURN || usage == 0x58 {
            if focused.is_some_and(|f| f.role == "search_field") {
                return Vec::new();
            }
            let pid = focused.map(|f| f.pid).or_else(|| screen.apps.first().map(|a| a.pid));
            let mut out: Vec<Concern> = Vec::new();
            for b in screen.elements.iter().filter(|e| {
                e.on_screen
                    && matches!(e.role.as_str(), "button" | "link")
                    && pid.is_none_or(|p| e.pid == p)
            }) {
                let found = self.word_concerns(b, |w| {
                    matches!(w.category, C::Money | C::Delete | C::Custom)
                        || (w.category == C::Send && !WEAK_SEND.contains(&w.text.as_str()))
                });
                for (category, word) in found {
                    if out.iter().any(|c| c.category == category) {
                        continue;
                    }
                    let name = name_of(b);
                    out.push(Concern {
                        category,
                        why: say(
                            format!("Return 키가 \"{name}\"을(를) 누를 수 있음"),
                            format!("Return may press \"{name}\""),
                        ),
                        rule: format!("return:{word}"),
                    });
                }
            }
            return out;
        }
        if usage == keys::KEY_BACKSPACE || usage == keys::KEY_DELETE_FORWARD {
            let editing = focused.is_some_and(|f| FIELD_ROLES.contains(&f.role.as_str()));
            if !editing {
                return vec![Concern {
                    category: GuardCategory::Shortcut,
                    rule: "delete_key_without_field".into(),
                    why: say(
                        "입력 칸 없이 누른 삭제 키는 항목을 지울 수 있음".into(),
                        "a delete key with no text field focused can delete items".into(),
                    ),
                }];
            }
        }
        Vec::new()
    }

    /// Typing into `field`: a password field, or text sent with Return.
    pub fn type_concerns(&self, field: &Element, submit: bool) -> Vec<Concern> {
        let mut out = Vec::new();
        if field.role == "secure_text_field" {
            out.push(Concern {
                category: GuardCategory::Secret,
                rule: "secure_field".into(),
                why: say(
                    "비밀번호 칸에 입력함".into(),
                    "types into a password field".into(),
                ),
            });
        }
        if submit && field.role != "search_field" {
            out.push(Concern {
                category: GuardCategory::Send,
                rule: "submit".into(),
                why: say(
                    "입력 후 Return으로 보냄".into(),
                    "presses Return after typing, which may send it".into(),
                ),
            });
        }
        out
    }

    /// Input sent without reading the screen first.
    pub fn blind_concern(&self) -> Concern {
        Concern {
            category: GuardCategory::Blind,
            rule: "unread_input".into(),
            why: say(
                "화면을 읽지 않고 보내는 입력이라 무엇을 누를지 알 수 없음".into(),
                "input sent without reading the screen: what it presses is unknown".into(),
            ),
        }
    }

    /// `only_apps` / `block_apps`, in every mode. `Err((code, message))`.
    /// An app that cannot be identified is refused under `only_apps` and
    /// allowed otherwise (there is nothing to compare).
    pub fn app_policy(&self, bundle: Option<&str>) -> Result<(), (&'static str, String)> {
        if let Some(b) = bundle
            && self.block_apps.iter().any(|x| x.eq_ignore_ascii_case(b))
        {
            return Err((
                "app_blocked",
                say(
                    "[guard] block_apps에 있는 앱이라 입력하지 않음".into(),
                    "this app is in [guard] block_apps".into(),
                ),
            ));
        }
        if self.only_apps.is_empty() {
            return Ok(());
        }
        match bundle {
            Some(b) if self.only_apps.iter().any(|x| x.eq_ignore_ascii_case(b)) => Ok(()),
            Some(_) => Err((
                "app_not_allowed",
                say(
                    "[guard] only_apps에 없는 앱이라 입력하지 않음".into(),
                    "this app is not in [guard] only_apps".into(),
                ),
            )),
            None => Err((
                "app_not_allowed",
                say(
                    "앞에 있는 앱을 알 수 없어 [guard] only_apps를 확인할 수 없음".into(),
                    "cannot tell which app is in front to check [guard] only_apps".into(),
                ),
            )),
        }
    }

    /// `confirm_apps`: taps, typing, keys, swipes and launches always ask.
    pub fn app_concern(&self, bundle: Option<&str>, action: Action) -> Option<Concern> {
        let b = bundle?;
        if !self.confirm_apps.iter().any(|x| x.eq_ignore_ascii_case(b)) {
            return None;
        }
        if matches!(action, Action::Scroll | Action::Button) {
            return None;
        }
        Some(Concern {
            category: GuardCategory::App,
            rule: "confirm_apps".into(),
            why: say(
                format!("{b}은(는) [guard] confirm_apps에 있어 입력마다 확인함"),
                format!("{b} is in [guard] confirm_apps: every input asks"),
            ),
        })
    }
}

/// "확인", "OK", or a short form like "확인 (2)" or "Next →"; not "확인된 내역".
fn is_generic_confirm(label: &str) -> bool {
    if label.chars().count() > 12 {
        return false;
    }
    GENERIC_CONFIRM.iter().any(|w| {
        label == *w
            || label.strip_prefix(w).is_some_and(|rest| {
                rest.trim_start().chars().next().is_none_or(|c| !c.is_alphanumeric())
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rules() -> Rules {
        Rules::new(&GuardConfig::default())
    }

    fn el(v: serde_json::Value) -> Element {
        let mut v = v;
        if v.get("id").is_none() {
            v["id"] = json!("e0000001");
        }
        if v.get("pid").is_none() {
            v["pid"] = json!(7);
        }
        serde_json::from_value(v).unwrap()
    }

    fn button(label: &str) -> Element {
        el(json!({ "role": "button", "label": label }))
    }

    fn words(r: &Rules, label: &str) -> Vec<String> {
        r.press_concerns(&button(label)).into_iter().map(|c| c.rule).collect()
    }

    fn cats(r: &Rules, label: &str) -> Vec<GuardCategory> {
        r.press_concerns(&button(label)).into_iter().map(|c| c.category).collect()
    }

    fn screen(elements: Vec<serde_json::Value>) -> Screen {
        let elements: Vec<serde_json::Value> = elements
            .into_iter()
            .enumerate()
            .map(|(i, mut e)| {
                if e.get("id").is_none() {
                    e["id"] = json!(format!("e{i:07}"));
                }
                if e.get("pid").is_none() {
                    e["pid"] = json!(7);
                }
                e
            })
            .collect();
        serde_json::from_value(json!({
            "width": 430.0, "height": 932.0,
            "apps": [{ "pid": 7, "name": "가상 앱" }],
            "elements": elements,
        }))
        .unwrap()
    }

    #[test]
    fn korean_nouns_need_an_action_not_a_list() {
        let r = rules();
        assert_eq!(cats(&r, "3,000원 결제"), vec![C::Money]);
        assert_eq!(cats(&r, "결제 (3,000원)"), vec![C::Money]);
        assert_eq!(cats(&r, "결제하기"), vec![C::Money]);
        assert_eq!(cats(&r, "12,000원 결제해 주세요"), vec![C::Money]);
        assert_eq!(cats(&r, "친구에게 보내기"), vec![C::Send]);
        assert!(cats(&r, "결제 내역").is_empty());
        assert!(cats(&r, "결제 완료").is_empty());
        assert!(cats(&r, "송금 내역").is_empty());
        assert!(cats(&r, "송금받기").is_empty());
        assert!(cats(&r, "일반").is_empty());
        assert_eq!(cats(&r, "구독 취소"), vec![C::Delete]);
    }

    #[test]
    fn latin_words_need_word_boundaries() {
        let r = rules();
        assert_eq!(words(&r, "Pay ₩3,000"), vec!["pay"]);
        assert_eq!(words(&r, "Buy now"), vec!["buy"]);
        // The longest word names the concern; "send" is reported as its own category.
        assert_eq!(words(&r, "Send Money"), vec!["send money", "send"]);
        assert!(words(&r, "Display & Brightness").is_empty());
        assert!(words(&r, "Payment history").is_empty());
        assert!(words(&r, "Buyer protection").is_empty());
    }

    #[test]
    fn exceptions_are_removed_before_matching() {
        let r = rules();
        assert!(words(&r, "Wallet & Apple Pay").is_empty());
        assert_eq!(words(&r, "Pay with Apple Pay"), vec!["pay"]);
        assert!(words(&r, "Don't Allow").is_empty());
        assert!(words(&r, "Don\u{2019}t Allow").is_empty());
        assert_eq!(words(&r, "Allow"), vec!["allow"]);
        assert!(words(&r, "허용 안 함").is_empty());
        assert_eq!(cats(&r, "허용"), vec![C::Permission]);
        let custom = Rules::new(&GuardConfig { except: vec!["Send feedback".into()], ..Default::default() });
        assert!(words(&custom, "Send Feedback").is_empty());
        assert_eq!(words(&custom, "Send"), vec!["send"]);
    }

    #[test]
    fn invisible_and_full_width_characters_do_not_hide_words() {
        let r = rules();
        assert_eq!(words(&r, "P\u{200B}a\u{200D}y"), vec!["pay"]);
        assert_eq!(words(&r, "ＰＡＹ"), vec!["pay"]);
        assert_eq!(cats(&r, "결\u{E0041}제\u{00AD}하기"), vec![C::Money]);
        assert_eq!(cats(&r, "\u{202E}송금"), vec![C::Money]);
        assert_eq!(normalize("  A\u{200B}  b\n c "), "a b c");
    }

    #[test]
    fn identifiers_are_split_into_words() {
        let r = rules();
        let e = el(json!({ "role": "button", "identifier": "btnSendMessage" }));
        assert_eq!(r.press_concerns(&e)[0].rule, "send");
        let e = el(json!({ "role": "button", "identifier": "PAYButton" }));
        assert_eq!(r.press_concerns(&e)[0].rule, "pay");
        let e = el(json!({ "role": "button", "identifier": "display_settings" }));
        assert!(r.press_concerns(&e).is_empty());
        assert_eq!(split_identifier("btnSendMessage"), "btn Send Message");
    }

    #[test]
    fn values_hints_fields_and_headers_are_not_read_for_words() {
        let r = rules();
        let typed = el(json!({ "role": "button", "label": "메모", "value": "송금", "hint": "pay" }));
        assert!(r.press_concerns(&typed).is_empty());
        for role in ["text_field", "search_field", "secure_text_field", "header", "application"] {
            assert!(r.press_concerns(&el(json!({ "role": role, "label": "결제" }))).is_empty(), "{role}");
        }
        assert_eq!(r.press_concerns(&el(json!({ "role": "text", "label": "결제하기" }))).len(), 1);
    }

    #[test]
    fn config_adds_removes_and_picks_categories() {
        let r = Rules::new(&GuardConfig {
            add: vec!["Tip".into()],
            remove: vec!["보내기".into()],
            categories: vec![C::Money],
            ..Default::default()
        });
        assert_eq!(cats(&r, "Tip"), vec![C::Custom]);
        assert!(cats(&r, "메모 보내기").is_empty());
        let both = r.press_concerns(&button("Delete and pay"));
        assert_eq!(both.len(), 2);
        let kept = r.filter(both);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].category, C::Money);
        assert!(r.enabled(C::Custom));
        assert!(!r.enabled(C::Send));
    }

    #[test]
    fn a_generic_confirm_is_judged_by_the_text_around_it() {
        let r = rules();
        let alert = screen(vec![
            json!({ "role": "text", "label": "김철수님에게 50,000원을 송금하시겠습니까?" }),
            json!({ "id": "cancel01", "role": "button", "label": "취소" }),
            json!({ "id": "ok000001", "role": "button", "label": "확인" }),
        ]);
        let ok = alert.by_id("ok000001").unwrap();
        let found = r.context_concern(&alert, ok);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].category, C::Money);
        let cancel = alert.by_id("cancel01").unwrap();
        assert!(r.context_concern(&alert, cancel).is_empty());

        // Another app's text does not count, nor does a calm screen.
        let mut other = alert.clone();
        other.elements[0].pid = 99;
        assert!(r.context_concern(&other, other.by_id("ok000001").unwrap()).is_empty());

        assert!(is_generic_confirm("확인"));
        assert!(is_generic_confirm("확인 (2)"));
        assert!(is_generic_confirm("next →"));
        assert!(!is_generic_confirm("확인된 내역"));
        assert!(!is_generic_confirm("다음에 하기"));
        assert!(!is_generic_confirm("예약"));
    }

    #[test]
    fn return_is_judged_by_the_buttons_it_may_press() {
        let r = rules();
        let chat = screen(vec![
            json!({ "role": "text_field", "label": "메시지", "focused": true }),
            json!({ "role": "button", "label": "보내기" }),
        ]);
        let found = r.key_concerns(keys::KEY_RETURN, &[], &chat);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].category, C::Send);

        let search = screen(vec![
            json!({ "role": "search_field", "label": "검색", "focused": true }),
            json!({ "role": "button", "label": "결제하기" }),
        ]);
        assert!(r.key_concerns(keys::KEY_RETURN, &[], &search).is_empty());

        let login = screen(vec![
            json!({ "role": "secure_text_field", "label": "비밀번호", "focused": true }),
            json!({ "role": "button", "label": "로그인" }),
            json!({ "role": "button", "label": "공유" }),
        ]);
        assert!(r.key_concerns(keys::KEY_RETURN, &[], &login).is_empty());

        let checkout = screen(vec![json!({ "role": "button", "label": "3,000원 결제하기" })]);
        let found = r.key_concerns(0x58, &[], &checkout);
        assert_eq!(found[0].category, C::Money);
    }

    #[test]
    fn shortcuts_and_delete_keys() {
        let r = rules();
        let empty = screen(vec![]);
        assert!(r.key_concerns(0x19, &[keys::MOD_COMMAND], &empty).is_empty()); // cmd+v
        assert!(r.key_concerns(0x1D, &[keys::MOD_COMMAND, keys::MOD_SHIFT], &empty).is_empty());
        let d = r.key_concerns(0x07, &[keys::MOD_COMMAND, keys::MOD_SHIFT], &empty);
        assert_eq!(d[0].category, C::Shortcut);
        assert_eq!(r.key_concerns(0x04, &[keys::MOD_CONTROL], &empty)[0].category, C::Shortcut);
        assert!(r.key_concerns(0x04, &[keys::MOD_SHIFT], &empty).is_empty());

        assert_eq!(r.key_concerns(keys::KEY_BACKSPACE, &[], &empty)[0].category, C::Shortcut);
        let field = screen(vec![json!({ "role": "text_field", "focused": true })]);
        assert!(r.key_concerns(keys::KEY_BACKSPACE, &[], &field).is_empty());
        assert!(r.key_concerns(keys::KEY_DELETE_FORWARD, &[], &field).is_empty());
        assert!(r.key_concerns(keys::KEY_ESCAPE, &[], &empty).is_empty());
    }

    #[test]
    fn typing_secrets_and_submitting() {
        let r = rules();
        let secure = el(json!({ "role": "secure_text_field" }));
        let plain = el(json!({ "role": "text_field" }));
        let search = el(json!({ "role": "search_field" }));
        assert_eq!(r.type_concerns(&secure, false)[0].category, C::Secret);
        assert!(r.type_concerns(&plain, false).is_empty());
        assert_eq!(r.type_concerns(&plain, true)[0].category, C::Send);
        assert!(r.type_concerns(&search, true).is_empty());
        assert_eq!(r.type_concerns(&secure, true).len(), 2);
    }

    #[test]
    fn app_lists() {
        let r = rules();
        assert!(!r.has_app_lists());
        assert!(r.app_policy(None).is_ok());
        assert!(r.app_policy(Some("com.example.shop")).is_ok());

        let only = Rules::new(&GuardConfig { only_apps: vec!["com.apple.Preferences".into()], ..Default::default() });
        assert!(only.has_app_lists());
        assert!(only.app_policy(Some("com.apple.Preferences")).is_ok());
        assert_eq!(only.app_policy(Some("com.example.shop")).unwrap_err().0, "app_not_allowed");
        assert_eq!(only.app_policy(None).unwrap_err().0, "app_not_allowed");

        let block = Rules::new(&GuardConfig { block_apps: vec!["com.example.bank".into()], ..Default::default() });
        assert_eq!(block.app_policy(Some("com.example.bank")).unwrap_err().0, "app_blocked");
        assert!(block.app_policy(None).is_ok());

        let ask = Rules::new(&GuardConfig { confirm_apps: vec!["com.example.bank".into()], ..Default::default() });
        assert_eq!(ask.app_concern(Some("com.example.bank"), Action::Tap).unwrap().category, C::App);
        assert!(ask.app_concern(Some("com.example.bank"), Action::Scroll).is_none());
        assert!(ask.app_concern(Some("com.other"), Action::Tap).is_none());
        assert!(ask.app_concern(None, Action::Tap).is_none());
    }
}
