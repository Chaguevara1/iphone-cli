//! `~/.config/iph/config.toml`.

use serde::Deserialize;

use crate::paths;

#[derive(Debug, Clone, Deserialize, serde::Serialize, Default)]
#[serde(default)]
pub struct Config {
    /// Device to use when `--udid` is not given.
    pub udid: Option<String>,
    pub dnd: DndConfig,
    pub daemon: DaemonConfig,
    pub script: ScriptConfig,
    pub guard: GuardConfig,
    pub mcp: McpConfig,
}

/// How the guard treats an action it judges risky.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GuardMode {
    /// Ask the person at the PC with a dialog; no answer is a no.
    #[default]
    Human,
    /// Refuse without asking.
    Deny,
    /// No word or confirmation checks; app lists still apply.
    Off,
}

impl GuardMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GuardMode::Human => "human",
            GuardMode::Deny => "deny",
            GuardMode::Off => "off",
        }
    }
}

/// What a risky action would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GuardCategory {
    Money,
    Send,
    Delete,
    Permission,
    Secret,
    Shortcut,
    Blind,
    App,
    /// Words from `[guard] add`; always guarded.
    Custom,
}

impl GuardCategory {
    /// The categories guarded by default: all but `custom`, which is always on.
    pub const DEFAULT: [GuardCategory; 8] = [
        GuardCategory::Money,
        GuardCategory::Send,
        GuardCategory::Delete,
        GuardCategory::Permission,
        GuardCategory::Secret,
        GuardCategory::Shortcut,
        GuardCategory::Blind,
        GuardCategory::App,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            GuardCategory::Money => "money",
            GuardCategory::Send => "send",
            GuardCategory::Delete => "delete",
            GuardCategory::Permission => "permission",
            GuardCategory::Secret => "secret",
            GuardCategory::Shortcut => "shortcut",
            GuardCategory::Blind => "blind",
            GuardCategory::App => "app",
            GuardCategory::Custom => "custom",
        }
    }
}

/// `[guard]`: what iph refuses or asks about before it presses something.
/// Unknown keys and bad values fail the config load, so a typo cannot turn
/// the guard off silently.
#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GuardConfig {
    pub mode: GuardMode,
    pub categories: Vec<GuardCategory>,
    /// Extra words, always guarded (category `custom`).
    pub add: Vec<String>,
    /// Built-in words to stop guarding.
    pub remove: Vec<String>,
    /// Extra phrases removed from a label before matching.
    pub except: Vec<String>,
    /// How long the dialog waits (10 to 90 seconds); no answer is a no.
    pub confirm_timeout_secs: u64,
    /// After a no or a timeout, refuse guarded actions without asking for
    /// this many seconds.
    pub deny_cooldown_secs: u64,
    /// When not empty: the only bundle ids iph may launch or send input to.
    /// An app it cannot identify is refused.
    pub only_apps: Vec<String>,
    /// Taps, typing, keys, swipes and launches in these apps always ask.
    pub confirm_apps: Vec<String>,
    /// Never launch or send input to these apps.
    pub block_apps: Vec<String>,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            mode: GuardMode::Human,
            categories: GuardCategory::DEFAULT.to_vec(),
            add: Vec::new(),
            remove: Vec::new(),
            except: Vec::new(),
            confirm_timeout_secs: 45,
            deny_cooldown_secs: 30,
            only_apps: Vec::new(),
            confirm_apps: Vec::new(),
            block_apps: Vec::new(),
        }
    }
}

impl GuardConfig {
    /// The dialog's wait, clamped to 10..=90 seconds.
    pub fn confirm_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.confirm_timeout_secs.clamp(10, 90))
    }

    pub fn deny_cooldown(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.deny_cooldown_secs)
    }
}

/// `[mcp]`: settings of `iph mcp` only.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    /// Absolute folder for `iph_screenshot` files; default
    /// `<state dir>/screenshots`.
    pub screenshot_dir: Option<std::path::PathBuf>,
}

/// Limits for `iph script` and `iph run`.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct ScriptConfig {
    /// A script is stopped after this many seconds (at most 840).
    pub timeout_secs: u64,
    /// A script is stopped when it holds more than this many megabytes.
    pub memory_mb: u64,
}

impl Default for ScriptConfig {
    fn default() -> Self {
        Self { timeout_secs: 300, memory_mb: 64 }
    }
}

