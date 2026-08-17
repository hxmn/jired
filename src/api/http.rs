//! Shared HTTP core: TLS, auth headers, rate limiting, retry [M1.6].

use crate::api::{AuthKind, Connection, Deployment};
use crate::config::Resolver;
use crate::error::{JiredError, Result};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{Method, StatusCode};
use secrecy::ExposeSecret;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Default)]
pub struct TlsOptions {
    pub ca_path: Option<PathBuf>,
    pub insecure: bool,
}

impl TlsOptions {
    pub fn from_resolver(r: &mut Resolver) -> Result<Self> {
        Ok(Self {
            ca_path: r.get_value("tls.ca_cert")?.map(PathBuf::from),
            insecure: r.get_bool("tls.insecure")?.unwrap_or(false),
        })
    }
}

/// Requests per second allowed before jired throttles itself.
///
/// Cloud enforces per-user limits that a 10k-issue fetch will hit; Data Center
/// instances are usually smaller machines, so they get a gentler default.
fn default_rate(deployment: Deployment) -> f64 {
    match deployment {
        Deployment::Cloud => 8.0,
        Deployment::DataCenter => 4.0,
    }
}

const MAX_RETRIES: u32 = 5;

/// Token bucket shared by all requests of one client.
struct RateLimiter {
    inner: Mutex<Bucket>,
    rate: f64,
    burst: f64,
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    fn new(rate: f64) -> Self {
        let burst = (rate * 2.0).max(1.0);
        Self {
            inner: Mutex::new(Bucket {
                tokens: burst,
                last: Instant::now(),
            }),
            rate,
            burst,
        }
    }

    async fn acquire(&self) {
        loop {
            let wait = {
                let mut b = self.inner.lock().await;
                let now = Instant::now();
                let elapsed = now.duration_since(b.last).as_secs_f64();
                b.last = now;
                b.tokens = (b.tokens + elapsed * self.rate).min(self.burst);
                if b.tokens >= 1.0 {
                    b.tokens -= 1.0;
                    return;
                }
                Duration::from_secs_f64((1.0 - b.tokens) / self.rate)
            };
            tokio::time::sleep(wait).await;
        }
    }
}

pub struct HttpClient {
    client: reqwest::Client,
    base_url: String,
    deployment: Deployment,
    limiter: Arc<RateLimiter>,
}

/// rustls needs exactly one process-wide crypto provider. jired picks `ring`
/// so the build stays pure Rust; installing it is idempotent and racing
/// installs are harmless because a second call simply loses.
fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

