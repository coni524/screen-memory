//! Tray residency (menu bar on macOS, notification area on Windows). Both platforms show the
//! same menu.
//!
//! Instead of winit / tao, we drive the OS event pump by hand on the main thread and let
//! tray-icon do nothing more than create the NSStatusItem / notification-area icon on top of
//! it. The capture loop stays on its own thread in agent.rs; state (paused, permission, last
//! capture time) is shared through atomics, and only the browser login result and the settings
//! window's save notification arrive over mpsc channels.
//!
//! Switching modes (image / OCR / local-only) is done by rebuilding the agent. Local-only mode
//! never starts the uploader thread at all, so flipping a flag at runtime could not guarantee
//! "never talks to AWS".
//!
//! The only per-OS differences are the event pump and how the visual state is switched, so just
//! those live in `platform` (macOS: the menu bar icon; Windows: the notification area icon and
//! the tooltip). Both draw their shapes out of `art`, which also defines the app icon artwork.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tray_icon::TrayIcon;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};

use crate::agent::Agent;
use crate::auth::CognitoAuth;
use crate::config::{Config, Mode, load_mode, store_mode};
use crate::i18n::t;
use crate::local::local_dir;
use crate::oauth::{self, Tokens};
use crate::settings;

fn aws_incomplete() -> &'static str {
    t(
        "AWS settings are incomplete (enter them via Open Settings)",
        "AWS 設定が未完成（「設定を開く」で入力する）",
    )
}

fn pause_label() -> &'static str {
    t("Pause", "一時停止")
}

fn starting_label() -> &'static str {
    t("Starting…", "起動中…")
}

fn login_failed(e: &anyhow::Error) -> String {
    format!("{}: {e:#}", t("Login failed", "ログイン失敗"))
}

pub(crate) mod art;

#[cfg(target_os = "macos")]
#[path = "darwin.rs"]
mod platform;

#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod platform;

/// How long one turn of the event pump waits. This bounds the latency of menu interactions.
const PUMP_INTERVAL: f64 = 0.5;
/// How often the status line (last capture time) is rewritten.
const STATUS_INTERVAL: Duration = Duration::from_secs(30);

struct App {
    cfg: Config,
    /// None while the AWS settings are incomplete. Rebuilt on every settings save.
    auth: Option<Arc<CognitoAuth>>,
    mode: Mode,
    agent: Option<Agent>,
    tray: TrayIcon,
    status_item: MenuItem,
    login_item: MenuItem,
    pause_item: MenuItem,
    mode_image_item: CheckMenuItem,
    mode_ocr_item: CheckMenuItem,
    mode_local_item: CheckMenuItem,
    /// Some only while a browser login is in flight.
    login_rx: Option<mpsc::Receiver<Result<Tokens>>>,
    /// The settings window. Some only while it is open (the loop drops it once closed).
    settings_window: Option<settings::Window>,
    /// Save notifications from the settings window.
    saved_rx: Option<mpsc::Receiver<()>>,
    /// The text last written to status_item; skip the rewrite if it is unchanged.
    last_status: String,
}

impl App {
    fn set_status(&mut self, text: &str) {
        if self.last_status != text {
            self.status_item.set_text(text);
            platform::set_status_hint(&self.tray, text);
            self.last_status = text.to_string();
        }
    }

    /// Start the agent in the current mode. In modes that upload to the cloud, check that the
    /// tokens are still valid first.
    fn start_agent(&mut self) {
        let auth = if self.mode.uploads() {
            let Some(auth) = self.auth.clone() else {
                self.set_status(aws_incomplete());
                return;
            };
            if let Err(e) = auth.aws_credentials() {
                self.set_status(&format!("{}: {e:#}", t("Login required", "要ログイン")));
                self.login_item.set_enabled(true);
                return;
            }
            self.login_item.set_enabled(false); // already logged in, so grey the item out
            Some(auth)
        } else {
            None
        };
        match Agent::start(self.cfg.clone(), auth, self.mode) {
            Ok(agent) => {
                self.agent = Some(agent);
                self.pause_item.set_enabled(true);
                self.pause_item.set_text(pause_label());
                platform::set_running(&self.tray, true);
                self.update_status();
            }
            Err(e) => {
                tracing::warn!(event = "agent_start_error", error = %format!("{e:#}"));
                self.set_status(&format!("{}: {e:#}", t("Failed to start", "起動失敗")));
            }
        }
    }

    fn stop_agent(&mut self) {
        if let Some(mut agent) = self.agent.take() {
            agent.stop();
        }
        self.pause_item.set_enabled(false);
    }

