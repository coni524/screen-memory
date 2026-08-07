//! Uploads from the spool to S3, running on a dedicated background thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use aws_config::{BehaviorVersion, Region};
use aws_sdk_s3::primitives::ByteStream;

use crate::auth::{AwsCredentials, CognitoAuth};
use crate::rt::runtime;
use crate::spool::{Entry, Metadata, Spool};

const BACKOFF_INITIAL: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_secs(5);
const CREDS_REFRESH_MARGIN: Duration = Duration::from_secs(300);

pub struct Uploader {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Uploader {
    pub fn start(
        spool: Arc<Spool>,
        bucket: String,
        region: String,
        auth: Arc<CognitoAuth>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("uploader".into())
            .spawn(move || run(&spool, &bucket, &region, &auth, &stop_for_thread))
            .expect("cannot start the uploader thread");
        Self {
            stop,
            handle: Some(handle),
        }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Wait for the given duration, returning early once stop is set. Returns true if stopped.
fn wait(stop: &AtomicBool, duration: Duration) -> bool {
    let deadline = std::time::Instant::now() + duration;
    while std::time::Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    stop.load(Ordering::Relaxed)
}

/// An S3 client built from Cognito temporary credentials. Rebuilt once the credentials
/// are within 5 minutes of expiry.
struct S3Cache {
    client: Option<aws_sdk_s3::Client>,
    expires: Option<SystemTime>,
}

impl S3Cache {
    fn get(&mut self, region: &str, auth: &CognitoAuth) -> Result<&aws_sdk_s3::Client> {
        let stale = match (&self.client, self.expires) {
            (Some(_), Some(expires)) => {
                expires
                    .duration_since(SystemTime::now())
                    .unwrap_or(Duration::ZERO)
                    < CREDS_REFRESH_MARGIN
            }
            _ => true,
        };
        if stale {
            let creds = auth.aws_credentials()?;
            self.client = Some(make_s3(region, &creds));
            self.expires = Some(creds.expiration);
        }
        Ok(self.client.as_ref().expect("set just above"))
    }
}

/// The connection check in the settings screen (settings.rs) builds its client the same way.
pub fn make_s3(region: &str, creds: &AwsCredentials) -> aws_sdk_s3::Client {
    let provider = aws_sdk_s3::config::Credentials::new(
        creds.access_key_id.clone(),
        creds.secret_key.clone(),
        Some(creds.session_token.clone()),
        Some(creds.expiration),
        "cognito-identity",
    );
    let conf = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::latest())
        .region(Region::new(region.to_string()))
        .credentials_provider(provider)
        .build();
    aws_sdk_s3::Client::from_conf(conf)
}

fn run(spool: &Spool, bucket: &str, region: &str, auth: &CognitoAuth, stop: &AtomicBool) {
    let mut s3 = S3Cache {
        client: None,
        expires: None,
    };
    let mut backoff = BACKOFF_INITIAL;
    while !stop.load(Ordering::Relaxed) {
        let entries = spool.entries();
        let Some(entry) = entries.first() else {
            if wait(stop, POLL_INTERVAL) {
                return;
            }
            continue;
        };

        match upload_entry(&mut s3, bucket, region, auth, entry) {
            Ok(Some(key)) => {
                if let Some(webp) = &entry.webp {
                    let _ = std::fs::remove_file(webp);
                }
                let _ = std::fs::remove_file(&entry.json);
                tracing::info!(event = "uploaded", key = %key);
                backoff = BACKOFF_INITIAL;
            }
            // A JSON whose image was removed by the spool cap can never be uploaded, so drop it
            Ok(None) => {
                let _ = std::fs::remove_file(&entry.json);
                tracing::warn!(
                    event = "dropped_orphan",
                    key = %entry.json.file_name().unwrap_or_default().to_string_lossy(),
                );
            }
            Err(e) => {
                tracing::warn!(
                    event = "upload_error",
                    key = %entry.json.file_name().unwrap_or_default().to_string_lossy(),
                    error = %format!("{e:#}"),
                );
                if wait(stop, backoff) {
                    return;
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

/// Upload one entry to S3 and return the key it was written to. Returns None for an
/// orphaned JSON whose image is gone.
fn upload_entry(
    s3: &mut S3Cache,
    bucket: &str,
    region: &str,
    auth: &CognitoAuth,
    entry: &Entry,
) -> Result<Option<String>> {
    let meta: Metadata = serde_json::from_str(&std::fs::read_to_string(&entry.json)?)?;
    let json_bytes = std::fs::read(&entry.json)?;

    let json_key = match (&meta.image_key, &entry.webp) {
        // Image mode: image first, JSON second (the analysis Lambda subscribes only to
        // .json events)
        (Some(img_key), Some(webp)) => {
            let client = s3.get(region, auth)?;
            runtime().block_on(
                client
                    .put_object()
                    .bucket(bucket)
                    .key(img_key)
                    .content_type("image/webp")
                    .body(ByteStream::from(std::fs::read(webp)?))
                    .send(),
            )?;
            img_key.strip_suffix(".webp").unwrap_or(img_key).to_string() + ".json"
        }
        (Some(_), None) => return Ok(None),
        // Local OCR mode: a single JSON is all there is
        (None, _) => {
            let captured_at = chrono::DateTime::parse_from_rfc3339(&meta.captured_at)
                .context("cannot read capturedAt from the metadata JSON")?
                .to_utc();
            crate::spool::json_key(&meta.device, captured_at)
        }
    };

    let client = s3.get(region, auth)?;
    runtime().block_on(
        client
            .put_object()
            .bucket(bucket)
            .key(&json_key)
            .content_type("application/json")
            .body(ByteStream::from(json_bytes))
            .send(),
    )?;
    Ok(Some(json_key))
}