impl HttpClient {
    pub fn new(conn: Connection) -> Result<Self> {
        install_crypto_provider();
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(
            AUTHORIZATION,
            auth_header(&conn).map_err(|e| {
                JiredError::auth(format!("cannot build the authorization header: {e}"))
            })?,
        );
        // Atlassian rejects some clients without this on POST/PUT.
        headers.insert("X-Atlassian-Token", HeaderValue::from_static("no-check"));

        let mut builder = reqwest::Client::builder()
            .default_headers(headers)
            .user_agent(concat!("jired/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(60))
            .connect_timeout(Duration::from_secs(15));

        if conn.tls.insecure {
            builder = builder.danger_accept_invalid_certs(true);
        }
        if let Some(ca) = &conn.tls.ca_path {
            let pem = std::fs::read(ca).map_err(|e| {
                JiredError::usage(format!("cannot read CA bundle {}: {e}", ca.display()))
            })?;
            // A bundle may hold several certificates; add them all.
            for cert in reqwest::Certificate::from_pem_bundle(&pem).map_err(|e| {
                JiredError::usage(format!("invalid CA bundle {}: {e}", ca.display()))
            })? {
                builder = builder.add_root_certificate(cert);
            }
        }

        let client = builder
            .build()
            .map_err(|e| JiredError::network(format!("cannot create the HTTP client: {e}")))?;

        Ok(Self {
            client,
            base_url: conn.base_url.trim_end_matches('/').to_string(),
            deployment: conn.deployment,
            limiter: Arc::new(RateLimiter::new(default_rate(conn.deployment))),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn deployment(&self) -> Deployment {
        self.deployment
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub async fn get_json(&self, path: &str) -> Result<serde_json::Value> {
        self.request_json(Method::GET, path, None).await
    }

    pub async fn post_json(
        &self,
        path: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.request_json(Method::POST, path, Some(body)).await
    }

    pub async fn put_json(&self, path: &str, body: serde_json::Value) -> Result<serde_json::Value> {
        self.request_json(Method::PUT, path, Some(body)).await
    }

    pub async fn delete(&self, path: &str) -> Result<()> {
        self.request_json(Method::DELETE, path, None).await?;
        Ok(())
    }

    /// Issue a request, retrying throttling and transient server failures.
    pub async fn request_json(
        &self,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let url = self.url(path);
        let mut attempt = 0u32;
        loop {
            self.limiter.acquire().await;
            let mut req = self.client.request(method.clone(), &url);
            if let Some(b) = &body {
                req = req.header(CONTENT_TYPE, "application/json").json(b);
            }
            let response = req.send().await;

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        // 204 and empty bodies are normal for PUT/DELETE.
                        let text = resp.text().await.unwrap_or_default();
                        if text.trim().is_empty() {
                            return Ok(serde_json::Value::Null);
                        }
                        return serde_json::from_str(&text).map_err(|e| {
                            JiredError::generic(format!("{method} {path}: malformed JSON: {e}"))
                        });
                    }

                    if should_retry_status(status) && attempt < MAX_RETRIES {
                        let wait = retry_after(&resp).unwrap_or_else(|| backoff(attempt));
                        tracing::warn!(
                            "{method} {path} returned {status}; retrying in {:.1}s \
                             (attempt {}/{MAX_RETRIES})",
                            wait.as_secs_f64(),
                            attempt + 1
                        );
                        tokio::time::sleep(wait).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(self.status_error(status, &method, path, resp).await);
                }
                Err(e) => {
                    if (e.is_timeout() || e.is_connect()) && attempt < MAX_RETRIES {
                        let wait = backoff(attempt);
                        tracing::warn!(
                            "{method} {path}: {e}; retrying in {:.1}s",
                            wait.as_secs_f64()
                        );
                        tokio::time::sleep(wait).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(network_error(&e, &method, path));
                }
            }
        }
    }

    pub async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        self.limiter.acquire().await;
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| network_error(&e, &Method::GET, url))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(JiredError::network(format!(
                "GET {url} failed with {status}"
            )));
        }
        Ok(resp
            .bytes()
            .await
            .map_err(|e| JiredError::network(format!("reading {url}: {e}")))?
            .to_vec())
    }

    /// Multipart upload used by the attachment endpoints.
    pub async fn post_multipart(
        &self,
        path: &str,
        filename: &str,
        bytes: Vec<u8>,
    ) -> Result<serde_json::Value> {
        self.limiter.acquire().await;
        let mime = mime_guess::from_path(filename)
            .first_or_octet_stream()
            .to_string();
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename.to_string())
            .mime_str(&mime)
            .map_err(|e| JiredError::generic(format!("invalid MIME type for {filename}: {e}")))?;
        let form = reqwest::multipart::Form::new().part("file", part);
        let url = self.url(path);

        let resp = self
            .client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| network_error(&e, &Method::POST, path))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(self.status_error(status, &Method::POST, path, resp).await);
        }
        let text = resp.text().await.unwrap_or_default();
        if text.trim().is_empty() {
            return Ok(serde_json::Value::Null);
        }
        Ok(serde_json::from_str(&text)?)
    }

    async fn status_error(
        &self,
        status: StatusCode,
        method: &Method,
        path: &str,
        resp: reqwest::Response,
    ) -> JiredError {
        let body = resp.text().await.unwrap_or_default();
        let detail = extract_error_messages(&body);
        let message = match detail {
            Some(d) => format!("{method} {path} failed with {status}: {d}"),
            None => format!("{method} {path} failed with {status}"),
        };
        let err = match status {
            StatusCode::UNAUTHORIZED => {
                JiredError::auth(message).with_hint("run `jired init --force` to re-authenticate")
            }
            StatusCode::FORBIDDEN => JiredError::auth(message)
                .with_hint("the account lacks permission for this operation"),
            StatusCode::NOT_FOUND => JiredError::generic(message)
                .with_hint("the issue may not exist, or the account cannot see it"),
            s if s.is_server_error() => JiredError::network(message),
            _ => JiredError::generic(message),
        };
        err.with_detail("status", status.as_u16())
            .with_detail("path", path.to_string())
    }
}

fn auth_header(
    conn: &Connection,
) -> std::result::Result<HeaderValue, reqwest::header::InvalidHeaderValue> {
    let token = conn.token.expose_secret();
    let mut value = match conn.auth_kind {
        // Cloud API tokens authenticate as basic auth with the account email.
        AuthKind::ApiToken | AuthKind::Basic => {
            let user = conn.identity.clone().unwrap_or_default();
            HeaderValue::from_str(&format!("Basic {}", b64(&format!("{user}:{token}"))))?
        }
        AuthKind::Pat | AuthKind::OAuth2 => HeaderValue::from_str(&format!("Bearer {token}"))?,
        AuthKind::Cookie => HeaderValue::from_str(token)?,
    };
    // Marking the header sensitive keeps it out of reqwest's own debug output.
    value.set_sensitive(true);
    Ok(value)
}