    fn update_status(&mut self) {
        let Some(agent) = &self.agent else {
            return;
        };
        if agent.paused() {
            self.set_status(t("Paused", "一時停止中"));
            return;
        }
        // Windows has no screen capture permission model, so this only ever trips on macOS
        if agent.permission_denied() {
            self.set_status(t(
                "Screen Recording permission missing (allow it in System Settings, then restart)",
                "画面収録の許可がない（システム設定で許可して再起動）",
            ));
            return;
        }
        let prefix = match agent.mode() {
            Mode::Local => t("Running (local only)", "ローカル保存で稼働中"),
            Mode::Image | Mode::Ocr => t("Running", "稼働中"),
        };
        let suffix = match agent.last_captured_at() {
            Some(at) => t(" (last capture {time})", "（直近の撮影 {time}）")
                .replace("{time}", &at.format("%H:%M").to_string()),
            None => t(" (waiting for first capture)", "（撮影待ち）").to_string(),
        };
        let text = format!("{prefix}{suffix}");
        self.set_status(&text);
    }

    fn on_login(&mut self) {
        let Some(client_id) = self.cfg.user_pool_client_id.clone() else {
            self.set_status(aws_incomplete());
            return;
        };
        let Some(domain) = self.cfg.cognito_domain.clone() else {
            tracing::warn!(event = "login_error", error = "cognito_domain is not set");
            self.set_status(t(
                "cognito_domain is not set (enter it via Open Settings)",
                "cognito_domain が未設定（「設定を開く」で入力する）",
            ));
            return;
        };
        self.login_item.set_enabled(false); // prevent kicking off a second login
        self.set_status(t("Logging in via the browser…", "ブラウザでログイン中…"));
        let (tx, rx) = mpsc::channel();
        self.login_rx = Some(rx);
        std::thread::Builder::new()
            .name("login".into())
            .spawn(move || {
                let _ = tx.send(oauth::browser_login(
                    &domain,
                    &client_id,
                    crate::LOGIN_TIMEOUT,
                ));
            })
            .expect("cannot start the login thread");
    }

    /// Take in the result from the login thread.
    fn poll_login(&mut self) {
        let Some(rx) = &self.login_rx else { return };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.login_rx = None;
                self.login_item.set_enabled(true);
                return;
            }
        };
        self.login_rx = None;
        let Some(auth) = self.auth.clone() else {
            self.login_item.set_enabled(true);
            self.set_status(aws_incomplete());
            return;
        };
        match result {
            Ok(tokens) => {
                if let Err(e) =
                    auth.store_refresh_token(&tokens.refresh_token, Some(&tokens.id_token))
                {
                    self.set_status(&login_failed(&e));
                    self.login_item.set_enabled(true);
                    return;
                }
                if self.mode.uploads() {
                    self.stop_agent();
                    self.start_agent();
                } else {
                    self.set_status(t(
                        "Logged in (still in local-only mode)",
                        "ログインした（ローカル保存モードのまま）",
                    ));
                    self.login_item.set_enabled(true);
                }
            }
            Err(e) => {
                tracing::warn!(event = "login_error", error = %format!("{e:#}"));
                self.set_status(&login_failed(&e));
                self.login_item.set_enabled(true);
            }
        }
    }

    /// Switch modes. The agent is rebuilt, because local-only mode never starts the uploader
    /// and so cannot be reached by flipping a flag.
    fn set_mode(&mut self, mode: Mode) {
        self.sync_mode_checks(mode);
        if mode == self.mode {
            return;
        }
        self.mode = mode;
        store_mode(mode);
        tracing::info!(event = "mode_changed", mode = mode.as_str());
        self.stop_agent();
        self.start_agent();
    }

    /// A CheckMenuItem toggles its own `checked` on click, so rewrite all three items together.
    fn sync_mode_checks(&self, mode: Mode) {
        self.mode_image_item.set_checked(mode == Mode::Image);
        self.mode_ocr_item.set_checked(mode == Mode::Ocr);
        self.mode_local_item.set_checked(mode == Mode::Local);
    }

    fn on_pause(&mut self) {
        let Some(agent) = &self.agent else { return };
        if agent.paused() {
            agent.resume();
            self.pause_item.set_text(pause_label());
            platform::set_running(&self.tray, true);
        } else {
            agent.pause();
            self.pause_item.set_text(t("Resume", "再開"));
            platform::set_running(&self.tray, false);
        }
        self.update_status();
    }

    /// Open the local storage directory in Finder / Explorer.
    fn on_open_store(&mut self) {
        let dir = local_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.set_status(&format!(
                "{}: {e}",
                t("Cannot create the data folder", "保存先を作れない")
            ));
            return;
        }
        if let Err(e) = open_in_file_manager(&dir) {
            self.set_status(&format!(
                "{}: {e:#}",
                t("Cannot open the data folder", "保存先を開けない")
            ));
        }
    }

    /// Open the settings window, or just bring it to the front if it is already open.
    fn on_settings(&mut self) {
        if let Some(window) = &self.settings_window {
            if window.is_open() {
                window.focus();
                return;
            }
            self.settings_window = None;
        }
        let (tx, rx) = mpsc::channel();
        match settings::Window::open(tx) {
            Ok(window) => {
                self.settings_window = Some(window);
                self.saved_rx = Some(rx);
            }
            Err(e) => {
                tracing::warn!(event = "settings_error", error = %format!("{e:#}"));
                self.set_status(&format!(
                    "{}: {e:#}",
                    t("Cannot open the settings window", "設定画面を開けない")
                ));
            }
        }
    }

    /// Push the settings window's connection check results into the UI, and drop the window if
    /// it has been closed.
    fn poll_settings(&mut self) {
        let Some(window) = &mut self.settings_window else {
            return;
        };
        window.poll();
        if !window.is_open() {
            self.settings_window = None;
        }
    }

    /// After a save in the settings window, rebuild the config and auth, then restart the agent.
    fn poll_saved(&mut self) {
        let Some(rx) = &self.saved_rx else { return };
        let mut saved = false;
        while rx.try_recv().is_ok() {
            saved = true;
        }
        if !saved {
            return;
        }
        match Config::load(None) {
            Ok(cfg) => {
                self.cfg = cfg;
                self.auth = crate::make_auth(&self.cfg).ok();
                if self.auth.is_some() {
                    self.login_item.set_enabled(true);
                }
                tracing::info!(event = "config_reloaded");
                self.stop_agent();
                self.start_agent();
            }
            Err(e) => self.set_status(&format!(
                "{}: {e:#}",
                t("Failed to reload the settings", "設定の再読込に失敗")
            )),
        }
    }
}

