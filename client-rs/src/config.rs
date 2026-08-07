//! Loading and validation of the configuration file (config.toml).

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub fn default_config_path() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(std::env::var("APPDATA").expect("APPDATA is not set"))
            .join("screen-memory")
            .join("config.toml")
    } else {
        home_dir()
            .join(".config")
            .join("screen-memory")
            .join("config.toml")
    }
}

pub fn state_dir() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA is not set"))
            .join("screen-memory")
    } else {
        home_dir()
            .join(".local")
            .join("state")
            .join("screen-memory")
    }
}

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is not set"))
}

/// Operating mode. The tray menu switches it, which restarts the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Upload the image (WebP) and the metadata JSON to S3
    Image,
    /// Run OCR on this machine and upload only the metadata JSON, text included, to S3
    Ocr,
    /// Send nothing to AWS; keep the OCR text in a local JSONL file
    Local,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Image => "image",
            Mode::Ocr => "ocr",
            Mode::Local => "local",
        }
    }

    /// Whether this mode sends anything to AWS.
    pub fn uploads(self) -> bool {
        !matches!(self, Mode::Local)
    }
}

/// File that persists the mode. It lives in the state directory because config.toml
/// uses deny_unknown_fields, so adding a key there would stop older binaries from starting.
fn mode_path() -> PathBuf {
    state_dir().join("mode")
}

/// Reads the stored mode. Falls back to image mode when the file is missing or unreadable.
pub fn load_mode() -> Mode {
    std::fs::read_to_string(mode_path())
        .map(|s| parse_mode(&s))
        .unwrap_or(Mode::Image)
}

/// People edit this file by hand, so tolerate a trailing newline and a BOM.
/// PowerShell's `Set-Content -Encoding utf8` writes a BOM, and Rust's trim
/// does not strip U+FEFF.
fn parse_mode(s: &str) -> Mode {
    match s.trim_start_matches('\u{feff}').trim() {
        "ocr" => Mode::Ocr,
        "local" => Mode::Local,
        _ => Mode::Image,
    }
}

pub fn store_mode(mode: Mode) {
    let path = mode_path();
    let _ = std::fs::create_dir_all(path.parent().expect("state_dir is not the root"));
    if let Err(e) = std::fs::write(&path, mode.as_str()) {
        tracing::warn!(event = "mode_store_error", error = %e);
    }
}

fn default_device_id() -> String {
    "local".to_string()
}

fn default_interval() -> u64 {
    60
}

fn default_idle_threshold() -> u64 {
    300
}

fn default_dhash_threshold() -> u32 {
    5
}

/// The full set of AWS settings the cloud-sending modes need.
/// Someone using local-only mode can omit config.toml entirely, so Config holds
/// these as Options and `Config::aws()` validates them all right before use.
#[derive(Debug, Clone)]
pub struct AwsSettings {
    pub bucket: String,
    pub region: String,
    pub user_pool_id: String,
    pub user_pool_client_id: String,
    pub identity_pool_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_device_id")]
    pub device_id: String,
    pub bucket: Option<String>,
    pub region: Option<String>,
    pub user_pool_id: Option<String>,
    pub user_pool_client_id: Option<String>,
    pub identity_pool_id: Option<String>,
    /// Base URL of the Hosted UI, used for the browser login
    pub cognito_domain: Option<String>,
    /// Accepted for compatibility with older config files, but unused: the loop is pinned to minute boundaries
    #[allow(dead_code)]
    #[serde(default = "default_interval")]
    pub interval_seconds: u64,
    #[serde(default = "default_idle_threshold")]
    pub idle_threshold_seconds: u64,
    #[serde(default = "default_dhash_threshold")]
    pub dhash_threshold: u32,
    #[serde(default)]
    pub exclude_apps: Vec<String>,
    #[serde(default)]
    pub exclude_title_patterns: Vec<String>,
}