fn b64(s: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn should_retry_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || matches!(status.as_u16(), 502..=504)
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let value = resp.headers().get("retry-after")?.to_str().ok()?;
    value
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        // Cap so a hostile or misconfigured header cannot hang a command.
        .map(|d| d.min(Duration::from_secs(120)))
}

fn backoff(attempt: u32) -> Duration {
    let base = 2f64.powi(attempt as i32).min(30.0);
    // Jitter avoids synchronized retries when several issues fail at once.
    let jitter = (attempt as f64 * 0.37).fract() * 0.5;
    Duration::from_secs_f64(base * (1.0 + jitter))
}

fn network_error(e: &reqwest::Error, method: &Method, path: &str) -> JiredError {
    let kind = if e.is_timeout() {
        "timed out"
    } else if e.is_connect() {
        "cannot connect"
    } else {
        "request failed"
    };
    let mut err = JiredError::network(format!("{method} {path}: {kind}: {e}"));
    let text = e.to_string().to_lowercase();
    if text.contains("certificate") || text.contains("tls") || text.contains("ssl") {
        err = err.with_hint(
            "for a self-signed Data Center instance pass --ca-cert <bundle.pem>, \
             or --insecure to skip verification",
        );
    }
    err
}

/// Jira reports problems in `errorMessages` and `errors`; surface both.
fn extract_error_messages(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let mut parts = Vec::new();
    if let Some(arr) = v.get("errorMessages").and_then(|m| m.as_array()) {
        parts.extend(arr.iter().filter_map(|m| m.as_str().map(str::to_string)));
    }
    if let Some(obj) = v.get("errors").and_then(|m| m.as_object()) {
        parts.extend(
            obj.iter()
                .map(|(k, v)| format!("{k}: {}", v.as_str().unwrap_or(""))),
        );
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_reference_vectors() {
        assert_eq!(b64(""), "");
        assert_eq!(b64("f"), "Zg==");
        assert_eq!(b64("fo"), "Zm8=");
        assert_eq!(b64("foo"), "Zm9v");
        assert_eq!(b64("foob"), "Zm9vYg==");
        assert_eq!(b64("fooba"), "Zm9vYmE=");
        assert_eq!(b64("foobar"), "Zm9vYmFy");
        assert_eq!(b64("a@b.c:tok"), "YUBiLmM6dG9r");
    }

    #[test]
    fn only_throttling_and_gateway_failures_are_retried() {
        assert!(should_retry_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(should_retry_status(StatusCode::BAD_GATEWAY));
        assert!(should_retry_status(StatusCode::SERVICE_UNAVAILABLE));
        assert!(!should_retry_status(StatusCode::UNAUTHORIZED));
        assert!(!should_retry_status(StatusCode::NOT_FOUND));
        // A 500 usually means a bad payload; retrying just repeats the mistake.
        assert!(!should_retry_status(StatusCode::INTERNAL_SERVER_ERROR));
    }

    #[test]
    fn backoff_grows_and_stays_bounded() {
        let waits: Vec<f64> = (0..6).map(|a| backoff(a).as_secs_f64()).collect();
        assert!(waits[0] < waits[3]);
        assert!(waits.iter().all(|w| *w <= 45.0), "{waits:?}");
    }

    #[test]
    fn jira_error_bodies_are_summarized() {
        let body = r#"{"errorMessages":["Issue does not exist"],"errors":{}}"#;
        assert_eq!(
            extract_error_messages(body).as_deref(),
            Some("Issue does not exist")
        );
        let body = r#"{"errorMessages":[],"errors":{"summary":"is required"}}"#;
        assert_eq!(
            extract_error_messages(body).as_deref(),
            Some("summary: is required")
        );
        assert!(extract_error_messages("not json").is_none());
    }

    #[tokio::test]
    async fn rate_limiter_throttles_beyond_the_burst() {
        let limiter = RateLimiter::new(10.0);
        let start = Instant::now();
        // burst is 2x rate, so the 25th request must have waited.
        for _ in 0..25 {
            limiter.acquire().await;
        }
        assert!(
            start.elapsed() >= Duration::from_millis(300),
            "expected throttling, took {:?}",
            start.elapsed()
        );
    }
}
