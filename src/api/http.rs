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
            ca_path: r.get_quiet("tls.ca_cert")?.map(PathBuf::from),
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

/// An HTML reply, and the URL the redirects finished at.
pub struct WebPage {
    pub url: String,
    pub html: String,
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
pub(crate) fn install_crypto_provider() {
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
            // Only Data Center's Move wizard needs this, and it needs it twice
            // over: the session cookie carries the wizard's own state between
            // its two steps, and the XSRF cookie is half of the pair the
            // `atl_token` in the form is checked against.
            .cookie_store(true)
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
            // Every request, at `-v`. Without this, `-v` promised REST paths
            // and printed nothing, so the way to find out whether a call was
            // even made was to read the source — which is exactly the question
            // a debug flag exists to answer. The body stays at `-vv`: it can
            // carry issue content, and a token never travels in one.
            let started = std::time::Instant::now();
            tracing::debug!("{method} {path}");
            if let Some(b) = &body {
                tracing::trace!("{method} {path} body: {b}");
            }
            let response = req.send().await;

            match response {
                Ok(resp) => {
                    let status = resp.status();
                    tracing::debug!(
                        "{method} {path} → {} in {}ms",
                        status.as_u16(),
                        started.elapsed().as_millis()
                    );
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

    /// Fetch a Jira page that is not part of the REST API.
    ///
    /// One operation needs this: re-parenting a sub-task on Data Center, which
    /// Jira exposes only as the web action behind the Move dialog. The reply is
    /// HTML, so `Accept` has to say so — the client-wide default asks for JSON,
    /// and Jira honours it by answering some web actions with a JSON error
    /// instead of the form the caller came for.
    pub async fn get_page(&self, path: &str) -> Result<WebPage> {
        self.limiter.acquire().await;
        let url = self.url(path);
        tracing::debug!("GET {path}");
        let resp = self
            .client
            .get(&url)
            .header(ACCEPT, "text/html,application/xhtml+xml")
            .send()
            .await
            .map_err(|e| network_error(&e, &Method::GET, path))?;
        self.page(Method::GET, path, resp).await
    }

    /// Submit an HTML form, following Jira's redirect to the next step.
    ///
    /// `action` is whatever the form itself declared, which is a path far more
    /// often than a URL; both are accepted so the caller can pass it through
    /// unread.
    pub async fn post_form(&self, action: &str, fields: &[(String, String)]) -> Result<WebPage> {
        self.limiter.acquire().await;
        let url = if action.starts_with("http://") || action.starts_with("https://") {
            action.to_string()
        } else {
            self.url(action)
        };
        tracing::debug!("POST {action} ({} field(s))", fields.len());
        // The values can name an issue and its new parent, which is content;
        // the same rule the JSON bodies follow puts them at `-vv`.
        tracing::trace!("POST {action} form: {fields:?}");
        let resp = self
            .client
            .post(&url)
            .header(ACCEPT, "text/html,application/xhtml+xml")
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form_encode(fields))
            .send()
            .await
            .map_err(|e| network_error(&e, &Method::POST, action))?;
        self.page(Method::POST, action, resp).await
    }

    async fn page(&self, method: Method, path: &str, resp: reqwest::Response) -> Result<WebPage> {
        let status = resp.status();
        // Where the redirects stopped, which is how the wizard says which step
        // it is on — and, at the end, that it has left the wizard entirely.
        let url = resp.url().to_string();
        tracing::debug!("{method} {path} → {} at {url}", status.as_u16());
        if !status.is_success() {
            return Err(self.status_error(status, &method, path, resp).await);
        }
        Ok(WebPage {
            url,
            html: resp.text().await.unwrap_or_default(),
        })
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

        // The REST path is jired's business, not the user's. What they need is
        // what jired was trying to do and what the server said back; the path
        // stays in `details`, which `-v` prints.
        let subject = describe_target(path);
        let message = match (&subject, &detail) {
            (Some(s), Some(d)) => format!("{s}: {d} (HTTP {})", status.as_u16()),
            (Some(s), None) => format!("{s}: the server refused with HTTP {}", status.as_u16()),
            (None, Some(d)) => format!("{d} (HTTP {})", status.as_u16()),
            (None, None) => format!("the server refused with HTTP {}", status.as_u16()),
        };

        let err = match status {
            StatusCode::UNAUTHORIZED => JiredError::auth(message)
                .with_hint("the credential was rejected — run `jired init --force`"),
            StatusCode::FORBIDDEN => JiredError::auth(message)
                .with_hint("the account lacks permission for this operation"),
            // A 404 on a probe endpoint means the URL is not a Jira, which is a
            // very different problem from an issue that does not exist.
            StatusCode::NOT_FOUND if is_probe_path(path) => JiredError::usage(format!(
                "no Jira REST API at this URL (HTTP {})",
                status.as_u16()
            ))
            .with_hint(
                "check server.url — it should be the site root, e.g. https://acme.atlassian.net",
            ),
            // The user typed the key; a key that does not resolve is their input.
            StatusCode::NOT_FOUND => JiredError::usage(message)
                .with_hint("the issue may not exist, or the account cannot see it"),
            s if s.is_server_error() => JiredError::network(message)
                .with_hint("the server failed, not jired — retrying later usually works"),
            _ => JiredError::generic(message),
        };
        err.with_detail("status", status.as_u16())
            .with_detail("method", method.to_string())
            .with_detail("path", path.to_string())
    }
}

/// Endpoints jired calls to check the server itself rather than an issue.
fn is_probe_path(path: &str) -> bool {
    path.contains("serverInfo") || path.ends_with("/myself")
}

/// Name what a REST path was acting on, in the user's vocabulary.
fn describe_target(path: &str) -> Option<String> {
    let rest = path.split("/issue/").nth(1)?;
    let key = rest.split('/').next()?;
    if key.is_empty() {
        return None;
    }
    let what = if rest.contains("/comment") {
        "comment on "
    } else if rest.contains("/attachments") {
        "attachment on "
    } else if rest.contains("/transitions") {
        "transition of "
    } else if rest.contains("/assignee") {
        "assignee of "
    } else {
        ""
    };
    Some(format!("{what}{key}"))
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

/// `application/x-www-form-urlencoded`, as a browser would send it.
///
/// A space is `+` and everything outside the unreserved set is escaped: an
/// XSRF token carries `|`, and a summary carried in a hidden field can carry
/// anything at all.
fn form_encode(fields: &[(String, String)]) -> String {
    fields
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                urlencoding::encode(k).replace("%20", "+"),
                urlencoding::encode(v).replace("%20", "+")
            )
        })
        .collect::<Vec<_>>()
        .join("&")
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
        "the server did not answer in time"
    } else if e.is_connect() {
        "cannot reach the server"
    } else {
        "the request failed"
    };
    // `reqwest`'s Display is a chain ending in the part that actually explains
    // it; the whole chain repeats the URL and the layer names for no gain.
    let cause = innermost_cause(e);
    let text = format!("{e}").to_lowercase();
    let hint = if text.contains("certificate") || text.contains("tls") || text.contains("ssl") {
        "for a self-signed Data Center instance pass --ca-cert <bundle.pem>, \
         or --insecure to skip verification"
    } else if text.contains("dns") || text.contains("resolve") {
        "the hostname did not resolve — check server.url, and any VPN or proxy you need"
    } else if e.is_timeout() {
        "check your network and any HTTPS_PROXY setting; `jired doctor` verifies connectivity"
    } else {
        "check server.url and your network; `jired doctor` verifies connectivity"
    };
    JiredError::network(format!("{kind}: {cause}"))
        .with_hint(hint)
        .with_detail("method", method.to_string())
        .with_detail("path", path.to_string())
        .with_detail("cause", e.to_string())
}

