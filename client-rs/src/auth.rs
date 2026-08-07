//! Cognito login and retrieval of temporary AWS credentials.
//!
//! The cognito-idp / cognito-identity APIs used here (InitiateAuth, GetId,
//! GetCredentialsForIdentity) are generated with the noAuth scheme, so a config
//! built with `no_credentials()` can call them unsigned.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use aws_config::{BehaviorVersion, Region};
use aws_sdk_cognitoidentityprovider::types::AuthFlowType;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::rt::runtime;

/// Reads a JWT's exp (expiry, Unix seconds) without verifying the token.
fn jwt_exp(token: &str) -> Result<f64> {
    let payload = token.split('.').nth(1).context("the JWT is malformed")?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .context("cannot base64-decode the JWT payload")?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    value["exp"].as_f64().context("the JWT has no exp")
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("the clock is before the UNIX epoch")
        .as_secs_f64()
}

/// Contents of auth.json. Files written by older versions stay readable and writable as-is.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct StoredAuth {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity_id: Option<String>,
}

/// auth.json, which holds the refresh token and friends (mode 600).
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Option<StoredAuth> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save(&self, data: &StoredAuth) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, serde_json::to_string(data)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

/// Temporary credentials handed to the S3 client.
#[derive(Debug, Clone)]
pub struct AwsCredentials {
    pub access_key_id: String,
    pub secret_key: String,
    pub session_token: String,
    pub expiration: SystemTime,
}

pub struct CognitoAuth {
    idp: aws_sdk_cognitoidentityprovider::Client,
    ci: aws_sdk_cognitoidentity::Client,
    provider: String,
    client_id: String,
    identity_pool_id: String,
    store: TokenStore,
    id_token: Mutex<Option<String>>,
}

impl CognitoAuth {
    pub fn new(
        region: &str,
        user_pool_id: &str,
        client_id: &str,
        identity_pool_id: &str,
        token_path: PathBuf,
    ) -> Self {
        let shared = runtime().block_on(
            aws_config::defaults(BehaviorVersion::latest())
                .region(Region::new(region.to_string()))
                .no_credentials()
                .load(),
        );
        Self {
            idp: aws_sdk_cognitoidentityprovider::Client::new(&shared),
            ci: aws_sdk_cognitoidentity::Client::new(&shared),
            provider: format!("cognito-idp.{region}.amazonaws.com/{user_pool_id}"),
            client_id: client_id.to_string(),
            identity_pool_id: identity_pool_id.to_string(),
            store: TokenStore::new(token_path),
            id_token: Mutex::new(None),
        }
    }

    pub fn has_login(&self) -> bool {
        self.store.load().is_some_and(|d| d.refresh_token.is_some())
    }

    /// Stores the tokens obtained from the browser login (oauth.rs).
    pub fn store_refresh_token(&self, refresh_token: &str, id_token: Option<&str>) -> Result<()> {
        let mut data = self.store.load().unwrap_or_default();
        // Drop the cached IdentityId in case this is a re-login as a different user
        data.identity_id = None;
        data.refresh_token = Some(refresh_token.to_string());
        self.store.save(&data)?;
        *self.id_token.lock().unwrap() = id_token.map(String::from);
        Ok(())
    }

