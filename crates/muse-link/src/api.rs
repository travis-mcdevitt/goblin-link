//! The Muse device API client: leased-VM lookup and device-token rotation.
//!
//! Port of `muse_api.py`. Two calls over HTTPS:
//!
//! * [`fetch_vms`] — `GET /fetch_vms` with the device access token as a bearer,
//!   returning the leased VMs and a fresh per-VM bearer for each.
//! * [`refresh_device_token`] — `POST /device_token/refresh` with the refresh
//!   token, rotating the token pair.
//!
//! Two reference rules are preserved exactly, because breaking either silently
//! bricks a pairing:
//!
//! * The access token is **never** presented to the refresh endpoint. The
//!   server can accept it and return 200 with replacement tokens that every
//!   other endpoint then rejects, overwriting working credentials.
//! * Apps now hand over refresh tokens already carrying a `hatch_refresh:`
//!   prefix; we take only the part after the last `:` and add the prefix
//!   ourselves, since doubling it makes the server reject it.

use std::fmt;
use std::time::Duration;

use serde_json::Value;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::error::ApiError;
use crate::http::{self, HttpHeader};
use crate::tls;

/// Default device API base.
pub const API_BASE: &str = "https://api.muse.ai";
const FETCH_PATH: &str = "/fetch_vms";
const REFRESH_PATH: &str = "/device_token/refresh";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The URL that API paths are appended to: `api_url_v2`, or the Muse API.
pub fn api_root(api_url_v2: &str) -> String {
    let root = if api_url_v2.is_empty() {
        API_BASE
    } else {
        api_url_v2
    };
    root.trim_end_matches('/').to_owned()
}

/// The `User-Agent` for every request: the device model (when
/// readable), the OS/arch, and the crate marker.
pub fn user_agent() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let mut details: Vec<String> = Vec::new();
    if let Ok(model) = std::fs::read_to_string("/proc/device-tree/model") {
        let model = model.replace('\0', "");
        let model = model.trim();
        if !model.is_empty() {
            details.push(model.to_owned());
        }
    }
    details.push(format!(
        "{} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    let joined = details.join("; ");
    format!("musegadget/{version} ({joined}) goblin-link-rs/{version}")
}

/// A leased VM the device can connect to.
#[derive(Clone, PartialEq, Eq)]
pub struct Vm {
    /// WebSocket URL or host of the VM.
    pub vm_url: String,
    /// Per-VM bearer token for the Noise upgrade (secret).
    pub vm_auth_token: String,
    /// Human-readable VM name.
    pub vm_name: String,
    /// VM identifier.
    pub vm_id: String,
    /// Whether this is the device's default VM.
    pub is_default: bool,
}

impl fmt::Debug for Vm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Vm")
            .field("vm_url", &self.vm_url)
            .field("vm_auth_token", &"<redacted>")
            .field("vm_name", &self.vm_name)
            .field("vm_id", &self.vm_id)
            .field("is_default", &self.is_default)
            .finish()
    }
}

/// A rotated device-token pair.
#[derive(Clone, PartialEq, Eq)]
pub struct Tokens {
    /// New device access token (secret).
    pub access_token: String,
    /// New refresh token (secret).
    pub refresh_token: String,
}

impl fmt::Debug for Tokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Tokens(<redacted>)")
    }
}

struct Endpoint {
    host: String,
    port: u16,
    base_path: String,
}

fn parse_root(root: &str) -> Result<Endpoint, ApiError> {
    let (scheme, rest) = root
        .split_once("://")
        .ok_or_else(|| ApiError::Request("API root has no scheme".into()))?;
    if scheme != "https" {
        return Err(ApiError::Request("API root must be https".into()));
    }
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (rest, String::new()),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_owned(),
            p.parse::<u16>()
                .map_err(|_| ApiError::Request("API root has a bad port".into()))?,
        ),
        None => (authority.to_owned(), 443),
    };
    if host.is_empty() {
        return Err(ApiError::Request("API root has no host".into()));
    }
    Ok(Endpoint {
        host,
        port,
        base_path: path.trim_end_matches('/').to_owned(),
    })
}

