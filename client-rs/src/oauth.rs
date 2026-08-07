//! Browser login. Opens the Hosted UI and receives the authorization code on a
//! localhost callback.
//!
//! Authorization code grant with PKCE (RFC 7636). The callback URL is fixed at
//! http://localhost:8765/callback, already registered on the user pool's app client.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const CALLBACK_PORT: u16 = 8765;
pub const CALLBACK_PATH: &str = "/callback";

const DONE_HTML: &str = "<!doctype html><meta charset='utf-8'>\
<title>screen-memory</title>\
<p>Logged in. You can close this window.</p>";

#[derive(Debug, Deserialize)]
pub struct Tokens {
    pub id_token: String,
    pub refresh_token: String,
}

/// Returns (code_verifier, code_challenge), base64url encoded without padding.
pub fn make_pkce_pair() -> (String, String) {
    let verifier = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let digest = Sha256::digest(verifier.as_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(digest);
    (verifier, challenge)
}

/// Has the user log in through the default browser and returns the resulting tokens.
pub fn browser_login(cognito_domain: &str, client_id: &str, timeout: Duration) -> Result<Tokens> {
    let domain = cognito_domain.trim_end_matches('/');
    let (verifier, challenge) = make_pkce_pair();
    let state = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 16]>());
    let redirect_uri = format!("http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}");

    let listener = TcpListener::bind(("127.0.0.1", CALLBACK_PORT))
        .with_context(|| format!("cannot listen on port {CALLBACK_PORT}"))?;
    listener.set_nonblocking(true)?;

    let authorize_url = format!(
        "{domain}/oauth2/authorize?{}",
        urlencode_pairs(&[
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", &redirect_uri),
            ("scope", "openid"),
            ("state", &state),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
        ])
    );
    open_browser(&authorize_url)?;

    let code = wait_for_code(&listener, &state, timeout)?;
    exchange_code(domain, client_id, &code, &redirect_uri, &verifier)
}

/// Waits for the callback and returns the authorization code.
fn wait_for_code(listener: &TcpListener, state: &str, timeout: Duration) -> Result<String> {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(result) = handle_request(stream, state) {
                    return result;
                }
                // Anything other than /callback (favicon and the like) keeps us waiting
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() > deadline {
                    bail!("login timed out");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// Handles one request. Returns Some(result) when it was the callback.
fn handle_request(mut stream: TcpStream, state: &str) -> Option<Result<String>> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).ok()?;
    let request = String::from_utf8_lossy(&buf[..n]).into_owned();
    let target = request.split_whitespace().nth(1)?.to_string();
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));

    if path != CALLBACK_PATH {
        let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
        return None;
    }

    let body = DONE_HTML.as_bytes();
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    let _ = stream.write_all(body);

    let params: Vec<(String, String)> = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), urldecode(v)))
        .collect();
    let get = |key: &str| {
        params
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };

    if get("state").as_deref() != Some(state) {
        return Some(Err(anyhow!("login failed: state does not match")));
    }
    match get("code") {
        Some(code) => Some(Ok(code)),
        None => Some(Err(anyhow!(
            "login failed: {}",
            get("error").unwrap_or_else(|| "no authorization code".into())
        ))),
    }
}

/// Exchanges the authorization code for the tokens at the token endpoint.
pub fn exchange_code(
    domain: &str,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<Tokens> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let body = urlencode_pairs(&[
        ("grant_type", "authorization_code"),
        ("client_id", client_id),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
    ]);
    let response = client
        .post(format!("{domain}/oauth2/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .context("cannot reach the token endpoint")?;
    let status = response.status();
    if !status.is_success() {
        bail!(
            "token exchange failed ({}): {}",
            status.as_u16(),
            response.text().unwrap_or_default()
        );
    }
    Ok(response.json()?)
}

/// Opens a URL in the default browser. The settings screen (settings.rs) uses this too.
pub fn open_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("open").arg(url).status();
    // cmd reads an unquoted & as a command separator, and the authorize URL is full of them.
    // Rust's default argument quoting does not escape &, so pass the raw command line
    // with the quotes written in by hand.
    #[cfg(target_os = "windows")]
    let status = {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .raw_arg(format!("/C start \"\" \"{url}\""))
            .status()
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let status = std::process::Command::new("xdg-open").arg(url).status();
    let status = status.context("cannot open the browser")?;
    if !status.success() {
        bail!("cannot open the browser (exit status {status})");
    }
    Ok(())
}

fn urlencode_pairs(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Also used to decode the settings screen's form (settings.rs).
pub fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 3 <= bytes.len()
                && let Some(hex) = s
                    .get(i + 1..i + 3)
                    .and_then(|h| u8::from_str_radix(h, 16).ok()) =>
            {
                out.push(hex);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_is_s256_base64url_no_padding() {
        let (verifier, challenge) = make_pkce_pair();
        // 32 random bytes encode to 43 chars, and so does a SHA-256 digest. Neither is padded
        assert_eq!(verifier.len(), 43);
        assert_eq!(challenge.len(), 43);
        assert!(!verifier.contains('='));
        assert!(!challenge.contains('='));
        assert!(!verifier.contains('+') && !verifier.contains('/'));
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
    }

    #[test]
    fn pkce_pairs_are_random() {
        assert_ne!(make_pkce_pair().0, make_pkce_pair().0);
    }

    #[test]
    fn urlencode_decode_roundtrip() {
        let original = "a b+c/d=e&f";
        assert_eq!(urldecode(&urlencode(original)), original);
    }
}
