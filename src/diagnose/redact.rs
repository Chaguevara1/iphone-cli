//! Remove personal and network identifiers from a diagnostics report before it
//! is written. The report stays complete in memory; only the file is redacted.
//! Placeholders are stable for the whole run, so `<udid-1>` names the same
//! phone in every snapshot and on every save.

use std::{
    collections::HashMap,
    net::{Ipv4Addr, Ipv6Addr},
    sync::LazyLock,
};

use regex::Regex;
use serde_json::{Value, json};

/// String values under these keys are device identifiers.
const UDID_KEYS: &[&str] = &[
    "udid",
    "selected_udid",
    "SerialNumber",
    "USBSerialNumber",
    "UniqueDeviceID",
    "IPH_UDID",
];
/// Default device names that identify nobody.
const GENERIC_DEVICE_NAMES: &[&str] = &["iphone", "ipad", "ipod touch"];
/// Keys of the reproduction output that carry screen text or typed text.
const SCREEN_KEYS: &[&str] = &[
    "elements",
    "screen_summary",
    "candidates",
    "element",
    "field",
    "value",
    "value_before",
    "outputs",
    "logs",
    "rows",
    "apps",
    "covered_by",
    "text",
];

fn pattern(text: &str) -> Regex {
    Regex::new(text).expect("redaction pattern")
}

// The patterns carry no context of their own; `separated` rejects matches that
// are part of a longer word or a longer dotted/colon/dash run.
static DUID: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)[0-9a-f]{2}(?:-[0-9a-f]{2}){7,}"));
static MAC: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r"(?i)[0-9a-f]{2}(?::[0-9a-f]{2}){5}|[0-9a-f]{2}(?:-[0-9a-f]{2}){5}")
});
static UDID_MODERN: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)[0-9a-f]{8}-[0-9a-f]{16}"));
static UDID_LEGACY: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)[0-9a-f]{40}"));
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")
});
static IPV6: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r"(?i)[0-9a-f]{0,4}(?::[0-9a-f]{0,4}){2,7}(?:%[0-9a-z_-]+)?")
});
static IPV4: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}\.[0-9]{1,3}"));
static SUBCOMMAND: LazyLock<Regex> = LazyLock::new(|| pattern(r"^[a-z][a-z0-9-]{0,31}$"));

#[derive(Default)]
pub struct Redactor {
    /// Lowercased literal -> placeholder.
    literals: HashMap<String, String>,
    literal_re: Option<Regex>,
    dirty: bool,
    /// "kind:lowercased value" -> placeholder.
    names: HashMap<String, String>,
    counts: HashMap<&'static str, usize>,
}

impl Redactor {
    /// Literals from this computer: home directory, user, host and domain names.
    pub fn new() -> Self {
        let var = |key: &str| std::env::var(key).ok().filter(|v| !v.trim().is_empty());
        let homes: Vec<String> = ["HOME", "USERPROFILE"].into_iter().filter_map(var).collect();
        let users: Vec<String> = ["USERNAME", "USER"].into_iter().filter_map(var).collect();
        let domains: Vec<String> = ["USERDNSDOMAIN", "USERDOMAIN"]
            .into_iter()
            .filter_map(var)
            .collect();
        let mut hosts: Vec<String> = var("COMPUTERNAME").into_iter().collect();
        hosts.extend(hostname());
        let short: Vec<String> = hosts
            .iter()
            .filter_map(|h| h.split_once('.').map(|(name, _)| name.to_string()))
            .collect();
        hosts.extend(short);
        Self::from_parts(&homes, &users, &hosts, &domains)
    }

    fn from_parts(homes: &[String], users: &[String], hosts: &[String], domains: &[String]) -> Self {
        let mut redactor = Self::default();
        for home in homes {
            let home = home.trim().trim_end_matches(['/', '\\']);
            if home.chars().count() >= 3 {
                redactor.add_literal(home, "~");
                if home.contains('\\') {
                    redactor.add_literal(&home.replace('\\', "/"), "~");
                }
            }
        }
        for (values, label) in [(hosts, "<host>"), (users, "<user>"), (domains, "<domain>")] {
            for value in values {
                let value = value.trim();
                if value.chars().count() >= 3 && !value.eq_ignore_ascii_case("localhost") {
                    redactor.add_literal(value, label);
                }
            }
        }
        redactor
    }

