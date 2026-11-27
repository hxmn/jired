//! A Jira on a loopback port, for the tests that have to reach the real HTTP
//! client.
//!
//! Every end-to-end test runs against the fixture client, which stands in for
//! [`crate::api::rest::Rest`] above the transport, so retries, error mapping,
//! paging and where the credential goes were tested by nothing; the bugs found
//! there — a POST sent twice, a search cut short, a token sent to another host
//! — lived exactly in that gap. A plain `TcpListener` and a closure that
//! answers each request is all it takes, and keeps a mock-server crate out of
//! the dependency tree.

use crate::api::http::{HttpClient, TlsOptions};
use crate::api::{AuthKind, Connection, Deployment};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::{Arc, Mutex};

/// One request as the server read it.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    /// Path and query, as sent.
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }
}

/// What to answer with.
pub enum Reply {
    Json(u16, serde_json::Value),
    /// Status, extra header lines, body.
    With(u16, Vec<String>, String),
    /// Bytes written as they are, connection closed after: a body cut short.
    Raw(String),
}

pub type Seen = Arc<Mutex<Vec<Request>>>;

/// The token every loopback client sends, distinctive enough to search for.
pub const TOKEN: &str = "loopback-secret-7f3a";

/// Start a server answering with `answer`; its URL, and the requests seen.
pub fn serve(answer: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> (String, Seen) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let answer = Arc::new(answer);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let (answer, log) = (answer.clone(), log.clone());
            // A thread a connection, so requests made at once are answered at
            // once, as a server would.
            std::thread::spawn(move || answer_one(stream, &*answer, &log));
        }
    });
    (url, seen)
}

fn answer_one(mut stream: std::net::TcpStream, answer: &dyn Fn(&Request) -> Reply, log: &Seen) {
    let Some(request) = read_request(&stream) else {
        return;
    };
    let reply = answer(&request);
    log.lock().unwrap().push(request);
    let bytes = match reply {
        Reply::Json(status, v) => {
            let body = v.to_string();
            head(status, &["Content-Type: application/json".into()], &body)
        }
        Reply::With(status, headers, body) => head(status, &headers, &body),
        Reply::Raw(text) => text,
    };
    let _ = stream.write_all(bytes.as_bytes());
}

fn head(status: u16, headers: &[String], body: &str) -> String {
    let mut out = format!("HTTP/1.1 {status} X\r\n");
    for h in headers {
        out.push_str(h);
        out.push_str("\r\n");
    }
    out.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    out
}

fn read_request(stream: &std::net::TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
            break;
        }
        if let Some((n, v)) = line.split_once(':') {
            headers.push((n.trim().to_string(), v.trim().to_string()));
        }
    }
    let length = headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

/// The real HTTP client, pointed at `url`, carrying [`TOKEN`].
pub fn client(url: &str, deployment: Deployment) -> HttpClient {
    let (auth_kind, identity) = match deployment {
        Deployment::Cloud => (AuthKind::ApiToken, Some("someone@example.invalid".into())),
        Deployment::DataCenter => (AuthKind::Pat, None),
    };
    HttpClient::new(Connection {
        base_url: url.to_string(),
        deployment,
        auth_kind,
        identity,
        token: TOKEN.to_string().into(),
        tls: TlsOptions::default(),
    })
    .unwrap()
}