fn open_in_file_manager(dir: &std::path::Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("open")
            .arg(dir)
            .status()
            .context("failed to run the open command")?;
        anyhow::ensure!(status.success(), "open failed (exit status {status})");
        Ok(())
    }
    #[cfg(target_os = "windows")]
    {
        // explorer can return 1 even on success, so don't look at the exit code
        std::process::Command::new("explorer")
            .arg(dir)
            .spawn()
            .context("failed to launch explorer")?;
        Ok(())
    }
}

pub fn run(cfg: Config, auth: Option<Arc<CognitoAuth>>) -> Result<()> {
    let ui = platform::init()?;
    let mode = load_mode();

    let status_item = MenuItem::with_id("status", starting_label(), false, None);
    let login_item = MenuItem::with_id("login", t("Login", "ログイン"), true, None);
    let pause_item = MenuItem::with_id("pause", pause_label(), false, None);
    let mode_image_item = CheckMenuItem::with_id(
        "mode_image",
        t("AWS analysis (send images)", "AWS解析（画像を送信）"),
        true,
        false,
        None,
    );
    let mode_ocr_item = CheckMenuItem::with_id(
        "mode_ocr",
        t("AWS analysis (send text)", "AWS解析（文字を送信）"),
        true,
        false,
        None,
    );
    let mode_local_item = CheckMenuItem::with_id(
        "mode_local",
        t("Local only (no upload)", "ローカル保存（送信なし）"),
        true,
        false,
        None,
    );
    let open_store_item = MenuItem::with_id(
        "open_store",
        t("Open local data folder", "ローカル保存先を開く"),
        true,
        None,
    );
    let settings_item =
        MenuItem::with_id("settings", t("Open Settings…", "設定を開く…"), true, None);
    let quit_item = MenuItem::with_id("quit", t("Quit", "終了"), true, None);
    let menu = Menu::new();
    menu.append_items(&[
        &status_item,
        &PredefinedMenuItem::separator(),
        &login_item,
        &pause_item,
        &PredefinedMenuItem::separator(),
        &mode_image_item,
        &mode_ocr_item,
        &mode_local_item,
        &PredefinedMenuItem::separator(),
        &open_store_item,
        &settings_item,
        &PredefinedMenuItem::separator(),
        &quit_item,
    ])?;
    let tray = platform::build_tray(menu)?;

    let mut state = App {
        cfg,
        auth,
        mode,
        agent: None,
        tray,
        status_item,
        login_item,
        pause_item,
        mode_image_item,
        mode_ocr_item,
        mode_local_item,
        login_rx: None,
        settings_window: None,
        saved_rx: None,
        last_status: starting_label().to_string(),
    };
    state.sync_mode_checks(mode);
    tracing::info!(event = "tray_started", mode = mode.as_str());
    if !state.mode.uploads() || state.auth.as_ref().is_some_and(|a| a.has_login()) {
        state.start_agent();
    } else if state.auth.is_none() {
        state.set_status(aws_incomplete());
    } else {
        state.set_status(t("Not logged in", "未ログイン"));
    }

    let mut status_refreshed = Instant::now();
    loop {
        ui.pump(PUMP_INTERVAL);
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            match event.id.0.as_str() {
                "login" => state.on_login(),
                "pause" => state.on_pause(),
                "mode_image" => state.set_mode(Mode::Image),
                "mode_ocr" => state.set_mode(Mode::Ocr),
                "mode_local" => state.set_mode(Mode::Local),
                "open_store" => state.on_open_store(),
                "settings" => state.on_settings(),
                "quit" => {
                    state.stop_agent();
                    tracing::info!(event = "tray_stopped");
                    return Ok(());
                }
                _ => {}
            }
        }
        state.poll_login();
        state.poll_settings();
        state.poll_saved();
        if status_refreshed.elapsed() >= STATUS_INTERVAL {
            state.update_status();
            status_refreshed = Instant::now();
        }
    }
}