impl Config {
    /// Reads the configuration file. A missing file is normal in local-only mode,
    /// so treat it as an all-defaults configuration with no AWS values.
    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let path = path.unwrap_or_else(default_config_path);
        let text = if path.exists() {
            std::fs::read_to_string(&path)
                .with_context(|| format!("cannot read the config file: {}", path.display()))?
        } else {
            String::new()
        };
        let cfg: Config =
            toml::from_str(&text).with_context(|| "the config file is broken TOML")?;
        if !device_id_valid(&cfg.device_id) {
            bail!(
                "device_id must match ^[a-z0-9-]{{1,32}}$: {:?}",
                cfg.device_id
            );
        }
        Ok(cfg)
    }

    /// Extracts, with validation, the AWS settings the cloud-sending modes need.
    pub fn aws(&self) -> Result<AwsSettings> {
        let missing: Vec<&str> = [
            ("bucket", &self.bucket),
            ("region", &self.region),
            ("user_pool_id", &self.user_pool_id),
            ("user_pool_client_id", &self.user_pool_client_id),
            ("identity_pool_id", &self.identity_pool_id),
        ]
        .iter()
        .filter(|(_, v)| v.as_deref().is_none_or(str::is_empty))
        .map(|(k, _)| *k)
        .collect();
        if !missing.is_empty() {
            bail!(
                "{}: {}",
                crate::i18n::t(
                    "AWS settings are missing from config.toml (enter them via Open Settings in the menu)",
                    "config.toml に AWS の設定が足りない（メニューの「設定を開く」で入力できる）",
                ),
                missing.join(", ")
            );
        }
        Ok(AwsSettings {
            bucket: self.bucket.clone().expect("validated above"),
            region: self.region.clone().expect("validated above"),
            user_pool_id: self.user_pool_id.clone().expect("validated above"),
            user_pool_client_id: self.user_pool_client_id.clone().expect("validated above"),
            identity_pool_id: self.identity_pool_id.clone().expect("validated above"),
        })
    }
}

pub fn device_id_valid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config> {
        // Tests run in parallel, so give each one a unique file name
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sm-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("config-{seq}.toml"));
        std::fs::write(&path, text).unwrap();
        Config::load(Some(path))
    }

    const MINIMAL: &str = r#"
device_id = "mac-main"
bucket = "b"
region = "ap-northeast-1"
user_pool_id = "p"
user_pool_client_id = "c"
identity_pool_id = "i"
"#;

    #[test]
    fn minimal_config_has_defaults() {
        let cfg = parse(MINIMAL).unwrap();
        assert_eq!(cfg.interval_seconds, 60);
        assert_eq!(cfg.idle_threshold_seconds, 300);
        assert_eq!(cfg.dhash_threshold, 5);
        assert!(cfg.exclude_apps.is_empty());
        assert!(cfg.cognito_domain.is_none());
        assert!(cfg.aws().is_ok());
    }

    #[test]
    fn missing_file_is_local_only_default() {
        let cfg =
            Config::load(Some(std::env::temp_dir().join("sm-config-not-exist.toml"))).unwrap();
        assert_eq!(cfg.device_id, "local");
        let err = format!("{:#}", cfg.aws().unwrap_err());
        assert!(err.contains("bucket"), "{err}");
        assert!(err.contains("identity_pool_id"), "{err}");
    }

    #[test]
    fn empty_aws_value_counts_as_missing() {
        let text = MINIMAL.replace("bucket = \"b\"", "bucket = \"\"");
        let err = format!("{:#}", parse(&text).unwrap().aws().unwrap_err());
        assert!(err.contains("bucket"), "{err}");
        assert!(!err.contains("region"), "{err}");
    }

    #[test]
    fn unknown_key_is_error() {
        let text = format!("{MINIMAL}\nunknown_key = 1\n");
        assert!(parse(&text).is_err());
    }

    #[test]
    fn mode_allows_bom_and_newline() {
        assert_eq!(parse_mode("ocr"), Mode::Ocr);
        assert_eq!(parse_mode("\u{feff}ocr"), Mode::Ocr);
        assert_eq!(parse_mode("ocr\r\n"), Mode::Ocr);
        assert_eq!(parse_mode("local"), Mode::Local);
        assert_eq!(parse_mode("\u{feff}local\r\n"), Mode::Local);
        assert_eq!(parse_mode("image"), Mode::Image);
        assert_eq!(parse_mode(""), Mode::Image);
        assert_eq!(parse_mode("なにか別の文字"), Mode::Image);
    }

    #[test]
    fn invalid_device_id_is_error() {
        let text = MINIMAL.replace("mac-main", "Mac_Main");
        assert!(parse(&text).is_err());
    }
}
