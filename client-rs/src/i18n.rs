//! UI language selection (Japanese / English).
//!
//! The language is decided once at startup and cached: the preference stored from the
//! settings window wins, then the `SCREEN_MEMORY_LANG=ja|en` environment variable
//! (handy for testing), then the OS preference. Call sites carry both languages
//! inline via `t()`, so there is no central table.
//!
//! The preference lives in its own file in the state directory (like the mode file),
//! because config.toml uses deny_unknown_fields and a new key there would stop older
//! binaries from starting. A change takes effect on the next launch, since the tray
//! menu is built once at startup.

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Ja,
    En,
}

/// The stored preference: follow the OS, or force one language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pref {
    Auto,
    Ja,
    En,
}

impl Pref {
    /// Index in the settings window's selector, in `settings::language_options` order.
    pub fn index(self) -> usize {
        match self {
            Pref::Auto => 0,
            Pref::Ja => 1,
            Pref::En => 2,
        }
    }

    pub fn from_index(index: usize) -> Pref {
        match index {
            1 => Pref::Ja,
            2 => Pref::En,
            _ => Pref::Auto,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Pref::Auto => "auto",
            Pref::Ja => "ja",
            Pref::En => "en",
        }
    }
}

fn pref_path() -> std::path::PathBuf {
    crate::config::state_dir().join("lang")
}

pub fn load_pref() -> Pref {
    match std::fs::read_to_string(pref_path()) {
        Ok(s) => match s.trim_start_matches('\u{feff}').trim() {
            "ja" => Pref::Ja,
            "en" => Pref::En,
            _ => Pref::Auto,
        },
        Err(_) => Pref::Auto,
    }
}

pub fn store_pref(pref: Pref) {
    let path = pref_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, pref.as_str()) {
        tracing::warn!(event = "lang_store_error", error = %e);
    }
}

pub fn lang() -> Lang {
    static LANG: OnceLock<Lang> = OnceLock::new();
    *LANG.get_or_init(detect)
}

/// Picks the string for the current UI language.
pub fn t(en: &'static str, ja: &'static str) -> &'static str {
    match lang() {
        Lang::Ja => ja,
        Lang::En => en,
    }
}

fn detect() -> Lang {
    match load_pref() {
        Pref::Ja => return Lang::Ja,
        Pref::En => return Lang::En,
        Pref::Auto => {}
    }
    if let Ok(v) = std::env::var("SCREEN_MEMORY_LANG") {
        if v.starts_with("ja") {
            return Lang::Ja;
        }
        if v.starts_with("en") {
            return Lang::En;
        }
    }
    if os_prefers_japanese() {
        Lang::Ja
    } else {
        Lang::En
    }
}

#[cfg(target_os = "macos")]
fn os_prefers_japanese() -> bool {
    // The first entry of preferredLanguages is the UI language ("ja-JP" and the like).
    // The LANG environment variable is absent under launchd, so it is only a fallback.
    let preferred = objc2_foundation::NSLocale::preferredLanguages();
    if let Some(first) = preferred.iter().next() {
        return first.to_string().starts_with("ja");
    }
    env_prefers_japanese()
}

#[cfg(target_os = "windows")]
fn os_prefers_japanese() -> bool {
    // Primary language ID 0x11 is Japanese
    let langid = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    (langid & 0x3ff) == 0x11
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn os_prefers_japanese() -> bool {
    env_prefers_japanese()
}

#[allow(dead_code)]
fn env_prefers_japanese() -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .next()
        .is_some_and(|v| v.starts_with("ja"))
}