    /// Learn identifiers from the report: device ids and the device name.
    pub fn harvest(&mut self, data: &Value) {
        self.harvest_value(data, None);
        // The reproduction output is JSON inside a string.
        if let Some(stdout) = data["reproduction"]["stdout"].as_str()
            && let Ok(parsed) = serde_json::from_str::<Value>(stdout)
        {
            self.harvest_value(&parsed, None);
        }
    }

    fn harvest_value(&mut self, value: &Value, key: Option<&str>) {
        match value {
            Value::Object(map) => {
                for (key, item) in map {
                    self.harvest_value(item, Some(key.as_str()));
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.harvest_value(item, key);
                }
            }
            Value::String(text) => match key {
                Some(key) if UDID_KEYS.contains(&key) => {
                    let udid = text.trim();
                    if udid.len() >= 8 {
                        let name = self.pseudonym("udid", udid);
                        self.add_literal(udid, &name);
                    }
                }
                Some("DeviceName") => {
                    let name = text.trim();
                    let generic = GENERIC_DEVICE_NAMES
                        .iter()
                        .any(|g| name.eq_ignore_ascii_case(g));
                    if name.chars().count() >= 3 && !generic {
                        self.add_literal(name, "<device-name>");
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    /// A redacted copy of the report.
    pub fn apply(&mut self, data: &Value) -> Value {
        let mut out = data.clone();
        match &mut out {
            Value::Object(map) => {
                for (key, value) in map.iter_mut() {
                    match key.as_str() {
                        "reproduction" => self.reproduction(value),
                        "config" => self.walk(value, true),
                        _ => self.walk(value, false),
                    }
                }
            }
            other => self.walk(other, false),
        }
        out
    }

    fn walk(&mut self, value: &mut Value, config: bool) {
        match value {
            Value::String(text) => *text = self.text(text),
            Value::Array(items) => {
                for item in items {
                    self.walk(item, config);
                }
            }
            Value::Object(map) => {
                for (key, item) in map.iter_mut() {
                    if key == "NetworkAddress" && item.is_array() {
                        *item = json!("<sockaddr redacted>");
                    } else if config && key.ends_with("_apps") && item.is_array() {
                        let count = item.as_array().map_or(0, Vec::len);
                        *item = json!(format!("<{count} bundle ids>"));
                    } else {
                        self.walk(item, config);
                    }
                }
            }
            _ => {}
        }
    }

    fn reproduction(&mut self, value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, item) in map.iter_mut() {
                    match key.as_str() {
                        "stdout" => {
                            let redacted = item.as_str().map(|text| self.stdout(text));
                            match redacted {
                                Some(redacted) => *item = redacted,
                                None => self.walk(item, false),
                            }
                        }
                        "args" => match item {
                            Value::Array(args) => self.args(args),
                            other => self.walk(other, false),
                        },
                        _ => self.walk(item, false),
                    }
                }
            }
            other => self.walk(other, false),
        }
    }

    /// Keep the outcome, error and timings of the reproduced command; reduce
    /// screen content and typed text to its shape.
    fn stdout(&mut self, text: &str) -> Value {
        match serde_json::from_str::<Value>(text) {
            Ok(mut parsed) if parsed.is_object() || parsed.is_array() => {
                self.screen(&mut parsed);
                Value::String(parsed.to_string())
            }
            _ if text.is_empty() => json!(""),
            _ => json!(format!("<{} bytes redacted>", text.len())),
        }
    }

    fn screen(&mut self, value: &mut Value) {
        match value {
            Value::Object(map) => {
                for (key, item) in map.iter_mut() {
                    if SCREEN_KEYS.contains(&key.as_str()) {
                        *item = shape(item);
                    } else {
                        self.screen(item);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.screen(item);
                }
            }
            Value::String(text) => *text = self.text(text),
            _ => {}
        }
    }

    /// Keep flags and the subcommand name; other values become a placeholder.
    fn args(&mut self, args: &mut [Value]) {
        let mut value_next = false;
        let mut command_seen = false;
        for arg in args.iter_mut() {
            let Some(text) = arg.as_str().map(str::to_owned) else {
                continue;
            };
            let redacted = if text.starts_with('-') {
                value_next = text == "--udid";
                match text.find('=') {
                    Some(at) => format!("{}={}", &text[..at], self.arg_value(&text[at + 1..])),
                    None => text,
                }
            } else if !value_next && !command_seen && SUBCOMMAND.is_match(&text) {
                command_seen = true;
                text
            } else {
                value_next = false;
                self.arg_value(&text)
            };
            *arg = Value::String(redacted);
        }
    }

    fn arg_value(&mut self, value: &str) -> String {
        let redacted = self.text(value);
        let placeholder = redacted.starts_with('<')
            && redacted.ends_with('>')
            && !redacted[1..redacted.len() - 1].contains(['<', '>']);
        if placeholder {
            redacted
        } else {
            "<redacted>".into()
        }
    }

    fn add_literal(&mut self, literal: &str, placeholder: &str) {
        let key = literal.to_lowercase();
        if !self.literals.contains_key(&key) {
            self.literals.insert(key, placeholder.to_string());
            self.dirty = true;
        }
    }

    fn pseudonym(&mut self, kind: &'static str, value: &str) -> String {
        let key = format!("{kind}:{}", value.to_lowercase());
        if let Some(name) = self.names.get(&key) {
            return name.clone();
        }
        let count = self.counts.entry(kind).or_insert(0);
        *count += 1;
        let name = format!("<{kind}-{count}>");
        self.names.insert(key, name.clone());
        name
    }

    fn refresh(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        // Longest first: the home directory wins over the user name inside it.
        let mut keys: Vec<&String> = self.literals.keys().collect();
        keys.sort_by(|a, b| {
            b.chars()
                .count()
                .cmp(&a.chars().count())
                .then_with(|| a.cmp(b))
        });
        let alternatives: Vec<String> = keys.iter().map(|k| regex::escape(k)).collect();
        self.literal_re = if alternatives.is_empty() {
            None
        } else {
            Regex::new(&format!("(?i){}", alternatives.join("|"))).ok()
        };
    }

    fn text(&mut self, input: &str) -> String {
        self.refresh();
        let mut text = match self.literal_re.clone() {
            Some(re) => replace_matches(input, &re, |m, before, after| {
                if !bounded(m, before, after) {
                    return None;
                }
                let name = self.literals.get(&m.to_lowercase()).cloned();
                Some(name.unwrap_or_else(|| "<redacted>".into()))
            }),
            None => input.to_string(),
        };
        text = replace_matches(&text, &DUID, |_, before, after| {
            separated(before, after, &['-']).then(|| "<duid>".to_string())
        });
        text = replace_matches(&text, &MAC, |m, before, after| {
            if !separated(before, after, &[':', '-']) {
                return None;
            }
            Some(self.pseudonym("mac", &m.replace('-', ":")))
        });
        for re in [&*UDID_MODERN, &*UDID_LEGACY] {
            text = replace_matches(&text, re, |m, before, after| {
                if !separated(before, after, &['-']) {
                    return None;
                }
                Some(self.pseudonym("udid", m))
            });
        }
        text = EMAIL.replace_all(&text, "<email>").into_owned();
        text = replace_matches(&text, &IPV6, |m, before, after| {
            // A '<' follows our own `fe80::<iid-N>`; never redact it twice.
            if !separated(before, after, &[':', '.']) || after.starts_with('<') {
                return None;
            }
            let (address, scope) = match m.split_once('%') {
                Some((address, scope)) => (address, Some(scope)),
                None => (m, None),
            };
            let ip: Ipv6Addr = address.parse().ok()?;
            self.ipv6(ip, scope)
        });
        replace_matches(&text, &IPV4, |m, before, after| {
            if !separated(before, after, &['.']) {
                return None;
            }
            let ip: Ipv4Addr = m.parse().ok()?;
            if keep_ipv4(ip) {
                return None;
            }
            Some(self.pseudonym("ip", &ip.to_string()))
        })
    }

    fn ipv6(&mut self, ip: Ipv6Addr, scope: Option<&str>) -> Option<String> {
        let segments = ip.segments();
        let documentation = segments[0] == 0x2001 && segments[1] == 0x0db8;
        if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || documentation {
            return None;
        }
        if let Some(v4) = ip.to_ipv4_mapped()
            && keep_ipv4(v4)
        {
            return None;
        }
        let scope = scope.map(|s| format!("%{s}")).unwrap_or_default();
        let key = ip.to_string();
        Some(if segments[0] & 0xffc0 == 0xfe80 {
            format!("fe80::{}{scope}", self.pseudonym("iid", &key))
        } else if segments[0] & 0xfe00 == 0xfc00 {
            format!("{}{scope}", self.pseudonym("ula", &key))
        } else {
            format!("{}{scope}", self.pseudonym("ip6", &key))
        })
    }
}

/// Private, loopback, link-local, CGNAT, documentation, multicast, reserved,
/// broadcast and netmask values say nothing about where the user is.
fn keep_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let bits = u32::from(ip);
    a == 0
        || a == 10
        || a == 127
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 169 && b == 254)
        || (a == 100 && (64..=127).contains(&b))
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224
        || bits.leading_ones() + bits.trailing_zeros() == 32
}

fn shape(value: &Value) -> Value {
    match value {
        Value::Null | Value::Bool(_) => value.clone(),
        Value::Number(_) => json!("<number>"),
        Value::String(text) => json!(format!("<text, {} chars>", text.chars().count())),
        Value::Array(items) => json!(format!("<{} items>", items.len())),
        Value::Object(map) => json!(format!("<object, {} keys>", map.len())),
    }
}

/// Replace the matches for which `replace` returns a value; keep the rest.
fn replace_matches(
    text: &str,
    re: &Regex,
    mut replace: impl FnMut(&str, &str, &str) -> Option<String>,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for m in re.find_iter(text) {
        if let Some(replacement) = replace(m.as_str(), &text[..m.start()], &text[m.end()..]) {
            out.push_str(&text[last..m.start()]);
            out.push_str(&replacement);
            last = m.end();
        }
    }
    out.push_str(&text[last..]);
    out
}

/// A literal is replaced only as a whole word ("kim" is not removed from "kimchi").
fn bounded(m: &str, before: &str, after: &str) -> bool {
    let starts = m.chars().next().is_some_and(char::is_alphanumeric);
    let ends = m.chars().next_back().is_some_and(char::is_alphanumeric);
    let touches_before = before.chars().next_back().is_some_and(char::is_alphanumeric);
    let touches_after = after.chars().next().is_some_and(char::is_alphanumeric);
    !(starts && touches_before) && !(ends && touches_after)
}

/// The match is not part of a longer word or of a longer run joined by `seps`
/// (so "1.2.3.4.5" or a 7-group MAC-like run is left alone). A trailing
/// separator followed by a non-alphanumeric character ends a sentence.
fn separated(before: &str, after: &str, seps: &[char]) -> bool {
    if before
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || seps.contains(&c))
    {
        return false;
    }
    let mut next = after.chars();
    match next.next() {
        None => true,
        Some(c) if c.is_alphanumeric() => false,
        Some(c) if seps.contains(&c) => !next.next().is_some_and(|d| d.is_alphanumeric()),
        Some(_) => true,
    }
}

