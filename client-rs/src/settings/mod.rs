//! The settings screen. The tray's "open settings" item brings up an OS-native window (AppKit
//! on macOS, Win32 directly on Windows — chosen so we don't have to pull in a GUI toolkit).
//!
//! - This file holds only the logic (field definitions, saving, the connection check); building
//!   and driving the window lives in darwin.rs / windows.rs.
//! - Saving goes through toml_edit so hand-written comments and unknown keys in config.toml
//!   survive.
//! - The connection check uses the values currently typed into the form: authenticate with
//!   Cognito, then do a test PUT to S3. The client's only IAM permission is PutObject under
//!   raw/*, so the check writes there too. The key must not end in .json, because the analysis
//!   Lambda subscribes to put events for .json objects.
//! - The connection check calls AWS and takes a few seconds, so it runs on its own thread and
//!   reports back over an mpsc channel. The tray's main loop calls `Window::poll()` every turn
//!   and puts whatever arrived on screen.

use std::sync::mpsc;

use anyhow::{Context, Result, bail};

use crate::auth::CognitoAuth;
use crate::config::{Config, default_config_path, device_id_valid, state_dir};
use crate::i18n::{self, t};
use crate::rt::runtime;

#[cfg(target_os = "macos")]
#[path = "darwin.rs"]
mod platform;

#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod platform;

pub use platform::Window;
#[cfg(target_os = "windows")]
pub use platform::is_dialog_message;

/// One field of the form. `key` must match the top-level key in config.toml.
pub(crate) struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub hint: &'static str,
}

pub(crate) const FIELD_COUNT: usize = 7;

/// The labels go through `t()`, so this is a function rather than a const.
pub(crate) fn fields() -> [Field; FIELD_COUNT] {
    [
        Field {
            key: "device_id",
            label: t("Device ID", "デバイス ID"),
            hint: t(
                "e.g. mac-main (lowercase letters, digits, hyphens)",
                "例: mac-main（英小文字・数字・ハイフン）",
            ),
        },
        Field {
            key: "bucket",
            label: t("S3 bucket name", "S3 バケット名"),
            hint: t("BucketName from the CDK outputs", "CDK 出力の BucketName"),
        },
        Field {
            key: "region",
            label: t("Region", "リージョン"),
            hint: t("e.g. ap-northeast-1", "例: ap-northeast-1"),
        },
        Field {
            key: "user_pool_id",
            label: t("Cognito user pool ID", "Cognito ユーザープール ID"),
            hint: t("UserPoolId from the CDK outputs", "CDK 出力の UserPoolId"),
        },
        Field {
            key: "user_pool_client_id",
            label: t("App client ID", "アプリクライアント ID"),
            hint: t(
                "UserPoolClientId from the CDK outputs",
                "CDK 出力の UserPoolClientId",
            ),
        },
        Field {
            key: "identity_pool_id",
            label: t("Identity pool ID", "ID プール ID"),
            hint: t(
                "IdentityPoolId from the CDK outputs",
                "CDK 出力の IdentityPoolId",
            ),
        },
        Field {
            key: "cognito_domain",
            label: t("Cognito domain", "Cognito ドメイン"),
            hint: t(
                "CognitoDomain from the CDK outputs (used for browser login)",
                "CDK 出力の CognitoDomain（ブラウザログインに使用）",
            ),
        },
    ]
}

pub(crate) fn window_title() -> &'static str {
    t("Screen Memory Settings", "Screen Memory 設定")
}

pub(crate) fn intro() -> &'static str {
    t(
        "Enter the values from the CDK deploy outputs.",
        "AWS 側の値（CDK デプロイの出力）を入れる。",
    )
}

pub(crate) fn broken_note() -> &'static str {
    t(
        "config.toml could not be read, so the form is shown empty. Fix the existing file before saving.",
        "config.toml を読めなかったため空欄で表示している。保存は既存ファイルを直してから行う。",
    )
}

pub(crate) fn testing_message() -> &'static str {
    t("Running…", "実行中…")
}

/// Message after a save. The language choice only takes effect on the next
/// launch, so say so when it changed.
pub(crate) fn saved_message(lang_changed: bool) -> String {
    let base = t(
        "✅ Saved. The tray app has picked it up",
        "✅ 保存した。常駐アプリに反映された",
    );
    if lang_changed {
        format!(
            "{base}{}",
            t(
                " (the language applies after restarting the app)",
                "（言語は再起動後に反映される）"
            )
        )
    } else {
        base.to_string()
    }
}

