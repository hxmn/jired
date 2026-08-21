//! Looking at a server's TLS certificate so a human can decide about it.
//!
//! Self-hosted Data Center instances very often present a certificate signed by
//! a company CA that this machine does not know about. The alternative to asking
//! is `--insecure`, which teaches people to turn verification off for good; so
//! `init` instead fetches the certificate once, shows what it is, and — if the
//! user recognizes it — pins it as a trust anchor for this workspace only.
//!
//! Nothing here verifies anything: the handshake deliberately accepts whatever
//! the server sends, because the whole point is to *display* an untrusted
//! certificate. The decision, and every later connection, stays with rustls.

use crate::error::{JiredError, Result};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

/// What `init` shows the user about an untrusted certificate.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ServerCert {
    pub subject: String,
    pub issuer: String,
    /// SHA-256 of the DER, in the colon-separated hex everyone else prints.
    pub fingerprint: String,
    pub not_before: String,
    pub not_after: String,
    pub names: Vec<String>,
    /// Issuer equals subject: nobody vouched for this but itself.
    pub self_signed: bool,
    /// Outside its validity window right now.
    pub expired: bool,
    /// Whether the hostname being connected to is one the certificate covers.
    pub covers_host: bool,
    /// How many certificates the server sent; more than one means it offered a
    /// chain, so the CA at the end is the thing worth trusting.
    pub chain_len: usize,
    #[serde(skip)]
    pem: String,
}

impl ServerCert {
    /// PEM for the certificate to pin: the last one in the chain, which is the
    /// issuing CA when the server bothered to send one.
    pub fn pem(&self) -> &str {
        &self.pem
    }

    /// A one-line reason this certificate is not trusted by default.
    pub fn summary(&self) -> String {
        if self.expired {
            "the certificate is outside its validity period".into()
        } else if !self.covers_host {
            "the certificate does not list this hostname".into()
        } else if self.self_signed {
            "the certificate is self-signed".into()
        } else {
            "the certificate is signed by a CA this machine does not know".into()
        }
    }
}

/// Connect to `host:port`, accept whatever certificate arrives, and describe it.
pub fn inspect(host: &str, port: u16) -> Result<ServerCert> {
    super::http::install_crypto_provider();

    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));

    let config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnything(provider)))
        .with_no_client_auth();

    let name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|_| JiredError::network(format!("`{host}` is not a valid hostname")))?;
    let mut conn = rustls::ClientConnection::new(Arc::new(config), name)
        .map_err(|e| JiredError::network(format!("cannot start a TLS handshake: {e}")))?;

    let mut sock = connect(host, port)?;
    while conn.is_handshaking() {
        conn.complete_io(&mut sock).map_err(|e| {
            JiredError::network(format!("TLS handshake with {host}:{port} failed: {e}"))
        })?;
    }
    let chain: Vec<Vec<u8>> = conn
        .peer_certificates()
        .unwrap_or(&[])
        .iter()
        .map(|c| c.as_ref().to_vec())
        .collect();
    conn.send_close_notify();
    let _ = conn.complete_io(&mut sock);
    let _ = sock.flush();

    let leaf = chain
        .first()
        .ok_or_else(|| JiredError::network(format!("{host}:{port} sent no certificate")))?;
    // Pin the root of what was offered: trusting the CA keeps working when the
    // leaf is rotated, which for an internal instance is a matter of months.
    let anchor = chain.last().expect("chain is non-empty");
    describe(leaf, anchor, chain.len(), host)
}

fn connect(host: &str, port: u16) -> Result<TcpStream> {
    use std::net::ToSocketAddrs;
    let mut last: Option<String> = None;
    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| JiredError::network(format!("cannot resolve {host}: {e}")))?;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(15)) {
            Ok(s) => {
                let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
                let _ = s.set_write_timeout(Some(Duration::from_secs(15)));
                return Ok(s);
            }
            Err(e) => last = Some(e.to_string()),
        }
    }
    Err(JiredError::network(format!(
        "cannot connect to {host}:{port}{}",
        last.map(|e| format!(": {e}")).unwrap_or_default()
    )))
}

fn describe(leaf: &[u8], pinned_der: &[u8], chain_len: usize, host: &str) -> Result<ServerCert> {
    use x509_parser::prelude::*;

    fn parse(der: &[u8]) -> Result<X509Certificate<'_>> {
        X509Certificate::from_der(der)
            .map(|(_, c)| c)
            .map_err(|e| JiredError::network(format!("cannot read the server certificate: {e}")))
    }
    let cert = parse(leaf)?;
    // Parsed only to fail early on something unreadable; the DER is what gets pinned.
    let _ = parse(pinned_der)?;

    let names = subject_names(&cert);
    let validity = cert.validity();
    Ok(ServerCert {
        subject: common_name(&cert.subject().to_string()),
        issuer: common_name(&cert.issuer().to_string()),
        fingerprint: fingerprint(pinned_der),
        not_before: date(&validity.not_before),
        not_after: date(&validity.not_after),
        covers_host: names.iter().any(|n| matches_host(n, host)),
        names,
        self_signed: cert.subject() == cert.issuer(),
        expired: !validity.is_valid(),
        chain_len,
        pem: pem(pinned_der),
    })
}

