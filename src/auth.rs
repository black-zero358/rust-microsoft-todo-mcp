//! Separate client authorization and Microsoft delegated authorization.
//! No upstream bodies, credentials, or task data are included in errors/logs.
use crate::{Error, Result, config::Config, db::Db};
use aes_gcm::{
    Aes256Gcm, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;
use zeroize::Zeroizing;

const SCOPES: &str = "User.Read Tasks.ReadWrite offline_access";
const MAX_BODY: usize = 256 * 1024;
const KEY_TTL: Duration = Duration::from_secs(300);
const REFRESH_COOLDOWN: Duration = Duration::from_secs(10);

pub struct Auth {
    pub config: Arc<Config>,
    db: Arc<Db>,
    client: reqwest::Client,
    microsoft_authority: String,
    profile_url: String,
    login_epoch: AtomicU64,
    keys: Mutex<Option<(JwkSet, Instant)>>,
    key_refresh: Mutex<Option<Instant>>,
    logins: Mutex<HashMap<String, DeviceLogin>>,
    account_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    access: Mutex<HashMap<String, AccessToken>>,
}
struct DeviceLogin {
    epoch: u64,
    code: Zeroizing<String>,
    expires: Instant,
    next_poll: Instant,
    interval: u64,
}
struct AccessToken {
    token: Zeroizing<String>,
    expires: Instant,
    generation: i64,
}
#[derive(Deserialize)]
struct Claims {
    sub: String,
    #[serde(default, alias = "scp")]
    scope: String,
}

impl Auth {
    pub fn new(config: Arc<Config>, db: Arc<Db>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| {
                Error::new(
                    "configuration",
                    "Unable to initialize authorization HTTP client",
                )
            })?;
        Ok(Self {
            config,
            db,
            client,
            microsoft_authority: "https://login.microsoftonline.com".into(),
            profile_url: "https://graph.microsoft.com/v1.0/me?$select=id,displayName".into(),
            login_epoch: AtomicU64::new(0),
            keys: Mutex::new(None),
            key_refresh: Mutex::new(None),
            logins: Mutex::new(HashMap::new()),
            account_locks: Mutex::new(HashMap::new()),
            access: Mutex::new(HashMap::new()),
        })
    }
    pub fn admin_authorized(&self, token: &str) -> bool {
        constant_eq(token, &self.config.admin_token)
    }
    pub fn protected_resource_metadata(&self) -> Value {
        json!({"resource":format!("{}/mcp",self.config.public_url), "authorization_servers":[self.config.issuer], "scopes_supported":[self.config.scope], "bearer_methods_supported":["header"], "resource_name":"Microsoft To Do MCP"})
    }
    pub async fn verify_bearer(&self, token: &str) -> Result<String> {
        if token.len() > 16 * 1024 {
            return Err(unauthorized());
        }
        if let Some(expected) = &self.config.dev_mcp_token {
            return if constant_eq(token, expected) {
                Ok("local-development".into())
            } else {
                Err(unauthorized())
            };
        }
        let header = decode_header(token).map_err(|_| unauthorized())?;
        if header.alg != Algorithm::RS256 {
            return Err(unauthorized());
        }
        let kid = header
            .kid
            .filter(|s| !s.is_empty() && s.len() <= 256)
            .ok_or_else(unauthorized)?;
        let key = self.signing_key(&kid).await?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[&self.config.issuer]);
        validation.set_audience(&[&self.config.audience]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.validate_nbf = true;
        validation.leeway = 30;
        let claims = decode::<Claims>(token, &key, &validation)
            .map_err(|_| unauthorized())?
            .claims;
        authorize_claims(claims, &self.config.owner_subject, &self.config.scope)
    }
    async fn signing_key(&self, kid: &str) -> Result<DecodingKey> {
        {
            let keys = self.keys.lock().await;
            if let Some((set, time)) = keys.as_ref()
                && time.elapsed() < KEY_TTL
                && let Some(key) = set.find(kid)
            {
                return decoding_key(key);
            }
        }
        let mut last = self.key_refresh.lock().await;
        {
            let keys = self.keys.lock().await;
            if let Some((set, time)) = keys.as_ref()
                && time.elapsed() < KEY_TTL
                && let Some(key) = set.find(kid)
            {
                return decoding_key(key);
            }
        }
        if last.is_some_and(|t| t.elapsed() < REFRESH_COOLDOWN) {
            return Err(unauthorized());
        }
        *last = Some(Instant::now());
        let response = self
            .client
            .get(&self.config.jwks_url)
            .send()
            .await
            .map_err(|_| {
                Error::new(
                    "auth_unavailable",
                    "Authorization signing keys are temporarily unavailable",
                )
            })?;
        if !response.status().is_success() {
            return Err(Error::new(
                "auth_unavailable",
                "Authorization signing keys are temporarily unavailable",
            ));
        }
        let set: JwkSet = bounded_json(response).await?;
        if set.keys.len() > 32 {
            return Err(Error::new(
                "auth_unavailable",
                "Authorization key set exceeds limit",
            ));
        }
        let selected = set.find(kid).map(decoding_key);
        *self.keys.lock().await = Some((set, Instant::now()));
        selected.ok_or_else(unauthorized)?
    }
    fn microsoft_endpoint(&self, endpoint: &str) -> String {
        format!(
            "{}/{}/oauth2/v2.0/{endpoint}",
            self.microsoft_authority, self.config.microsoft_tenant
        )
    }
    /// Creates a bounded server-side device session. Device codes never reach the browser.
    pub async fn start_device_login(&self) -> Result<Value> {
        let mut logins = self.logins.lock().await;
        logins.retain(|_, l| l.expires > Instant::now());
        if logins.len() >= 8 {
            return Err(Error::new(
                "login_limit",
                "Complete or wait for an existing account login to expire",
            ));
        }
        let response = self
            .client
            .post(self.microsoft_endpoint("devicecode"))
            .form(&[
                ("client_id", self.config.microsoft_client_id.as_str()),
                ("scope", SCOPES),
            ])
            .send()
            .await
            .map_err(|_| {
                Error::new(
                    "microsoft_unavailable",
                    "Microsoft authorization is temporarily unavailable",
                )
            })?;
        let success = response.status().is_success();
        let value: Value = bounded_json(response).await?;
        if !success {
            return Err(Error::new(
                "authorization_failed",
                "Microsoft rejected device login; check application ID and public client flows",
            ));
        }
        let code = field(&value, "device_code")?.to_owned();
        let user_code = field(&value, "user_code")?.to_owned();
        let uri = field(&value, "verification_uri")?.to_owned();
        let url = url::Url::parse(&uri).map_err(|_| malformed())?;
        if url.scheme() != "https"
            || !matches!(
                url.host_str(),
                Some("microsoft.com" | "www.microsoft.com" | "login.microsoftonline.com")
            )
        {
            return Err(malformed());
        }
        let expires = value["expires_in"]
            .as_u64()
            .filter(|v| (1..=1800).contains(v))
            .ok_or_else(malformed)?;
        let interval = value["interval"].as_u64().unwrap_or(5).clamp(5, 60);
        let id = uuid::Uuid::new_v4().to_string();
        logins.insert(
            id.clone(),
            DeviceLogin {
                epoch: self.login_epoch.load(Ordering::SeqCst),
                code: Zeroizing::new(code),
                expires: Instant::now() + Duration::from_secs(expires),
                next_poll: Instant::now() + Duration::from_secs(interval),
                interval,
            },
        );
        Ok(
            json!({"login_id":id,"user_code":user_code,"verification_uri":uri,"expires_in":expires,"interval":interval}),
        )
    }
    pub async fn poll_device_login(&self, login_id: &str) -> Result<Value> {
        let mut logins = self.logins.lock().await;
        logins.retain(|_, l| l.expires > Instant::now());
        let login = logins
            .get_mut(login_id)
            .ok_or_else(|| Error::new("login_expired", "Start a new Microsoft account login"))?;
        if Instant::now() < login.next_poll {
            return Ok(json!({"status":"pending","interval":login.interval}));
        }
        login.next_poll = Instant::now() + Duration::from_secs(login.interval);
        let response = self
            .client
            .post(self.microsoft_endpoint("token"))
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", self.config.microsoft_client_id.as_str()),
                ("device_code", login.code.as_str()),
            ])
            .send()
            .await
            .map_err(|_| {
                Error::new(
                    "microsoft_unavailable",
                    "Microsoft authorization is temporarily unavailable",
                )
            })?;
        let success = response.status().is_success();
        let mut value: Value = bounded_json(response).await?;
        if !success {
            match value["error"].as_str() {
                Some("authorization_pending") => {
                    return Ok(json!({"status":"pending","interval":login.interval}));
                }
                Some("slow_down") => {
                    login.interval = (login.interval + 5).min(60);
                    login.next_poll = Instant::now() + Duration::from_secs(login.interval);
                    return Ok(json!({"status":"pending","interval":login.interval}));
                }
                _ => {
                    logins.remove(login_id);
                    return Err(Error::new(
                        "authorization_failed",
                        "Microsoft login was denied or expired; start a new login",
                    ));
                }
            }
        }
        let epoch = login.epoch;
        // A successful code is single-use; remove it before any subsequent network request.
        logins.remove(login_id);
        drop(logins);
        let access = take_secret(&mut value, "access_token")?;
        let refresh = take_secret(&mut value, "refresh_token")?;
        let lifetime = token_lifetime(&value)?;
        let response = self
            .client
            .get(&self.profile_url)
            .bearer_auth(access.as_str())
            .send()
            .await
            .map_err(|_| {
                Error::new(
                    "account_lookup_failed",
                    "Microsoft account identity could not be read; start a new login",
                )
            })?;
        if !response.status().is_success() {
            return Err(Error::new(
                "account_lookup_failed",
                "Microsoft account identity could not be read; check User.Read permission",
            ));
        }
        let user: Value = bounded_json(response).await?;
        let graph_id = field(&user, "id")?;
        if graph_id.len() > 256 {
            return Err(malformed());
        }
        let id = format!("ms-{}", hex::encode(Sha256::digest(graph_id.as_bytes())));
        let display_name = user["displayName"].as_str().unwrap_or("Microsoft account");
        if display_name.len() > 1024 {
            return Err(malformed());
        }
        let lock = self.account_lock(&id).await;
        let _guard = lock.lock().await;
        if self.login_epoch.load(Ordering::SeqCst) != epoch {
            return Err(Error::new(
                "login_expired",
                "Account disconnection cancelled pending logins; start a new login",
            ));
        }
        if self.db.accounts()?.len() >= 32 && self.db.account(&id).is_err() {
            return Err(Error::new(
                "account_limit",
                "At most 32 Microsoft accounts can be connected",
            ));
        }
        let encrypted = encrypt_token(&self.config.encryption_key, &id, &refresh)?;
        let account = self.db.upsert_account(&id, display_name, &encrypted)?;
        self.access.lock().await.insert(
            id.clone(),
            AccessToken {
                token: access,
                expires: Instant::now() + lifetime,
                generation: account.generation,
            },
        );
        Ok(json!({"status":"connected","account":account}))
    }
    async fn account_lock(&self, id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.account_locks.lock().await;
        // Removing unreferenced locks bounds this map even when unknown IDs are requested.
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        locks
            .entry(id.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
    pub async fn access_token(&self, account_id: &str) -> Result<String> {
        self.access_token_checked(account_id, None).await
    }
    /// Bind a mutation's token acquisition to its original account authorization.
    /// Validation occurs under the same lock used by refresh and reauthorization.
    pub async fn access_token_for_generation(
        &self,
        account_id: &str,
        expected_generation: i64,
    ) -> Result<String> {
        self.access_token_checked(account_id, Some(expected_generation))
            .await
    }
    /// A delayed 401 for an older request must not discard a newer cached token.
    /// Returns whether the rejected token was still current and was removed.
    pub async fn invalidate_access_token(&self, account_id: &str, rejected_token: &str) -> bool {
        let lock = self.account_lock(account_id).await;
        let _guard = lock.lock().await;
        let mut cache = self.access.lock().await;
        if cache
            .get(account_id)
            .is_some_and(|token| constant_eq(&token.token, rejected_token))
        {
            cache.remove(account_id);
            true
        } else {
            false
        }
    }
    async fn access_token_checked(
        &self,
        account_id: &str,
        expected_generation: Option<i64>,
    ) -> Result<String> {
        let lock = self.account_lock(account_id).await;
        let _guard = lock.lock().await;
        let (encrypted, generation) = self.db.token(account_id)?;
        if expected_generation.is_some_and(|expected| expected != generation) {
            return Err(Error::new(
                "account_changed",
                "The account was revoked or reauthorized during this operation",
            ));
        }
        {
            let cache = self.access.lock().await;
            if let Some(token) = cache.get(account_id)
                && token.generation == generation
                && token.expires > Instant::now()
            {
                return Ok(token.token.to_string());
            }
        }
        let refresh = decrypt_token(&self.config.encryption_key, account_id, &encrypted)?;
        let response = self
            .client
            .post(self.microsoft_endpoint("token"))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", self.config.microsoft_client_id.as_str()),
                ("refresh_token", refresh.as_str()),
                ("scope", SCOPES),
            ])
            .send()
            .await
            .map_err(|_| {
                Error::new(
                    "microsoft_unavailable",
                    "Microsoft token refresh is temporarily unavailable",
                )
            })?;
        let success = response.status().is_success();
        let mut value: Value = bounded_json(response).await?;
        if !success {
            return Err(match value["error"].as_str() {
                Some("invalid_grant" | "interaction_required" | "invalid_client") => Error::new(
                    "reauthorization_required",
                    "Reconnect this Microsoft account in the management page",
                ),
                _ => Error::new(
                    "microsoft_unavailable",
                    "Microsoft token refresh is temporarily unavailable",
                ),
            });
        }
        let access = take_secret(&mut value, "access_token")?;
        let lifetime = token_lifetime(&value)?;
        let refresh = if value["refresh_token"].is_string() {
            take_secret(&mut value, "refresh_token")?
        } else {
            refresh
        };
        let encrypted = encrypt_token(&self.config.encryption_key, account_id, &refresh)?;
        self.db.update_token(account_id, generation, &encrypted)?;
        let result = access.to_string();
        self.access.lock().await.insert(
            account_id.into(),
            AccessToken {
                token: access,
                expires: Instant::now() + lifetime,
                generation,
            },
        );
        Ok(result)
    }
    /// Local disconnection deletes credentials/cache. Users revoke the Microsoft
    /// application grant itself at their Microsoft account permissions page.
    pub async fn revoke(&self, account_id: &str) -> Result<()> {
        let lock = self.account_lock(account_id).await;
        let _guard = lock.lock().await;
        self.login_epoch.fetch_add(1, Ordering::SeqCst);
        self.db.remove_account(account_id)?;
        self.logins.lock().await.clear();
        self.access.lock().await.remove(account_id);
        Ok(())
    }
}
fn decoding_key(key: &jsonwebtoken::jwk::Jwk) -> Result<DecodingKey> {
    use jsonwebtoken::jwk::{KeyAlgorithm, KeyOperations, PublicKeyUse};
    if key
        .common
        .key_algorithm
        .is_some_and(|a| a != KeyAlgorithm::RS256)
        || key
            .common
            .public_key_use
            .as_ref()
            .is_some_and(|u| *u != PublicKeyUse::Signature)
        || key
            .common
            .key_operations
            .as_ref()
            .is_some_and(|ops| !ops.contains(&KeyOperations::Verify))
    {
        return Err(unauthorized());
    }
    DecodingKey::from_jwk(key).map_err(|_| unauthorized())
}
fn constant_eq(a: &str, b: &str) -> bool {
    Sha256::digest(a.as_bytes())
        .ct_eq(&Sha256::digest(b.as_bytes()))
        .into()
}
fn authorize_claims(claims: Claims, owner: &str, scope: &str) -> Result<String> {
    if claims.sub != owner || !claims.scope.split_ascii_whitespace().any(|s| s == scope) {
        return Err(unauthorized());
    }
    Ok(claims.sub)
}
fn unauthorized() -> Error {
    Error::new(
        "unauthorized",
        "A valid owner access token with the required resource and scope is required",
    )
}
fn malformed() -> Error {
    Error::new(
        "authorization_failed",
        "Microsoft authorization returned an invalid response",
    )
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(malformed)
}
fn take_secret(value: &mut Value, name: &str) -> Result<Zeroizing<String>> {
    let secret = value
        .get_mut(name)
        .map(Value::take)
        .and_then(|v| v.as_str().map(str::to_owned))
        .filter(|s| !s.is_empty())
        .ok_or_else(malformed)?;
    Ok(Zeroizing::new(secret))
}
fn token_lifetime(value: &Value) -> Result<Duration> {
    let secs = value["expires_in"]
        .as_u64()
        .filter(|n| (60..=86400).contains(n))
        .ok_or_else(malformed)?;
    Ok(Duration::from_secs(secs.saturating_sub(60)))
}
async fn bounded_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_BODY as u64)
    {
        return Err(Error::new(
            "auth_response_limit",
            "Authorization response exceeds limit",
        ));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| Error::new("auth_unavailable", "Authorization response was interrupted"))?
    {
        if chunk.len() > MAX_BODY - bytes.len() {
            return Err(Error::new(
                "auth_response_limit",
                "Authorization response exceeds limit",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| Error::new("auth_unavailable", "Authorization response was invalid"))
}
fn encrypt_token(key: &[u8; 32], account: &str, token: &str) -> Result<String> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| Error::new("credential_storage", "Invalid encryption key"))?;
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let encrypted = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: token.as_bytes(),
                aad: account.as_bytes(),
            },
        )
        .map_err(|_| {
            Error::new(
                "credential_storage",
                "Unable to encrypt account credentials",
            )
        })?;
    let mut result = nonce.to_vec();
    result.extend(encrypted);
    Ok(STANDARD.encode(result))
}
fn decrypt_token(key: &[u8; 32], account: &str, encoded: &str) -> Result<Zeroizing<String>> {
    let error = || {
        Error::new(
            "credential_storage",
            "Unable to decrypt account credentials; verify the configured encryption key",
        )
    };
    let bytes = STANDARD.decode(encoded).map_err(|_| error())?;
    if bytes.len() < 28 {
        return Err(error());
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| error())?;
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&bytes[..12]),
            Payload {
                msg: &bytes[12..],
                aad: account.as_bytes(),
            },
        )
        .map_err(|_| error())?;
    Ok(Zeroizing::new(
        String::from_utf8(plaintext).map_err(|_| error())?,
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn token_encryption_is_random_and_account_bound() {
        let key = [42; 32];
        let a = encrypt_token(&key, "account-a", "secret-refresh").unwrap();
        let b = encrypt_token(&key, "account-a", "secret-refresh").unwrap();
        assert_ne!(a, b);
        assert_eq!(
            decrypt_token(&key, "account-a", &a).unwrap().as_str(),
            "secret-refresh"
        );
        assert!(decrypt_token(&key, "account-b", &a).is_err());
        assert!(decrypt_token(&[43; 32], "account-a", &a).is_err());
        let mut bytes = STANDARD.decode(&a).unwrap();
        bytes[15] ^= 1;
        assert!(decrypt_token(&key, "account-a", &STANDARD.encode(bytes)).is_err());
    }
    #[test]
    fn authorization_checks_owner_and_whole_scope() {
        assert!(
            authorize_claims(
                Claims {
                    sub: "owner".into(),
                    scope: "openid todo:mcp".into()
                },
                "owner",
                "todo:mcp"
            )
            .is_ok()
        );
        for (sub, scope) in [
            ("other", "todo:mcp"),
            ("owner", "todo:mcp-admin"),
            ("owner", ""),
        ] {
            assert!(
                authorize_claims(
                    Claims {
                        sub: sub.into(),
                        scope: scope.into()
                    },
                    "owner",
                    "todo:mcp"
                )
                .is_err()
            );
        }
        assert!(constant_eq("abc", "abc"));
        assert!(!constant_eq("abc", "abcd"));
    }
    #[test]
    fn malformed_token_lifetimes_are_rejected() {
        assert!(token_lifetime(&json!({"expires_in":1})).is_err());
        assert_eq!(
            token_lifetime(&json!({"expires_in":3600})).unwrap(),
            Duration::from_secs(3540)
        );
    }
}

#[cfg(test)]
mod jwt_tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    // Ephemeral synthetic fixture, generated exclusively for these tests.
    const TEST_KEY: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDCNzR0iqFM9JiN
wEtb6Dh7O9z/ei1YMzzCXvZzxyfA+DvFQrevwVEFwL3TwWaYDEI1lXKlLfw5e3XA
GQVWT6WhiiCqrnGJOgZ5d+bPnbv2rQF7k+TKfEL2pX/ZzZ+Mrt9ZjWaUu179Lnqc
rDKqPr6MBX1VDIQHcyKhcQnITAVjAwHT88FMBU732BbZ/QUyQymHNDS4Ph2WysXn
Ext/B3MkqQmtvFHf1INyYY3LV3F5sawoSKyjgvnKisw5W3+RVaHw1FdU4e7BleA3
wZ1FZJZp4dy/WagpshFff677r7IdMz+phCWsIThh0vSo4CJfyEHvTmYJS/vlrNlf
wkGE4jRTAgMBAAECggEARQ/Nat27EvrtXoaxJXrzzrqQURaYlkMOCY4sNCpvE/sR
gglB+VEUZyfNOMYOneXKC3TbwioShTxGOGuNTs5etw0/pYIHpIUKNVpyLITKCKMn
olKKaoh6ZWufhYtNf4DVsmoKPrxMQFqtRhYjpA9QwcjX8wzSzDVgbKkeEegaiES0
SUqx5tVoI4bsR7lHf9141Ve25hH55AgyyfQFhq9XjKfBZaIa5CRlQcDBv1HMDz1f
tkJzRw48n2IdTwgmqj/iXIUAgDfFgRfKDBm21gvGDd/LPjOyALE0KVN9y0I2S1WH
gWz/2OOkKy3DRvMBAzOrQ8syWXPZ8ioDstVQXXohaQKBgQDnlOL63IZ+U4Ek4Oxf
GWIk1Y3pzIuEYGZ6yugQY1LWUYqMo/jZVZbg6hoP3KjavChjAwxZxSnyi4o2WV9J
hmW1qvJP4nsCLgEKPJft6mGH7Wu1MsR+4j+zoV/De9XUS/eifbcN1vZfV6WvvW5M
Evit3PNvjANKApbYw/BuY8cdewKBgQDWsbG5q68uqc86RONrzb3rSDyp6Q6gMucu
gkgEPYY4aSpXNmauJkvKSRJH0ytoQcZDlegzPLba0OFp1DcYE6Rr+u2ib/dDvnVH
p7Ze4GQA7dbkg2Imb2nVGvqvV8F2dBOUs8BhZR1vOotn2uz2guS9ywvt/xBnQTjs
2pOp9bcRCQKBgBKJinnGOUXXbVkEYCdkosSMC6YRZ4GbLdjacSUOrkzVjh+f6IWN
IlyyoI9wfFu2AA2c6tosvCTcIp4jHUuHZJDcNDVMzcfo4tBs9URoJJJ64MpKXAxE
FBFO85l0SjOKOTJamdKTyHjtTSf6I+AXDKzuKNZfAMxl1KlZVd+gQQoNAoGAb+VL
GNcnRa3RSLBQy4X+vxr2KyRBsDL3lKDoQfWC9/Gv1KaQ26EwDS0Kbw7bZ8ooOF9w
FwxEzUTixxKI6G6MWo78vZWXjOI+I+uAPHC9DlLnlk/3nsqgoSYfk7WsKj2TWwRY
dyqJLciYkbdjm5Na/qROAmvWs8oljiTZo1JiaGkCgYEA2gthweFCe3MHwvd4YDR0
eiYbI5+08BxnAMzGjH62bogvHaB4F1XnmJi5B5qAzXv1WDiDImgfj+6gYollKU22
5Y1izor85zypvzobnWMhP79f42q0Xzzkj1KhQHltysq0CYQOtgXHdb5Htv8jIiZg
CQnO+yVTmDvAoQEBRe9YRHE=
-----END PRIVATE KEY-----
"#;
    fn config() -> Config {
        Config {
            bind: "127.0.0.1:8080".parse().unwrap(),
            public_url: "https://todo.example.com".into(),
            data_dir: "unused".into(),
            microsoft_client_id: "00000000-0000-0000-0000-000000000001".into(),
            microsoft_tenant: "common".into(),
            sync_interval_secs: 60,
            issuer: "https://auth.example.com".into(),
            jwks_url: "https://auth.example.com/jwks".into(),
            audience: "https://todo.example.com/mcp".into(),
            scope: "todo:mcp".into(),
            owner_subject: "owner".into(),
            admin_token: "synthetic-admin-secret-000000000000".into(),
            encryption_key: [42; 32],
            dev_mcp_token: None,
        }
    }
    fn signed(claims: &Value) -> String {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("test-key".into());
        encode(
            &header,
            claims,
            &EncodingKey::from_rsa_pem(TEST_KEY.as_bytes()).unwrap(),
        )
        .unwrap()
    }
    #[tokio::test]
    async fn full_bearer_path_verifies_signature_expiry_issuer_resource_owner_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(&tmp.path().join("auth.db")).unwrap());
        let auth = Auth::new(Arc::new(config()), db).unwrap();
        *auth.keys.lock().await=Some((serde_json::from_value(json!({"keys":[{"kty":"RSA","kid":"test-key","alg":"RS256","use":"sig","n":"wjc0dIqhTPSYjcBLW-g4ezvc_3otWDM8wl72c8cnwPg7xUK3r8FRBcC908FmmAxCNZVypS38OXt1wBkFVk-loYogqq5xiToGeXfmz5279q0Be5PkynxC9qV_2c2fjK7fWY1mlLte_S56nKwyqj6-jAV9VQyEB3MioXEJyEwFYwMB0_PBTAVO99gW2f0FMkMphzQ0uD4dlsrF5xMbfwdzJKkJrbxR39SDcmGNy1dxebGsKEiso4L5yorMOVt_kVWh8NRXVOHuwZXgN8GdRWSWaeHcv1moKbIRX3-u-6-yHTM_qYQlrCE4YdL0qOAiX8hB705mCUv75azZX8JBhOI0Uw","e":"AQAB"}]})).unwrap(),Instant::now()));
        *auth.key_refresh.lock().await = Some(Instant::now());
        let now = chrono::Utc::now().timestamp();
        let claims = json!({"sub":"owner","iss":"https://auth.example.com","aud":"https://todo.example.com/mcp","scope":"openid todo:mcp","exp":now+300,"nbf":now-1});
        assert_eq!(auth.verify_bearer(&signed(&claims)).await.unwrap(), "owner");
        for (field, value) in [
            ("sub", json!("other")),
            ("iss", json!("https://other.example.com")),
            ("aud", json!("another-resource")),
            ("scope", json!("todo:mcp-admin")),
            ("exp", json!(now - 60)),
            ("nbf", json!(now + 120)),
        ] {
            let mut invalid = claims.clone();
            invalid[field] = value;
            assert!(
                auth.verify_bearer(&signed(&invalid)).await.is_err(),
                "accepted invalid {field}"
            );
        }
        let mut missing = claims.clone();
        missing.as_object_mut().unwrap().remove("exp");
        assert!(auth.verify_bearer(&signed(&missing)).await.is_err());
        let hs = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(b"synthetic"),
        )
        .unwrap();
        assert!(auth.verify_bearer(&hs).await.is_err());
        let mut tampered = signed(&claims).into_bytes();
        let i = tampered.len() - 10;
        tampered[i] = if tampered[i] == b'A' { b'B' } else { b'A' };
        assert!(
            auth.verify_bearer(std::str::from_utf8(&tampered).unwrap())
                .await
                .is_err()
        );
        assert!(auth.admin_authorized("synthetic-admin-secret-000000000000"));
        assert!(!auth.admin_authorized(&signed(&claims)));
    }
    #[tokio::test]
    async fn removed_account_cannot_use_cached_access_token() {
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(&tmp.path().join("auth.db")).unwrap());
        let account = db
            .upsert_account("account-a", "Synthetic", "unused")
            .unwrap();
        let auth = Auth::new(Arc::new(config()), db.clone()).unwrap();
        auth.access.lock().await.insert(
            "account-a".into(),
            AccessToken {
                token: Zeroizing::new("cached-synthetic".into()),
                expires: Instant::now() + Duration::from_secs(60),
                generation: account.generation,
            },
        );
        assert_eq!(
            auth.access_token("account-a").await.unwrap(),
            "cached-synthetic"
        );
        auth.revoke("account-a").await.unwrap();
        assert!(auth.access_token("account-a").await.is_err());
        assert!(auth.access.lock().await.is_empty());
    }
    struct DeviceFixture {
        tokens: Mutex<std::collections::VecDeque<(u16, Value)>>,
        polls: std::sync::atomic::AtomicUsize,
        pause_profile: bool,
        profile_started: tokio::sync::Notify,
        profile_release: tokio::sync::Notify,
    }
    async fn device_code() -> axum::Json<Value> {
        axum::Json(
            json!({"device_code":"synthetic-device-secret","user_code":"ABCD-EFGH","verification_uri":"https://microsoft.com/devicelogin","expires_in":900,"interval":5}),
        )
    }
    async fn fixture_token(
        axum::extract::State(state): axum::extract::State<Arc<DeviceFixture>>,
    ) -> (axum::http::StatusCode, axum::Json<Value>) {
        state.polls.fetch_add(1, Ordering::SeqCst);
        let (status, body) = state
            .tokens
            .lock()
            .await
            .pop_front()
            .expect("unexpected token request");
        (
            axum::http::StatusCode::from_u16(status).unwrap(),
            axum::Json(body),
        )
    }
    async fn fixture_profile(
        axum::extract::State(state): axum::extract::State<Arc<DeviceFixture>>,
    ) -> axum::Json<Value> {
        if state.pause_profile {
            state.profile_started.notify_one();
            state.profile_release.notified().await;
        }
        axum::Json(json!({"id":"synthetic-graph-identity","displayName":"Synthetic User"}))
    }
    async fn fixture(
        tokens: Vec<(u16, Value)>,
        pause_profile: bool,
    ) -> (
        Arc<Auth>,
        Arc<DeviceFixture>,
        tokio::task::JoinHandle<()>,
        tempfile::TempDir,
    ) {
        let state = Arc::new(DeviceFixture {
            tokens: Mutex::new(tokens.into()),
            polls: std::sync::atomic::AtomicUsize::new(0),
            pause_profile,
            profile_started: tokio::sync::Notify::new(),
            profile_release: tokio::sync::Notify::new(),
        });
        let app = axum::Router::new()
            .route(
                "/common/oauth2/v2.0/devicecode",
                axum::routing::post(device_code),
            )
            .route(
                "/common/oauth2/v2.0/token",
                axum::routing::post(fixture_token),
            )
            .route("/me", axum::routing::get(fixture_profile))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let tmp = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open(&tmp.path().join("device.db")).unwrap());
        let mut auth = Auth::new(Arc::new(config()), db).unwrap();
        auth.microsoft_authority = base.clone();
        auth.profile_url = format!("{base}/me");
        auth.client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        (Arc::new(auth), state, server, tmp)
    }
    async fn allow_poll(auth: &Auth, id: &str) {
        auth.logins.lock().await.get_mut(id).unwrap().next_poll =
            Instant::now() - Duration::from_secs(1);
    }
    #[tokio::test]
    async fn device_pending_slowdown_success_and_refresh_rotation() {
        let (auth,state,server,_tmp)=fixture(vec![(400,json!({"error":"authorization_pending"})),(400,json!({"error":"slow_down"})),(200,json!({"access_token":"synthetic-access-1","refresh_token":"synthetic-refresh-1","expires_in":3600})),(200,json!({"access_token":"synthetic-access-2","refresh_token":"synthetic-refresh-2","expires_in":3600}))],false).await;
        let login = auth.start_device_login().await.unwrap();
        assert!(login.get("device_code").is_none());
        let id = login["login_id"].as_str().unwrap();
        assert_eq!(
            auth.poll_device_login(id).await.unwrap()["status"],
            "pending"
        );
        assert_eq!(state.polls.load(Ordering::SeqCst), 0);
        allow_poll(&auth, id).await;
        assert_eq!(auth.poll_device_login(id).await.unwrap()["interval"], 5);
        allow_poll(&auth, id).await;
        assert_eq!(auth.poll_device_login(id).await.unwrap()["interval"], 10);
        allow_poll(&auth, id).await;
        let connected = auth.poll_device_login(id).await.unwrap();
        assert_eq!(connected["status"], "connected");
        assert!(!connected.to_string().contains("synthetic-refresh"));
        let account = connected["account"]["id"].as_str().unwrap();
        assert_eq!(
            auth.access_token(account).await.unwrap(),
            "synthetic-access-1"
        );
        let (encrypted, _) = auth.db.token(account).unwrap();
        assert!(!encrypted.contains("synthetic-refresh"));
        assert_eq!(
            decrypt_token(&auth.config.encryption_key, account, &encrypted)
                .unwrap()
                .as_str(),
            "synthetic-refresh-1"
        );
        auth.access.lock().await.get_mut(account).unwrap().expires =
            Instant::now() - Duration::from_secs(1);
        assert_eq!(
            auth.access_token(account).await.unwrap(),
            "synthetic-access-2"
        );
        let (encrypted, _) = auth.db.token(account).unwrap();
        assert_eq!(
            decrypt_token(&auth.config.encryption_key, account, &encrypted)
                .unwrap()
                .as_str(),
            "synthetic-refresh-2"
        );
        assert_eq!(state.polls.load(Ordering::SeqCst), 4);
        assert!(auth.poll_device_login(id).await.is_err());
        server.abort();
    }
    #[tokio::test]
    async fn expired_device_login_does_not_poll_microsoft() {
        let (auth, state, server, _tmp) = fixture(vec![], false).await;
        let login = auth.start_device_login().await.unwrap();
        let id = login["login_id"].as_str().unwrap();
        auth.logins.lock().await.get_mut(id).unwrap().expires =
            Instant::now() - Duration::from_secs(1);
        assert_eq!(
            auth.poll_device_login(id).await.unwrap_err().code,
            "login_expired"
        );
        assert_eq!(state.polls.load(Ordering::SeqCst), 0);
        server.abort();
    }
    #[tokio::test]
    async fn disconnect_fences_successful_login_waiting_on_identity() {
        let (auth,state,server,_tmp)=fixture(vec![(200,json!({"access_token":"synthetic-access","refresh_token":"synthetic-refresh","expires_in":3600}))],true).await;
        let account = format!(
            "ms-{}",
            hex::encode(Sha256::digest(b"synthetic-graph-identity"))
        );
        auth.db
            .upsert_account(&account, "Previous login", "unused")
            .unwrap();
        let login = auth.start_device_login().await.unwrap();
        let id = login["login_id"].as_str().unwrap().to_owned();
        allow_poll(&auth, &id).await;
        let polling = tokio::spawn({
            let auth = auth.clone();
            async move { auth.poll_device_login(&id).await }
        });
        tokio::time::timeout(Duration::from_secs(3), state.profile_started.notified())
            .await
            .unwrap();
        auth.revoke(&account).await.unwrap();
        state.profile_release.notify_one();
        let result = polling.await.unwrap().unwrap_err();
        assert_eq!(result.code, "login_expired");
        assert!(auth.db.account(&account).is_err());
        assert!(auth.access.lock().await.is_empty());
        server.abort();
    }
    #[tokio::test]
    async fn generation_fence_rejects_stale_claim_before_cached_token_or_refresh() {
        let (auth, state, server, _tmp) = fixture(vec![], false).await;
        let old = auth
            .db
            .upsert_account("account-a", "Original", "unused")
            .unwrap();
        let current = auth
            .db
            .upsert_account("account-a", "Reauthorized", "unused")
            .unwrap();
        auth.access.lock().await.insert(
            "account-a".into(),
            AccessToken {
                token: Zeroizing::new("current-token".into()),
                expires: Instant::now() + Duration::from_secs(60),
                generation: current.generation,
            },
        );
        assert_eq!(
            auth.access_token_for_generation("account-a", old.generation)
                .await
                .unwrap_err()
                .code,
            "account_changed"
        );
        assert_eq!(
            auth.access_token_for_generation("account-a", current.generation)
                .await
                .unwrap(),
            "current-token"
        );
        auth.access.lock().await.clear();
        assert_eq!(
            auth.access_token_for_generation("account-a", old.generation)
                .await
                .unwrap_err()
                .code,
            "account_changed"
        );
        assert_eq!(state.polls.load(Ordering::SeqCst), 0);
        server.abort();
    }
    #[tokio::test]
    async fn rejected_token_refreshes_once_and_late_invalidation_preserves_replacement() {
        let (auth,state,server,_tmp)=fixture(vec![(200,json!({"access_token":"new-access","refresh_token":"new-refresh","expires_in":3600}))],false).await;
        let encrypted =
            encrypt_token(&auth.config.encryption_key, "account-a", "old-refresh").unwrap();
        let account = auth
            .db
            .upsert_account("account-a", "Synthetic", &encrypted)
            .unwrap();
        auth.access.lock().await.insert(
            "account-a".into(),
            AccessToken {
                token: Zeroizing::new("rejected-access".into()),
                expires: Instant::now() + Duration::from_secs(60),
                generation: account.generation,
            },
        );
        assert!(
            auth.invalidate_access_token("account-a", "rejected-access")
                .await
        );
        assert_eq!(
            auth.access_token_for_generation("account-a", account.generation)
                .await
                .unwrap(),
            "new-access"
        );
        assert!(
            !auth
                .invalidate_access_token("account-a", "rejected-access")
                .await
        );
        assert_eq!(auth.access_token("account-a").await.unwrap(), "new-access");
        assert!(!auth.invalidate_access_token("missing", "new-access").await);
        assert_eq!(state.polls.load(Ordering::SeqCst), 1);
        server.abort();
    }
    #[tokio::test]
    async fn graph_401_refreshes_reads_once_never_replays_writes_and_fences_reauthorization() {
        use axum::{
            extract::State,
            http::{HeaderMap, Method, StatusCode},
            routing::any,
        };
        type Seen = Arc<Mutex<Vec<(Method, String)>>>;
        async fn graph_handler(
            State(seen): State<Seen>,
            method: Method,
            headers: HeaderMap,
        ) -> (StatusCode, axum::Json<Value>) {
            let mut requests = seen.lock().await;
            let token = headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            let first = requests.is_empty();
            requests.push((method.clone(), token));
            if first || method == Method::POST {
                (
                    StatusCode::UNAUTHORIZED,
                    axum::Json(json!({"error":{"code":"InvalidAuthenticationToken"}})),
                )
            } else {
                (StatusCode::OK, axum::Json(json!({"value":[]})))
            }
        }
        let (auth,tokens,token_server,_tmp)=fixture(vec![(200,json!({"access_token":"refreshed-access","refresh_token":"refreshed-refresh","expires_in":3600}))],false).await;
        let encrypted =
            encrypt_token(&auth.config.encryption_key, "account-a", "initial-refresh").unwrap();
        let account = auth
            .db
            .upsert_account("account-a", "Synthetic", &encrypted)
            .unwrap();
        auth.access.lock().await.insert(
            "account-a".into(),
            AccessToken {
                token: Zeroizing::new("rejected-access".into()),
                expires: Instant::now() + Duration::from_secs(60),
                generation: account.generation,
            },
        );
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let app = axum::Router::new()
            .route("/v1.0/me/todo/lists", any(graph_handler))
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}/v1.0/", listener.local_addr().unwrap());
        let graph_server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let graph = crate::graph::GraphClient::for_test_with_auth(&root, auth.clone());
        assert_eq!(
            graph.get("account-a", "/me/todo/lists").await.unwrap(),
            json!({"value":[]})
        );
        assert_eq!(
            *seen.lock().await,
            vec![
                (Method::GET, "Bearer rejected-access".into()),
                (Method::GET, "Bearer refreshed-access".into())
            ]
        );
        assert_eq!(tokens.polls.load(Ordering::SeqCst), 1);
        let error = graph
            .write_at_generation(
                "account-a",
                account.generation,
                Method::POST,
                "/me/todo/lists",
                Some(json!({"displayName":"Synthetic"})),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "account_auth_required");
        assert_eq!(seen.lock().await.len(), 3);
        assert_eq!(
            seen.lock().await[2],
            (Method::POST, "Bearer refreshed-access".into())
        );
        assert_eq!(tokens.polls.load(Ordering::SeqCst), 1);
        assert!(!auth.access.lock().await.contains_key("account-a"));
        let renewed = auth
            .db
            .upsert_account("account-a", "Reauthorized", &encrypted)
            .unwrap();
        auth.access.lock().await.insert(
            "account-a".into(),
            AccessToken {
                token: Zeroizing::new("reauthorized-access".into()),
                expires: Instant::now() + Duration::from_secs(60),
                generation: renewed.generation,
            },
        );
        let stale = graph
            .write_at_generation(
                "account-a",
                account.generation,
                Method::POST,
                "/me/todo/lists",
                Some(json!({"displayName":"Stale intent"})),
            )
            .await
            .unwrap_err();
        assert_eq!(stale.code, "account_changed");
        assert_eq!(seen.lock().await.len(), 3);
        assert_eq!(tokens.polls.load(Ordering::SeqCst), 1);
        graph_server.abort();
        token_server.abort();
    }
}