async fn connect_tls(
    endpoint: &Endpoint,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>, ApiError> {
    let tcp = TcpStream::connect((endpoint.host.as_str(), endpoint.port))
        .await
        .map_err(|e| ApiError::Transport(e.to_string()))?;
    let connector = TlsConnector::from(tls::client_config());
    let server_name = rustls_pki_types::ServerName::try_from(endpoint.host.clone())
        .map_err(|_| ApiError::Request("API root host is not a valid DNS name".into()))?;
    connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| ApiError::Transport(e.to_string()))
}

/// Looks up the leased VMs for `access_token`.
///
/// A 401 ([`ApiError::is_unauthorized`]) means the device token was rejected
/// and a retry will not help; a transport error ([`ApiError::is_transient`])
/// is worth retrying.
pub async fn fetch_vms(access_token: &str, root: &str) -> Result<Vec<Vm>, ApiError> {
    let endpoint = parse_root(root)?;
    let path = format!("{}{}", endpoint.base_path, FETCH_PATH);
    let ua = user_agent();
    let auth = format!("Bearer {access_token}");
    let headers = [
        HttpHeader {
            name: "Authorization",
            value: &auth,
        },
        HttpHeader {
            name: "X-API-Version",
            value: "1.0.0",
        },
        HttpHeader {
            name: "User-Agent",
            value: &ua,
        },
        HttpHeader {
            name: "Accept",
            value: "application/json",
        },
    ];

    let resp = tokio::time::timeout(REQUEST_TIMEOUT, async {
        let mut stream = connect_tls(&endpoint).await?;
        http::request(&mut stream, &endpoint.host, "GET", &path, &headers, b"").await
    })
    .await
    .map_err(|_| ApiError::Transport("request timed out".into()))??;

    if resp.status == 401 {
        return Err(ApiError::Http(401));
    }
    if !(200..300).contains(&resp.status) {
        return Err(ApiError::Http(resp.status));
    }
    parse_vm_list(&resp.body)
}

