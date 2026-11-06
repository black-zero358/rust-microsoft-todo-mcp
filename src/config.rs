//! Validated configuration. Secrets deliberately have no Debug implementation.
use crate::{Error, Result};
use std::{collections::HashMap, net::SocketAddr, path::PathBuf};
use url::Url;

pub struct Config {
    pub bind: SocketAddr,
    pub public_url: String,
    pub data_dir: PathBuf,
    pub microsoft_client_id: String,
    pub microsoft_tenant: String,
    pub sync_interval_secs: u64,
    pub issuer: String,
    pub jwks_url: String,
    pub audience: String,
    pub scope: String,
    pub owner_subject: String,
    pub admin_token: String,
    pub encryption_key: [u8; 32],
    pub dev_mcp_token: Option<String>,
}
impl Config {
    pub fn from_env() -> Result<Self> {
        Self::parse(&std::env::vars().collect())
    }
    fn parse(env: &HashMap<String, String>) -> Result<Self> {
        let get =
            |key: &str, default: &str| env.get(key).cloned().unwrap_or_else(|| default.into());
        let required = |key: &str| -> Result<String> {
            env.get(key)
                .filter(|v| !v.trim().is_empty())
                .cloned()
                .ok_or_else(|| Error::new("configuration", format!("{key} is required")))
        };
        let bind: SocketAddr = get("TODO_BIND", "127.0.0.1:8080")
            .parse()
            .map_err(|_| Error::new("configuration", "TODO_BIND must be an IP address and port"))?;
        let public_url = required("TODO_PUBLIC_URL")?
            .trim_end_matches('/')
            .to_string();
        let url = validate_url(&public_url, "TODO_PUBLIC_URL", true)?;
        if url.path() != "/" {
            return Err(Error::new(
                "configuration",
                "TODO_PUBLIC_URL must be an origin without a path",
            ));
        }
        let public_url = url.origin().ascii_serialization();
        let dev_mcp_token = env
            .get("TODO_DEV_MCP_TOKEN")
            .filter(|s| !s.is_empty())
            .cloned();
        if let Some(token) = &dev_mcp_token
            && (!bind.ip().is_loopback() || !is_loopback(&url) || !valid_bearer_secret(token))
        {
            return Err(Error::new(
                "configuration",
                "Development MCP tokens require loopback bind/public URL and a 32–16377 character visible ASCII token without whitespace",
            ));
        }
        let admin_token = required("TODO_ADMIN_TOKEN")?;
        if !valid_bearer_secret(&admin_token) || admin_token.starts_with("replace-with-") {
            return Err(Error::new(
                "configuration",
                "TODO_ADMIN_TOKEN requires 32–16377 random visible ASCII characters without whitespace",
            ));
        }
        if dev_mcp_token.as_ref() == Some(&admin_token) {
            return Err(Error::new(
                "configuration",
                "Admin and MCP tokens must be distinct",
            ));
        }
        let key = hex::decode(required("TODO_ENCRYPTION_KEY")?).map_err(|_| {
            Error::new(
                "configuration",
                "TODO_ENCRYPTION_KEY must be 64 hexadecimal characters",
            )
        })?;
        let encryption_key: [u8; 32] = key.try_into().map_err(|_| {
            Error::new(
                "configuration",
                "TODO_ENCRYPTION_KEY must be 64 hexadecimal characters",
            )
        })?;
        if encryption_key == [0; 32] {
            return Err(Error::new(
                "configuration",
                "Generate a random encryption key",
            ));
        }
        let microsoft_client_id = required("MICROSOFT_CLIENT_ID")?;
        let microsoft_client_id = uuid::Uuid::parse_str(&microsoft_client_id).map_err(|_| {
            Error::new(
                "configuration",
                "MICROSOFT_CLIENT_ID must be an application UUID",
            )
        })?;
        if microsoft_client_id.is_nil() {
            return Err(Error::new(
                "configuration",
                "Replace the sample MICROSOFT_CLIENT_ID with your registered application UUID",
            ));
        }
        let microsoft_client_id = microsoft_client_id.to_string();
        let microsoft_tenant = get("MICROSOFT_TENANT", "common");
        let microsoft_tenant = if matches!(
            microsoft_tenant.as_str(),
            "common" | "organizations" | "consumers"
        ) {
            microsoft_tenant
        } else {
            uuid::Uuid::parse_str(&microsoft_tenant).map_err(|_| Error::new(
                "configuration",
                "MICROSOFT_TENANT must be common, organizations, consumers, or a tenant UUID",
            ))?.to_string()
        };
        let sync_interval_secs = get("TODO_SYNC_INTERVAL_SECS", "60")
            .parse::<u64>()
            .map_err(|_| {
                Error::new(
                    "configuration",
                    "TODO_SYNC_INTERVAL_SECS must be an integer",
                )
            })?;
        if !(15..=86400).contains(&sync_interval_secs) {
            return Err(Error::new(
                "configuration",
                "Sync interval must be between 15 and 86400 seconds",
            ));
        }
        let (issuer, jwks_url, audience, scope, owner_subject) = if dev_mcp_token.is_some() {
            (
                String::new(),
                String::new(),
                format!("{public_url}/mcp"),
                "todo:mcp".into(),
                "local-development".into(),
            )
        } else {
            let issuer = required("TODO_OAUTH_ISSUER")?;
            let issuer_url = validate_url(&issuer, "TODO_OAUTH_ISSUER", false)?;
            let jwks_url = required("TODO_OAUTH_JWKS_URL")?;
            let jwks = validate_url(&jwks_url, "TODO_OAUTH_JWKS_URL", false)?;
            if issuer_url.origin() != jwks.origin() {
                return Err(Error::new(
                    "configuration",
                    "JWKS must have the same HTTPS origin as the OAuth issuer",
                ));
            }
            let audience = required("TODO_OAUTH_AUDIENCE")?;
            if audience != format!("{public_url}/mcp") {
                return Err(Error::new(
                    "configuration",
                    "TODO_OAUTH_AUDIENCE must equal TODO_PUBLIC_URL followed by /mcp",
                ));
            }
            let scope = get("TODO_OAUTH_SCOPE", "todo:mcp");
            if scope.is_empty() || scope.chars().any(char::is_whitespace) {
                return Err(Error::new(
                    "configuration",
                    "Configure one nonempty OAuth scope",
                ));
            }
            (
                issuer,
                jwks_url,
                audience,
                scope,
                required("TODO_OAUTH_OWNER_SUBJECT")?,
            )
        };
        Ok(Self {
            bind,
            public_url,
            data_dir: PathBuf::from(get("TODO_DATA_DIR", "data")),
            microsoft_client_id,
            microsoft_tenant,
            sync_interval_secs,
            issuer,
            jwks_url,
            audience,
            scope,
            owner_subject,
            admin_token,
            encryption_key,
            dev_mcp_token,
        })
    }
}
fn valid_bearer_secret(token: &str) -> bool {
    // The shared HTTP parser bounds the entire "Bearer <token>" header to 16 KiB.
    (32..=16 * 1024 - 7).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_graphic())
}
fn is_loopback(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("[::1]")
    )
}
fn validate_url(value: &str, name: &str, allow_loopback: bool) -> Result<Url> {
    let url = Url::parse(value)
        .map_err(|_| Error::new("configuration", format!("{name} must be an absolute URL")))?;
    if (url.scheme() != "https" && !(allow_loopback && url.scheme() == "http" && is_loopback(&url)))
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::new(
            "configuration",
            format!("{name} requires HTTPS and no credentials, query, or fragment"),
        ));
    }
    Ok(url)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn env() -> HashMap<String, String> {
        [
            ("TODO_PUBLIC_URL", "http://127.0.0.1:8080"),
            ("TODO_ADMIN_TOKEN", "admin-012345678901234567890123456789"),
            (
                "TODO_DEV_MCP_TOKEN",
                "client-012345678901234567890123456789",
            ),
            (
                "TODO_ENCRYPTION_KEY",
                "1111111111111111111111111111111111111111111111111111111111111111",
            ),
            (
                "MICROSOFT_CLIENT_ID",
                "00000000-0000-0000-0000-000000000001",
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    }
    #[test]
    fn canonicalizes_uuid_configuration_before_constructing_authority_urls() {
        let mut e = env();
        e.insert(
            "MICROSOFT_CLIENT_ID".into(),
            "urn:uuid:00000000-0000-0000-0000-000000000001".into(),
        );
        e.insert(
            "MICROSOFT_TENANT".into(),
            "{00000000-0000-0000-0000-000000000002}".into(),
        );
        let config = Config::parse(&e).unwrap();
        assert_eq!(
            config.microsoft_client_id,
            "00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(
            config.microsoft_tenant,
            "00000000-0000-0000-0000-000000000002"
        );
    }
    #[test]
    fn rejects_secrets_that_cannot_be_used_in_bearer_headers() {
        for key in ["TODO_ADMIN_TOKEN", "TODO_DEV_MCP_TOKEN"] {
            for invalid in [
                "with space".repeat(5),
                "中文".repeat(20),
                "x".repeat(16 * 1024),
            ] {
                let mut e = env();
                e.insert(key.into(), invalid);
                assert!(Config::parse(&e).is_err(), "accepted unusable {key}");
            }
        }
    }
    #[test]
    fn development_is_loopback_only() {
        let mut e = env();
        assert!(Config::parse(&e).is_ok());
        e.insert("TODO_BIND".into(), "0.0.0.0:8080".into());
        assert!(Config::parse(&e).is_err());
    }
    #[test]
    fn rejects_zero_key_and_reused_secrets() {
        let mut e = env();
        e.insert("TODO_ENCRYPTION_KEY".into(), "0".repeat(64));
        assert!(Config::parse(&e).is_err());
        let mut e = env();
        e.insert("TODO_ADMIN_TOKEN".into(), e["TODO_DEV_MCP_TOKEN"].clone());
        assert!(Config::parse(&e).is_err());
    }
    #[test]
    fn production_pins_issuer_jwks_and_resource() {
        let mut e = env();
        e.remove("TODO_DEV_MCP_TOKEN");
        e.insert("TODO_PUBLIC_URL".into(), "https://todo.example.com".into());
        for (k, v) in [
            ("TODO_OAUTH_ISSUER", "https://auth.example.com/auth"),
            ("TODO_OAUTH_JWKS_URL", "https://auth.example.com/certs"),
            ("TODO_OAUTH_AUDIENCE", "https://todo.example.com/mcp"),
            ("TODO_OAUTH_OWNER_SUBJECT", "owner"),
        ] {
            e.insert(k.into(), v.into());
        }
        assert!(Config::parse(&e).is_ok());
        e.insert(
            "TODO_OAUTH_JWKS_URL".into(),
            "https://attacker.example/certs".into(),
        );
        assert!(Config::parse(&e).is_err());
    }
    #[test]
    fn normalizes_public_origin_and_rejects_sample_secrets() {
        let mut e = env();
        e.insert("TODO_PUBLIC_URL".into(), "http://LOCALHOST:80".into());
        let parsed = Config::parse(&e).unwrap();
        assert_eq!(parsed.public_url, "http://localhost");
        e.insert(
            "TODO_ADMIN_TOKEN".into(),
            "replace-with-at-least-32-random-characters".into(),
        );
        assert!(Config::parse(&e).is_err());
        let mut e = env();
        e.insert("MICROSOFT_CLIENT_ID".into(), uuid::Uuid::nil().to_string());
        assert!(Config::parse(&e).is_err());
    }
}