/// Label and choices of the language selector. The order matches `i18n::Pref::from_index`.
pub(crate) fn language_label() -> &'static str {
    t("Language", "言語")
}

pub(crate) fn language_options() -> [&'static str; 3] {
    [
        t("Auto (OS setting)", "自動（OS の設定）"),
        "日本語",
        "English",
    ]
}

/// Store the selector index and report whether it changed (the caller mentions the restart).
pub(crate) fn save_language(index: usize) -> bool {
    let pref = i18n::Pref::from_index(index);
    if pref == i18n::load_pref() {
        return false;
    }
    i18n::store_pref(pref);
    true
}

pub(crate) fn language_index() -> usize {
    i18n::load_pref().index()
}

/// The values in the input boxes, held in the same order as FIELDS.
pub(crate) type Values = Vec<(&'static str, String)>;

fn get<'a>(values: &'a Values, key: &str) -> &'a str {
    values
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
}

/// The current values from config.toml, in FIELDS order. If the file is broken we still want
/// the window to open, so return all-empty values and broken=true (the save path then refuses
/// to overwrite).
pub(crate) fn current_values() -> (Vec<String>, bool) {
    let Some(cfg) = Config::load(None).ok() else {
        return (fields().iter().map(|_| String::new()).collect(), true);
    };
    let value = |key: &str| -> String {
        match key {
            "device_id" => cfg.device_id.clone(),
            "bucket" => cfg.bucket.clone().unwrap_or_default(),
            "region" => cfg.region.clone().unwrap_or_default(),
            "user_pool_id" => cfg.user_pool_id.clone().unwrap_or_default(),
            "user_pool_client_id" => cfg.user_pool_client_id.clone().unwrap_or_default(),
            "identity_pool_id" => cfg.identity_pool_id.clone().unwrap_or_default(),
            "cognito_domain" => cfg.cognito_domain.clone().unwrap_or_default(),
            _ => String::new(),
        }
    };
    (fields().iter().map(|f| value(f.key)).collect(), false)
}

/// Write config.toml from the form values, keeping the existing file's comments and unknown
/// keys. Keys left blank are removed from the file, so optional settings can be reset to None.
pub(crate) fn save(values: &Values) -> Result<()> {
    let device_id = get(values, "device_id").trim();
    if !device_id.is_empty() && !device_id_valid(device_id) {
        bail!(
            "{}: {device_id:?}",
            t(
                "device_id must be lowercase letters, digits, and hyphens, up to 32 chars",
                "device_id は英小文字・数字・ハイフン32文字以内にする"
            )
        );
    }
    let path = default_config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(e).with_context(|| {
                format!(
                    "{}: {}",
                    t("cannot read config.toml", "config.toml を読めない"),
                    path.display()
                )
            });
        }
    };
    let mut doc: toml_edit::DocumentMut = text.parse().context(t(
        "the existing config.toml is broken TOML, so it will not be overwritten; fix or delete it by hand",
        "既存の config.toml が TOML として壊れているので上書きしない。手で直すか削除する",
    ))?;
    for field in fields() {
        let value = get(values, field.key).trim();
        if value.is_empty() {
            doc.remove(field.key);
        } else {
            doc[field.key] = toml_edit::value(value);
        }
    }
    // Before saving, confirm the resulting document still parses as a valid config
    let new_text = doc.to_string();
    toml::from_str::<Config>(&new_text).context(t(
        "these values do not parse as a valid config",
        "この内容では設定として読めない",
    ))?;
    let parent = path
        .parent()
        .context("config path has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, new_text)?;
    std::fs::rename(&tmp, &path)?;
    tracing::info!(event = "config_saved", path = %path.display());
    Ok(())
}

pub(crate) struct Step {
    name: &'static str,
    ok: bool,
    message: String,
}