/// `iph dnd`.
#[derive(Debug, Clone, Deserialize, serde::Serialize, Default)]
#[serde(default)]
pub struct DndConfig {
    /// The Focus to switch to instead of Do Not Disturb, by the name the
    /// phone shows for it.
    pub focus: Option<String>,
    /// Label of the Focus control in Control Center, for a phone whose
    /// language is neither Korean nor English.
    pub control: Option<String>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Close the touch-input media session after this many idle seconds.
    /// 0 closes it at the end of every command.
    pub input_idle_secs: u64,
    /// Exit the daemon after this many idle seconds.
    pub idle_exit_secs: u64,
    /// Also take the screen video while an input session is open. Touches
    /// count without it, so it is off: the video is megabits a second through
    /// the same tunnel as everything else.
    pub screen_video: bool,
    /// When the phone's accessibility daemon stops answering, end it so the
    /// phone starts a new one, and read again.
    pub restart_accessibility: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            input_idle_secs: 120,
            idle_exit_secs: 1800,
            screen_video: false,
            restart_accessibility: true,
        }
    }
}

impl Config {
    pub fn load() -> anyhow::Result<Config> {
        let path = paths::config_file();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|e| anyhow::anyhow!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(anyhow::anyhow!("{}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partial_config() {
        let c: Config = toml::from_str("udid = \"X\"\n[daemon]\ninput_idle_secs = 0\n").unwrap();
        assert_eq!(c.udid.as_deref(), Some("X"));
        assert_eq!(c.daemon.input_idle_secs, 0);
        assert_eq!(c.daemon.idle_exit_secs, 1800);
        assert_eq!((c.script.timeout_secs, c.script.memory_mb), (300, 64));
    }

    #[test]
    fn dnd_can_point_at_another_focus() {
        let c: Config = toml::from_str("[dnd]\nfocus = \"업무\"\n").unwrap();
        assert_eq!(c.dnd.focus.as_deref(), Some("업무"));
    }

    #[test]
    fn an_old_safety_section_is_ignored() {
        let c: Config = toml::from_str("[safety]\nonly_apps = [\"com.apple.Preferences\"]\ndnd = false\n").unwrap();
        assert!(c.dnd.focus.is_none());
        assert_eq!(c.guard, GuardConfig::default());
    }

    #[test]
    fn guard_defaults_ask_a_human() {
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c.guard.mode, GuardMode::Human);
        assert_eq!((c.guard.confirm_timeout_secs, c.guard.deny_cooldown_secs), (45, 30));
        assert_eq!(c.guard.categories, GuardCategory::DEFAULT.to_vec());
        assert!(c.guard.only_apps.is_empty() && c.guard.block_apps.is_empty());
        assert!(c.mcp.screenshot_dir.is_none());
        assert_eq!(Config::default().guard, c.guard);
    }

    #[test]
    fn guard_section_parses_strictly() {
        let c: Config = toml::from_str(
            "[guard]\nmode = \"deny\"\ncategories = [\"money\", \"send\"]\nadd = [\"기부\"]\nconfirm_timeout_secs = 500\n",
        )
        .unwrap();
        assert_eq!(c.guard.mode, GuardMode::Deny);
        assert_eq!(c.guard.categories, vec![GuardCategory::Money, GuardCategory::Send]);
        assert_eq!(c.guard.add, vec!["기부".to_string()]);
        assert_eq!(c.guard.confirm_timeout(), std::time::Duration::from_secs(90));
        assert_eq!(c.guard.deny_cooldown_secs, 30);
        // Fail closed: a bad mode, category or key is an error, not a default.
        assert!(toml::from_str::<Config>("[guard]\nmode = \"of\"\n").is_err());
        assert!(toml::from_str::<Config>("[guard]\ncategories = [\"cash\"]\n").is_err());
        assert!(toml::from_str::<Config>("[guard]\nmdoe = \"off\"\n").is_err());
        let off: Config = toml::from_str("[guard]\nmode = \"off\"\n").unwrap();
        assert_eq!(off.guard.mode, GuardMode::Off);
        let low: Config = toml::from_str("[guard]\nconfirm_timeout_secs = 1\n").unwrap();
        assert_eq!(low.guard.confirm_timeout(), std::time::Duration::from_secs(10));
    }

    #[test]
    fn mcp_screenshot_dir() {
        let c: Config = toml::from_str("[mcp]\nscreenshot_dir = \"/tmp/shots\"\n").unwrap();
        assert_eq!(c.mcp.screenshot_dir.as_deref(), Some(std::path::Path::new("/tmp/shots")));
    }
}