fn parse_vm_list(body: &[u8]) -> Result<Vec<Vm>, ApiError> {
    let data: Value = serde_json::from_slice(body).map_err(|_| ApiError::BadResponse)?;
    let obj = data.as_object().ok_or(ApiError::BadResponse)?;
    if obj.get("error_title").is_some_and(|v| !v.is_null())
        || obj.get("backend_error_code").is_some_and(|v| !v.is_null())
    {
        return Err(ApiError::BadResponse);
    }
    let list = obj
        .get("vm_list")
        .and_then(Value::as_array)
        .ok_or(ApiError::BadResponse)?;
    let mut vms = Vec::new();
    for entry in list {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let vm_url = entry
            .get("vm_ws_url")
            .or_else(|| entry.get("vm_url"))
            .and_then(Value::as_str);
        let vm_token = entry.get("vm_auth_token").and_then(Value::as_str);
        if let (Some(url), Some(token)) = (vm_url, vm_token) {
            if url.is_empty() || token.is_empty() {
                continue;
            }
            vms.push(Vm {
                vm_url: url.to_owned(),
                vm_auth_token: token.to_owned(),
                vm_name: entry
                    .get("vm_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                vm_id: entry
                    .get("vm_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                is_default: entry
                    .get("default")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            });
        }
    }
    Ok(vms)
}

/// Rotates the device token pair with the refresh token.
///
/// Returns the new pair. A 401 means the pairing is gone and the device must
/// be paired again. `sdk_token`, when present, is sent so a token set after
/// pairing takes effect.
pub async fn refresh_device_token(
    refresh_token: &str,
    device_id: &str,
    root: &str,
    sdk_token: Option<&str>,
) -> Result<Tokens, ApiError> {
    let endpoint = parse_root(root)?;
    let path = format!("{}{}", endpoint.base_path, REFRESH_PATH);
    // Take only the part after the last ':' and add the prefix ourselves.
    let raw_refresh = refresh_token.rsplit(':').next().unwrap_or(refresh_token);
    let auth = format!("Bearer hatch_refresh:{raw_refresh}");
    let ua = user_agent();

    let mut body = serde_json::Map::new();
    body.insert("device_id".into(), Value::String(device_id.to_owned()));
    if let Some(token) = sdk_token {
        body.insert("sdk_token".into(), Value::String(token.to_owned()));
    }
    let body = serde_json::to_vec(&Value::Object(body)).map_err(|_| ApiError::BadResponse)?;

    let headers = [
        HttpHeader {
            name: "Authorization",
            value: &auth,
        },
        HttpHeader {
            name: "Content-Type",
            value: "application/json",
        },
        HttpHeader {
            name: "User-Agent",
            value: &ua,
        },
    ];

    let resp = tokio::time::timeout(REQUEST_TIMEOUT, async {
        let mut stream = connect_tls(&endpoint).await?;
        http::request(&mut stream, &endpoint.host, "POST", &path, &headers, &body).await
    })
    .await
    .map_err(|_| ApiError::Transport("request timed out".into()))??;

    if !(200..300).contains(&resp.status) {
        return Err(ApiError::Http(resp.status));
    }
    parse_tokens(&resp.body)
}

fn parse_tokens(body: &[u8]) -> Result<Tokens, ApiError> {
    let mut data: Value = serde_json::from_slice(body).map_err(|_| ApiError::BadResponse)?;
    if let Some(payload) = data.get("payload").filter(|p| p.is_object()).cloned() {
        data = payload;
    }
    let obj = data.as_object().ok_or(ApiError::BadResponse)?;
    let access = obj.get("access_token").and_then(Value::as_str);
    let refresh = obj.get("refresh_token").and_then(Value::as_str);
    match (access, refresh) {
        (Some(a), Some(r)) if !a.is_empty() && !r.is_empty() => Ok(Tokens {
            access_token: a.to_owned(),
            refresh_token: r.to_owned(),
        }),
        _ => Err(ApiError::BadResponse),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_root_defaults_and_strips_slashes() {
        assert_eq!(api_root(""), "https://api.muse.ai");
        assert_eq!(api_root("https://x.example/"), "https://x.example");
    }

    #[test]
    fn parse_root_splits_host_port_path() {
        let e = parse_root("https://host.example:8443/hatch").unwrap();
        assert_eq!(e.host, "host.example");
        assert_eq!(e.port, 8443);
        assert_eq!(e.base_path, "/hatch");
        let e = parse_root("https://api.muse.ai").unwrap();
        assert_eq!(e.port, 443);
        assert_eq!(e.base_path, "");
    }

    #[test]
    fn parse_root_rejects_non_https() {
        assert!(parse_root("http://x").is_err());
        assert!(parse_root("ftp://x").is_err());
    }

    #[test]
    fn vm_list_parsed_and_default_detected() {
        let body = br#"{"vm_list":[
            {"vm_ws_url":"wss://a","vm_auth_token":"t1","vm_name":"A","vm_id":"id1"},
            {"vm_url":"wss://b","vm_auth_token":"t2","vm_name":"B","vm_id":"id2","default":true}
        ]}"#;
        let vms = parse_vm_list(body).unwrap();
        assert_eq!(vms.len(), 2);
        assert!(!vms[0].is_default);
        assert!(vms[1].is_default);
        assert_eq!(vms[1].vm_url, "wss://b");
    }

    #[test]
    fn vm_list_skips_incomplete_entries_and_rejects_errors() {
        let vms = parse_vm_list(br#"{"vm_list":[{"vm_url":"wss://a"}]}"#).unwrap();
        assert!(vms.is_empty());
        assert!(parse_vm_list(br#"{"error_title":"nope"}"#).is_err());
        assert!(parse_vm_list(br#"[]"#).is_err());
    }

    #[test]
    fn tokens_unwrap_payload_and_require_both() {
        let t = parse_tokens(br#"{"payload":{"access_token":"a","refresh_token":"r"}}"#).unwrap();
        assert_eq!(t.access_token, "a");
        assert_eq!(t.refresh_token, "r");
        assert!(parse_tokens(br#"{"access_token":"a"}"#).is_err());
    }

    #[test]
    fn user_agent_has_marker() {
        assert!(user_agent().contains("goblin-link-rs/"));
        assert!(user_agent().starts_with("musegadget/"));
    }

    #[test]
    fn tokens_debug_is_redacted() {
        let t = Tokens {
            access_token: "secret".into(),
            refresh_token: "secret".into(),
        };
        assert_eq!(format!("{t:?}"), "Tokens(<redacted>)");
    }
}