fn subject_names(cert: &x509_parser::certificate::X509Certificate) -> Vec<String> {
    use x509_parser::extensions::GeneralName;
    let mut names = Vec::new();
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for name in &san.value.general_names {
            match name {
                GeneralName::DNSName(n) => names.push((*n).to_string()),
                GeneralName::IPAddress(bytes) => {
                    if let Some(ip) = ip_string(bytes) {
                        names.push(ip);
                    }
                }
                _ => {}
            }
        }
    }
    if names.is_empty() {
        // Certificates without a SAN are ancient, but an internal CA may well
        // still be issuing them, and the CN is all the user has to go on.
        names.push(common_name(&cert.subject().to_string()));
    }
    names
}

fn ip_string(bytes: &[u8]) -> Option<String> {
    match bytes.len() {
        4 => Some(std::net::Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]).to_string()),
        16 => {
            let mut octets = [0u8; 16];
            octets.copy_from_slice(bytes);
            Some(std::net::Ipv6Addr::from(octets).to_string())
        }
        _ => None,
    }
}

/// Pull `CN=` out of an RFC 4514 distinguished name, falling back to the whole
/// thing: what matters is that the user recognizes it.
fn common_name(dn: &str) -> String {
    dn.split(',')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("CN="))
        .unwrap_or(dn)
        .to_string()
}

fn date(t: &x509_parser::prelude::ASN1Time) -> String {
    let d = t.to_datetime();
    format!("{}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

fn fingerprint(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    digest
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn pem(der: &[u8]) -> String {
    let body = base64(der);
    let mut out = String::from("-----BEGIN CERTIFICATE-----\n");
    for chunk in body.as_bytes().chunks(64) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push('\n');
    }
    out.push_str("-----END CERTIFICATE-----\n");
    out
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
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

/// Wildcard matching as the CA/Browser Forum defines it: one leading `*` that
/// stands for exactly one label.
fn matches_host(name: &str, host: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if let Some(suffix) = name.strip_prefix("*.") {
        return match host.split_once('.') {
            Some((_, rest)) => rest == suffix,
            None => false,
        };
    }
    name == host
}

/// Accepts every certificate: this connection exists to look, not to trust.
#[derive(Debug)]
struct AcceptAnything(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAnything {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Does this failure look like the server's certificate was the problem?
///
/// reqwest hides the rustls error behind two layers of `Box<dyn Error>`, so the
/// rendered message is the only thing left to match on.
pub fn looks_like_certificate_problem(err: &JiredError) -> bool {
    let text = format!("{} {}", err.message, err.details).to_lowercase();
    [
        "certificate",
        "unknownissuer",
        "self-signed",
        "self signed",
        "notvalidfor",
        "certexpired",
        "invalid peer",
        "tls handshake",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_cover_one_label_only() {
        assert!(matches_host("jira.corp.example", "jira.corp.example"));
        assert!(matches_host("*.corp.example", "jira.corp.example"));
        assert!(!matches_host("*.corp.example", "a.jira.corp.example"));
        assert!(!matches_host("*.corp.example", "corp.example"));
        assert!(matches_host("JIRA.corp.example.", "jira.corp.example"));
    }

    #[test]
    fn common_name_is_pulled_out_of_a_distinguished_name() {
        assert_eq!(
            common_name("C=DE, O=Acme, CN=jira.acme.test"),
            "jira.acme.test"
        );
        assert_eq!(common_name("O=Acme"), "O=Acme");
    }

    #[test]
    fn pem_wraps_at_64_columns_with_the_expected_envelope() {
        let der = vec![0x30u8; 100];
        let text = pem(&der);
        assert!(text.starts_with("-----BEGIN CERTIFICATE-----\n"));
        assert!(text.ends_with("-----END CERTIFICATE-----\n"));
        let body: Vec<&str> = text.lines().filter(|l| !l.starts_with('-')).collect();
        assert!(body.iter().all(|l| l.len() <= 64), "{body:?}");
    }

    #[test]
    fn fingerprints_match_the_usual_rendering() {
        // echo -n "" | sha256sum
        let fp = fingerprint(b"");
        assert!(fp.starts_with("E3:B0:C4:42:98:FC"), "{fp}");
        assert_eq!(fp.split(':').count(), 32);
    }

    #[test]
    fn certificate_failures_are_recognized_from_the_rendered_error() {
        let err = JiredError::network(
            "GET /rest/api/3/myself: cannot connect: invalid peer certificate: UnknownIssuer",
        );
        assert!(looks_like_certificate_problem(&err));
        let other = JiredError::network("GET /rest/api/3/myself failed with 401");
        assert!(!looks_like_certificate_problem(&other));
    }
}