#[cfg(unix)]
fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for `buf.len()` bytes; gethostname writes at
    // most that many and we stop at the first NUL (or the end).
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast::<libc::c_char>(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..end].to_vec())
        .ok()
        .filter(|h| !h.trim().is_empty())
}

#[cfg(not(unix))]
fn hostname() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(redactor: &mut Redactor, data: &Value) -> Value {
        redactor.harvest(data);
        redactor.apply(data)
    }

    #[test]
    fn device_ids_get_stable_placeholders_in_any_case() {
        let mut r = Redactor::default();
        let data = json!({ "selected_udid": "00000000-DIAG-TEST-PHONE",
            "log": "lost 00000000-diag-test-phone at /x", "path": "daemon-00000000-DIAG-TEST-PHONE.log" });
        let out = run(&mut r, &data);
        assert_eq!(out["selected_udid"], "<udid-1>");
        assert_eq!(out["log"], "lost <udid-1> at /x");
        assert_eq!(out["path"], "daemon-<udid-1>.log");
        // A later save keeps the same names.
        assert_eq!(run(&mut r, &data), out);
        let found = r.text("device 00008030-001A2B3C4D5E6F70 and 0123456789abcdef0123456789abcdef01234567");
        assert_eq!(found, "device <udid-2> and <udid-3>");
    }

    #[test]
    fn korean_device_name_is_replaced_but_generic_names_stay() {
        let mut r = Redactor::default();
        let data = json!({ "probes": [{ "values": { "DeviceName": "철수의 iPhone" } }],
            "bonjour": "철수의 iPhone._apple-mobdev2._tcp" });
        let out = run(&mut r, &data);
        assert_eq!(out["probes"][0]["values"]["DeviceName"], "<device-name>");
        assert_eq!(out["bonjour"], "<device-name>._apple-mobdev2._tcp");
        assert!(!out.to_string().contains("철수"));

        let mut r = Redactor::default();
        let data = json!({ "DeviceName": "iPhone", "note": "My iPhone" });
        assert_eq!(run(&mut r, &data)["note"], "My iPhone");
    }

    #[test]
    fn both_mac_formats_map_to_one_placeholder() {
        let mut r = Redactor::default();
        assert_eq!(
            r.text("ether a4:83:e7:12:34:56 and A4-83-E7-12-34-56."),
            "ether <mac-1> and <mac-1>."
        );
        assert_eq!(
            r.text("DUID: 00-01-00-01-2A-3B-4C-5D-A4-83-E7-12-34-56"),
            "DUID: <duid>"
        );
    }

    #[test]
    fn public_ipv4_is_replaced_and_local_ranges_stay() {
        let mut r = Redactor::default();
        let local = "inet 192.168.0.10 netmask 255.255.255.0 broadcast 192.168.0.255 \
            10.1.2.3 172.20.1.1 127.0.0.1 100.64.1.1 169.254.3.4 224.0.0.251 0.0.0.0 192.0.2.7";
        assert_eq!(r.text(local), local);
        assert_eq!(
            r.text("connect 8.8.8.8:443 then 8.8.8.8 and 17.5.7.9."),
            "connect <ip-1>:443 then <ip-1> and <ip-2>."
        );
    }

    #[test]
    fn ipv6_keeps_scope_and_local_kinds() {
        let mut r = Redactor::default();
        assert_eq!(
            r.text("inet6 fe80::1c2b:3a4d:5e6f:7a8b%en0 prefixlen 64"),
            "inet6 fe80::<iid-1>%en0 prefixlen 64"
        );
        assert_eq!(r.text("::1 ff02::fb 2001:db8::1"), "::1 ff02::fb 2001:db8::1");
        assert_eq!(r.text("fd12:3456:789a::1"), "<ula-1>");
        assert_eq!(r.text("[2600:1700:abcd::42]:62078"), "[<ip6-1>]:62078");
        // Our own output is not redacted again.
        assert_eq!(r.text("fe80::<iid-1>%en0"), "fe80::<iid-1>%en0");
    }

    #[test]
    fn ordinary_text_is_not_mistaken_for_identifiers() {
        let mut r = Redactor::default();
        for sample in [
            "2026-11-16T12:34:56.789Z",
            "elapsed 12:34:56",
            "iph 0.1.4",
            "ProductVersion 27.0.1",
            "Microsoft Windows [Version 10.0.26100.4652]",
            "build 0.1.4-1731234567",
            "3f2b8c1e-4d5a-4b6c-8d7e-9f0a1b2c3d4e",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "std::io::Error",
            "aa:bb@Phone with spaces",
            "1.2.3.4.5",
            "usbmuxd.ListDevices failed at 127.0.0.1:27015",
        ] {
            assert_eq!(r.text(sample), sample);
        }
        assert_eq!(r.text("mail kim.lee+x@example.co.kr now"), "mail <email> now");
    }

    #[test]
    fn sockaddr_bytes_and_bundle_lists_are_summarized() {
        let mut r = Redactor::default();
        let data = json!({
            "snapshots": [{ "raw_usbmuxd": { "response": { "DeviceList": [{ "Properties": {
                "NetworkAddress": [16, 2, 0, 0, 192, 168, 1, 4] } }] } } }],
            "config": { "values": { "guard": { "only_apps": ["com.a", "com.b"], "mode": "human" } } },
        });
        let out = run(&mut r, &data);
        assert_eq!(
            out["snapshots"][0]["raw_usbmuxd"]["response"]["DeviceList"][0]["Properties"]["NetworkAddress"],
            "<sockaddr redacted>"
        );
        assert_eq!(out["config"]["values"]["guard"]["only_apps"], "<2 bundle ids>");
        assert_eq!(out["config"]["values"]["guard"]["mode"], "human");
    }

    #[test]
    fn reproduction_keeps_the_error_code_and_drops_screen_and_typed_text() {
        let mut r = Redactor::default();
        let stdout = json!({ "ok": false, "outcome": "not_sent",
            "error": { "code": "no_device", "message": "no iPhone at 8.8.8.8" },
            "elements": [{ "label": "Secret" }], "value": "typed words", "timings": { "total_ms": 12 } })
        .to_string();
        let data = json!({
            "reproduction": { "stdout": stdout, "stderr": "",
                "args": ["--udid", "00000000-DIAG-TEST-PHONE", "type", "hello there", "--submit", "--text=abc"] },
            "selected_udid": "00000000-DIAG-TEST-PHONE",
        });
        let out = run(&mut r, &data);
        let parsed: Value =
            serde_json::from_str(out["reproduction"]["stdout"].as_str().unwrap()).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["outcome"], "not_sent");
        assert_eq!(parsed["error"]["code"], "no_device");
        assert_eq!(parsed["error"]["message"], "no iPhone at <ip-1>");
        assert_eq!(parsed["elements"], "<1 items>");
        assert_eq!(parsed["value"], "<text, 11 chars>");
        assert_eq!(parsed["timings"]["total_ms"], 12);
        let text = out.to_string();
        assert!(!text.contains("Secret") && !text.contains("typed words"));
        assert_eq!(
            out["reproduction"]["args"],
            json!(["--udid", "<udid-1>", "type", "<redacted>", "--submit", "--text=<redacted>"])
        );

        let data = json!({ "reproduction": { "stdout": "plain words\n" } });
        assert_eq!(run(&mut r, &data)["reproduction"]["stdout"], "<12 bytes redacted>");
    }

    #[test]
    fn home_user_and_host_are_replaced_as_whole_words() {
        let mut r = Redactor::from_parts(
            &["/Users/kim/".into()],
            &["kim".into()],
            &["kims-mac.local".into(), "kims-mac".into()],
            &[],
        );
        assert_eq!(r.text("/Users/kim/Library/Logs"), "~/Library/Logs");
        assert_eq!(r.text("user kim on KIMS-MAC.local"), "user <user> on <host>");
        assert_eq!(r.text("kimchi"), "kimchi");

        let mut r = Redactor::from_parts(&[r"C:\Users\Kim".into()], &["ab".into()], &[], &[]);
        assert_eq!(r.text(r"C:\Users\Kim\AppData\Local\iph"), r"~\AppData\Local\iph");
        assert_eq!(r.text("c:/users/kim/x"), "~/x");
        // Names shorter than 3 characters are never treated as identifiers.
        assert_eq!(r.text("ab cd"), "ab cd");
    }
}
