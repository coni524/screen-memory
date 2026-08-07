//! Entry point. Dispatches subcommands: the tray app (the default), headless, and login.

mod agent;
mod auth;
mod capture;
mod config;
mod dedupe;
mod exclude;
mod i18n;
mod local;
mod oauth;
mod ocr;
mod rt;
mod settings;
mod spool;
mod tray;
mod uploader;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::Rotation;

use crate::agent::Agent;
use crate::auth::CognitoAuth;
use crate::config::{Config, state_dir};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

fn setup_logging() -> Result<WorkerGuard> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir)?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("agent")
        .filename_suffix("log")
        .max_log_files(8)
        .build(&dir)
        .context("cannot open the log file")?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_target(false)
        .with_current_span(false)
        .with_span_list(false)
        .with_ansi(false)
        .with_writer(writer)
        .init();
    Ok(guard)
}

/// Builds the Cognito auth once the AWS settings are complete. Local-only mode does not need it.
fn make_auth(cfg: &Config) -> Result<Arc<CognitoAuth>> {
    let aws = cfg.aws()?;
    Ok(Arc::new(CognitoAuth::new(
        &aws.region,
        &aws.user_pool_id,
        &aws.user_pool_client_id,
        &aws.identity_pool_id,
        state_dir().join("auth.json"),
    )))
}

/// Runs only the browser login, then exits.
fn cmd_login(cfg: &Config) -> Result<()> {
    let aws = cfg.aws()?;
    let domain = cfg.cognito_domain.as_deref().context(
        "config.toml has no cognito_domain (the Hosted UI base URL), so browser login is not possible",
    )?;
    let tokens = oauth::browser_login(domain, &aws.user_pool_client_id, LOGIN_TIMEOUT)?;
    make_auth(cfg)?.store_refresh_token(&tokens.refresh_token, Some(&tokens.id_token))?;
    println!("[screen-memory] Logged in. The refresh token has been saved");
    Ok(())
}

/// Prevents a second instance from running. Automatic startup (launchd / Task Scheduler)
/// and manual startup (a double-click or a terminal) can overlap, so hold a lock file in
/// the state directory as the marker. Once the process ends its handle closes and the
/// lock releases on its own.
/// Returns Ok(None) when another instance is already running. That is deliberately not an
/// error, because launchd reads a non-zero exit as a signal to restart the job
/// (KeepAlive's SuccessfulExit: false).
#[cfg(windows)]
fn acquire_single_instance() -> Result<Option<std::fs::File>> {
    use std::os::windows::fs::OpenOptionsExt;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    let dir = state_dir();
    std::fs::create_dir_all(&dir)?;
    // Only opening it with sharing disabled (share_mode 0) matters; the contents go unused
    match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(0)
        .open(dir.join("agent.lock"))
    {
        Ok(file) => Ok(Some(file)),
        Err(e) if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => Ok(None),
        Err(e) => Err(e).context("cannot open the lock file"),
    }
}

/// A second instance can start on macOS too: launchd only prevents duplicates among the
/// jobs it manages, and a Finder double-click (LaunchServices) starts the app by another
/// route. Here the marker is an flock (an advisory lock) held for the process's lifetime.
#[cfg(not(windows))]
fn acquire_single_instance() -> Result<Option<std::fs::File>> {
    use std::os::fd::AsRawFd;
    let dir = state_dir();
    std::fs::create_dir_all(&dir)?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false) // The contents go unused; only acquiring the lock matters
        .open(dir.join("agent.lock"))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(Some(file));
    }
    let errno = std::io::Error::last_os_error();
    if errno.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(None)
    } else {
        Err(errno).context("cannot lock the lock file")
    }
}

/// Runs the capture loop headless, for manual checks.
fn run_headless(cfg: Config) -> Result<()> {
    let mode = config::load_mode();
    let auth = if mode.uploads() {
        let auth = make_auth(&cfg)?;
        if !auth.has_login() {
            anyhow::bail!("not logged in; run `screen-memory login` first");
        }
        // Confirm at startup that the tokens still work
        auth.aws_credentials().context("authentication error")?;
        Some(auth)
    } else {
        None
    };
    println!(
        "[screen-memory] Started (device={}, mode={}, destination={})",
        cfg.device_id,
        mode.as_str(),
        if mode.uploads() {
            cfg.bucket.clone().unwrap_or_default()
        } else {
            crate::local::local_dir().display().to_string()
        }
    );
    let _agent = Agent::start(cfg, auth, mode)?;
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// Opens only the settings window and returns once it is closed. The tray app reaches the same
/// window through its menu; this is the way in when the tray is not running, and it is what makes
/// the window's appearance straightforward to check while working on it.
#[cfg(windows)]
fn cmd_settings() -> Result<()> {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
    };

    let (tx, _rx) = std::sync::mpsc::channel();
    let mut window = settings::Window::open(tx)?;
    while window.is_open() {
        let mut msg = MSG::default();
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            if settings::is_dialog_message(&msg) {
                continue;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        window.poll();
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn run() -> Result<()> {
    let cfg = Config::load(None).context("configuration error")?;
    let command = std::env::args().nth(1).unwrap_or_default();
    match command.as_str() {
        "login" => cmd_login(&cfg),
        #[cfg(windows)]
        "settings" => cmd_settings(),
        // No argument means the tray app. A double-click in Finder or Explorer starts the
        // binary with no arguments, and that must not bring up an invisible headless run.
        // Two names are accepted to match what each OS calls it: menu bar on macOS, tray on Windows
        "" | "menubar" | "tray" => {
            let Some(_lock) = acquire_single_instance()? else {
                println!(
                    "[screen-memory] Another screen-memory is already running, so this one exits"
                );
                return Ok(());
            };
            let _guard = setup_logging()?;
            // The tray opens even with incomplete AWS settings, since the settings screen
            // and local-only mode still work
            let auth = make_auth(&cfg).ok();
            tray::run(cfg, auth)
        }
        "headless" => {
            let Some(_lock) = acquire_single_instance()? else {
                println!(
                    "[screen-memory] Another screen-memory is already running, so this one exits"
                );
                return Ok(());
            };
            let _guard = setup_logging()?;
            run_headless(cfg)
        }
        other => {
            anyhow::bail!(
                "unknown subcommand: {other} (login / settings / menubar / tray / headless / no argument = tray)"
            )
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("[screen-memory] {e:#}");
            ExitCode::FAILURE
        }
    }
}