/// Render the connection check results as one line per step for the result box.
///
/// macOS only: the Windows window draws each step itself, so that the tick and the cross can be
/// painted in the theme's colours instead of being emoji in a run of text.
#[cfg(target_os = "macos")]
pub(crate) fn format_steps(steps: &[Step]) -> String {
    steps
        .iter()
        .map(|s| {
            let mark = if s.ok { "✅" } else { "❌" };
            format!("{mark} {}: {}", s.name, s.message)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Run the connection check on its own thread and return the receiving end for the result.
pub(crate) fn spawn_test(values: Values) -> mpsc::Receiver<Vec<Step>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("settings-test".into())
        .spawn(move || {
            // If the receiver closed the window first, the result is simply dropped
            let _ = tx.send(run_test(&values));
        })
        .expect("failed to spawn the connection-check thread");
    rx
}

/// Verify connectivity using the values currently typed into the form (nothing is saved).
fn run_test(values: &Values) -> Vec<Step> {
    let mut steps = Vec::new();
    let value = |key: &str| get(values, key).trim().to_string();
    let device_id = {
        let v = value("device_id");
        if v.is_empty() { "local".to_string() } else { v }
    };

    // 1. Shape of the configured values
    let missing: Vec<&str> = [
        "bucket",
        "region",
        "user_pool_id",
        "user_pool_client_id",
        "identity_pool_id",
    ]
    .into_iter()
    .filter(|k| value(k).is_empty())
    .collect();
    let format_ok = missing.is_empty() && device_id_valid(&device_id);
    steps.push(Step {
        name: t("Settings check", "設定値の確認"),
        ok: format_ok,
        message: if format_ok {
            t("all required fields are filled", "必須の項目が埋まっている").to_string()
        } else if missing.is_empty() {
            format!(
                "{}: {device_id:?}",
                t("device_id is invalid", "device_id が不正")
            )
        } else {
            format!("{}: {}", t("missing", "未入力"), missing.join(", "))
        },
    });
    if !format_ok {
        return steps;
    }

    // 2. Cognito authentication (the stored refresh token, used against the pool settings
    //    entered in the form)
    let auth = CognitoAuth::new(
        &value("region"),
        &value("user_pool_id"),
        &value("user_pool_client_id"),
        &value("identity_pool_id"),
        state_dir().join("auth.json"),
    );
    if !auth.has_login() {
        steps.push(Step {
            name: t("Cognito authentication", "Cognito 認証"),
            ok: false,
            message: t(
                "not logged in; use Login in the tray menu, then check again",
                "未ログイン。メニューの「ログイン」を済ませてから再チェックする",
            )
            .to_string(),
        });
        return steps;
    }
    let creds = match auth.aws_credentials() {
        Ok(creds) => {
            steps.push(Step {
                name: t("Cognito authentication", "Cognito 認証"),
                ok: true,
                message: t("obtained temporary credentials", "一時認証情報を取得できた")
                    .to_string(),
            });
            creds
        }
        Err(e) => {
            steps.push(Step {
                name: t("Cognito authentication", "Cognito 認証"),
                ok: false,
                message: format!("{e:#}"),
            });
            return steps;
        }
    };

    // 3. Writing to S3
    let key = format!("raw/{device_id}/connection-test.txt");
    let put = runtime().block_on(
        crate::uploader::make_s3(&value("region"), &creds)
            .put_object()
            .bucket(value("bucket"))
            .key(&key)
            .content_type("text/plain")
            .body(aws_sdk_s3::primitives::ByteStream::from_static(
                b"screen-memory connection test",
            ))
            .send(),
    );
    steps.push(match put {
        Ok(_) => Step {
            name: t("S3 write", "S3 への書き込み"),
            ok: true,
            message: t("wrote s3://{path}", "s3://{path} に書けた")
                .replace("{path}", &format!("{}/{key}", value("bucket"))),
        },
        Err(e) => Step {
            name: t("S3 write", "S3 への書き込み"),
            ok: false,
            message: format!("{}", aws_sdk_s3::error::DisplayErrorContext(&e)),
        },
    });
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_returns_value_or_empty() {
        let values: Values = vec![("device_id", "mac-main".to_string())];
        assert_eq!(get(&values, "device_id"), "mac-main");
        assert_eq!(get(&values, "bucket"), "");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn format_steps_marks_ok_and_ng() {
        let steps = vec![
            Step {
                name: "Settings check",
                ok: true,
                message: "all required fields are filled".to_string(),
            },
            Step {
                name: "Cognito authentication",
                ok: false,
                message: "not logged in".to_string(),
            },
        ];
        assert_eq!(
            format_steps(&steps),
            "✅ Settings check: all required fields are filled\n❌ Cognito authentication: not logged in"
        );
    }
}