/// The last link in an error's source chain: the part that says what went wrong.
fn innermost_cause(e: &dyn std::error::Error) -> String {
    let mut current: &dyn std::error::Error = e;
    while let Some(next) = current.source() {
        current = next;
    }
    current.to_string()
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

    /// The credential lives in a header; if that header were ever formatted
    /// into a log or an error, the token would travel with it.
    #[test]
    fn the_authorization_header_never_formats_its_value() {
        use secrecy::SecretString;

        let conn = Connection {
            base_url: "https://fixture.invalid".into(),
            deployment: Deployment::Cloud,
            auth_kind: AuthKind::ApiToken,
            identity: Some("a@b.c".into()),
            token: SecretString::from("leak-canary".to_string()),
            tls: TlsOptions::default(),
        };
        let header = auth_header(&conn).unwrap();
        assert!(header.is_sensitive());
        assert!(
            !format!("{header:?}").contains("leak-canary"),
            "the header debug output exposed the credential"
        );

        // The encoded form does contain it, which is exactly why the header is
        // marked sensitive rather than merely being handled carefully.
        assert!(String::from_utf8_lossy(header.as_bytes()).starts_with("Basic "));
    }

    #[test]
    fn a_bearer_token_is_used_for_personal_access_tokens() {
        use secrecy::SecretString;

        let conn = Connection {
            base_url: "https://jira.corp.example".into(),
            deployment: Deployment::DataCenter,
            auth_kind: AuthKind::Pat,
            identity: None,
            token: SecretString::from("pat-value".to_string()),
            tls: TlsOptions::default(),
        };
        let header = auth_header(&conn).unwrap();
        assert_eq!(
            String::from_utf8_lossy(header.as_bytes()),
            "Bearer pat-value"
        );
        assert!(header.is_sensitive());
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