    /// A valid ID token. Refreshes it once it is within 60 seconds of expiry.
    pub fn id_token(&self) -> Result<String> {
        let mut cached = self.id_token.lock().unwrap();
        if let Some(token) = cached.as_ref()
            && jwt_exp(token).is_ok_and(|exp| exp - unix_now() > 60.0)
        {
            return Ok(token.clone());
        }
        let data = self.store.load().unwrap_or_default();
        let refresh_token = data.refresh_token.context(crate::i18n::t(
            "not logged in; use Login in the tray menu",
            "ログインしていない。メニューの「ログイン」を実行する",
        ))?;
        let resp = runtime().block_on(
            self.idp
                .initiate_auth()
                .client_id(&self.client_id)
                .auth_flow(AuthFlowType::RefreshTokenAuth)
                .auth_parameters("REFRESH_TOKEN", refresh_token)
                .send(),
        );
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                if e.as_service_error()
                    .is_some_and(|s| s.is_not_authorized_exception())
                {
                    bail!(
                        "{}",
                        crate::i18n::t(
                            "the login has expired; use Login in the tray menu again",
                            "ログインの有効期限が切れている。メニューの「ログイン」をやり直す",
                        )
                    );
                }
                return Err(anyhow!(e).context("InitiateAuth (REFRESH_TOKEN_AUTH) failed"));
            }
        };
        let token = resp
            .authentication_result()
            .and_then(|r| r.id_token())
            .context("the InitiateAuth response has no IdToken")?
            .to_string();
        *cached = Some(token.clone());
        Ok(token)
    }

    /// Temporary credentials obtained through the identity pool.
    pub fn aws_credentials(&self) -> Result<AwsCredentials> {
        let id_token = self.id_token()?;
        let logins: HashMap<String, String> = HashMap::from([(self.provider.clone(), id_token)]);

        let mut data = self.store.load().unwrap_or_default();
        let identity_id = match data.identity_id.clone() {
            Some(id) => id,
            None => {
                let resp = runtime().block_on(
                    self.ci
                        .get_id()
                        .identity_pool_id(&self.identity_pool_id)
                        .set_logins(Some(logins.clone()))
                        .send(),
                );
                let id = resp
                    .context("GetId failed")?
                    .identity_id()
                    .context("the GetId response has no IdentityId")?
                    .to_string();
                data.identity_id = Some(id.clone());
                self.store.save(&data)?;
                id
            }
        };

        let resp = runtime()
            .block_on(
                self.ci
                    .get_credentials_for_identity()
                    .identity_id(identity_id)
                    .set_logins(Some(logins))
                    .send(),
            )
            .context("GetCredentialsForIdentity failed")?;
        let creds = resp
            .credentials()
            .context("the GetCredentialsForIdentity response has no Credentials")?;
        let expiration = creds
            .expiration()
            .and_then(|dt| SystemTime::try_from(*dt).ok())
            .unwrap_or_else(|| SystemTime::now() + Duration::from_secs(3600));
        Ok(AwsCredentials {
            access_key_id: creds
                .access_key_id()
                .context("the Credentials have no AccessKeyId")?
                .to_string(),
            secret_key: creds
                .secret_key()
                .context("the Credentials have no SecretKey")?
                .to_string(),
            session_token: creds
                .session_token()
                .context("the Credentials have no SessionToken")?
                .to_string(),
            expiration,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_jwt(exp: f64) -> String {
        let payload = URL_SAFE_NO_PAD.encode(format!("{{\"exp\": {exp}}}"));
        format!("h.{payload}.s")
    }

    #[test]
    fn jwt_exp_reads_payload() {
        assert_eq!(jwt_exp(&fake_jwt(1234.0)).unwrap(), 1234.0);
        assert!(jwt_exp("not-a-jwt").is_err());
    }

    #[test]
    fn token_store_roundtrip_preserves_fields() {
        let path = std::env::temp_dir()
            .join(format!("sm-auth-test-{}", std::process::id()))
            .join("auth.json");
        let _ = std::fs::remove_file(&path);
        let store = TokenStore::new(path.clone());
        assert!(store.load().is_none());
        store
            .save(&StoredAuth {
                username: Some("u".into()),
                refresh_token: Some("r".into()),
                identity_id: None,
            })
            .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.username.as_deref(), Some("u"));
        assert_eq!(loaded.refresh_token.as_deref(), Some("r"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// An auth.json written by an older version (only the three keys) still parses.
    #[test]
    fn legacy_auth_json_is_readable() {
        let json =
            r#"{"username": "user1", "refresh_token": "abc", "identity_id": "ap-northeast-1:xyz"}"#;
        let parsed: StoredAuth = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.identity_id.as_deref(), Some("ap-northeast-1:xyz"));
    }
}
