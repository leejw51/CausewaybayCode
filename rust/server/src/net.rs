//! The server's network side: one self-signed certificate, HTTP/2 (and HTTP/1.1) over TLS on
//! a TCP port, and HTTP/3 over QUIC on the UDP port of the same number. Both feed the same
//! router; a client that cannot reach the UDP port falls back to HTTP/2.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::body::Body;
use bytes::{Buf, Bytes, BytesMut};
use http_body_util::BodyExt;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tower::ServiceExt;

pub struct Identity {
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
    /// SHA-256 of the certificate, in hex: what a remote client can compare.
    pub fingerprint: String,
}

/// Names and addresses this machine answers to, for the certificate.
fn names() -> Vec<String> {
    let mut names = vec!["localhost".to_string(), "127.0.0.1".to_string(), "::1".to_string()];
    if let Ok(out) = std::process::Command::new("/bin/hostname").output() {
        let host = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
            names.push(host);
        }
    }
    // The address other machines reach this one by (no packet is sent).
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("192.0.2.1:9").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                names.push(addr.ip().to_string());
            }
        }
    }
    names.extend(tailscale());
    names.dedup();
    names
}

/// This machine's Tailscale name and IPv4 address, when Tailscale is installed and running.
pub fn tailscale() -> Vec<String> {
    let found = ["tailscale", "/opt/homebrew/bin/tailscale", "/usr/local/bin/tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"].iter().find_map(|program| {
        let out = std::process::Command::new(program).args(["status", "--json"]).stderr(std::process::Stdio::null()).output().ok()?;
        out.status.success().then_some(out.stdout)
    });
    let Some(json) = found.and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok()) else { return vec![] };
    if json["BackendState"] != "Running" {
        return vec![];
    }
    let mut out = Vec::new();
    if let Some(name) = json["Self"]["DNSName"].as_str().map(|n| n.trim_end_matches('.')).filter(|n| !n.is_empty()) {
        out.push(name.to_string());
    }
    if let Some(ip) = json["Self"]["TailscaleIPs"].as_array().and_then(|ips| ips.iter().filter_map(|ip| ip.as_str()).find(|ip| !ip.contains(':'))) {
        out.push(ip.to_string());
    }
    out
}

fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path).with_context(|| format!("cannot write {}", path.display()))?;
    file.write_all(contents.as_bytes())?;
    Ok(())
}

/// How long a certificate is good for, and how long before its end a new one is made. Apple's
/// browsers refuse to load a page (after the first document) from a server whose certificate
/// runs longer than 398 days, lacks the server-authentication purpose, or does not name the
/// address it was reached at, even once the user has chosen to visit it.
const CERT_DAYS: i64 = 397;
const CERT_RENEW_DAYS: i64 = 30;

/// The server's certificate and key, kept in its folder (`cert.pem`, `key.pem`, and
/// `cert.json`: the names it was made for and when it ends). It is made on first use, and
/// made again when the machine has an address it does not name (a network joined since) or
/// it is about to end; the names it had are kept. Clients on this machine trust exactly this
/// certificate by reading `cert.pem`; others see a new fingerprint when it is remade.
pub fn identity(home: &Path) -> Result<Identity> {
    let (cert_path, key_path, note_path) = (home.join("cert.pem"), home.join("key.pem"), home.join("cert.json"));
    let now = time::OffsetDateTime::now_utc();
    let note: serde_json::Value = std::fs::read_to_string(&note_path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
    let had: Vec<String> = note["names"].as_array().map(|list| list.iter().filter_map(|n| n.as_str().map(String::from)).collect()).unwrap_or_default();
    let ends = note["ends"].as_i64().unwrap_or(0);
    let wanted = names();
    let good = cert_path.exists() && key_path.exists() && wanted.iter().all(|name| had.contains(name)) && ends - now.unix_timestamp() > CERT_RENEW_DAYS * 86_400;
    if !good {
        let mut all = had;
        for name in wanted {
            if !all.contains(&name) {
                all.push(name);
            }
        }
        let mut params = rcgen::CertificateParams::new(all.clone())?;
        params.distinguished_name = rcgen::DistinguishedName::new();
        params.distinguished_name.push(rcgen::DnType::CommonName, "Causewaybay Code");
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(CERT_DAYS);
        params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let ends = params.not_after.unix_timestamp();
        let key = rcgen::KeyPair::generate()?;
        let cert = params.self_signed(&key)?;
        write_private(&key_path, &key.serialize_pem())?;
        std::fs::write(&cert_path, cert.pem())?;
        std::fs::write(&note_path, serde_json::json!({ "names": all, "ends": ends }).to_string())?;
    }
    let cert = rustls_pemfile::certs(&mut std::fs::read(&cert_path)?.as_slice()).next().context("cert.pem holds no certificate")??;
    let key = rustls_pemfile::private_key(&mut std::fs::read(&key_path)?.as_slice())?.context("key.pem holds no key")?;
    let fingerprint = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&cert).iter().map(|b| format!("{b:02x}")).collect()
    };
    Ok(Identity { cert, key, fingerprint })
}

/// The token every request must carry, made on first use and kept in the server's folder.
pub fn token(home: &Path) -> Result<String> {
    let path = home.join("token");
    if let Ok(token) = std::fs::read_to_string(&path) {
        if token.trim().len() >= 32 {
            return Ok(token.trim().to_string());
        }
    }
    let mut bytes = [0u8; 24];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut bytes)?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    write_private(&path, &token)?;
    Ok(token)
}

fn tls(identity: &Identity, alpn: &[&[u8]]) -> Result<rustls::ServerConfig> {
    let mut config = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(vec![identity.cert.clone()], identity.key.clone_key())?;
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    Ok(config)
}

/// Serve the router over TLS on TCP: HTTP/2, and HTTP/1.1 for clients that ask for it.
pub async fn serve_tcp(addr: SocketAddr, identity: &Identity, router: Router, handle: axum_server::Handle) -> Result<()> {
    let config = axum_server::tls_rustls::RustlsConfig::from_config(Arc::new(tls(identity, &[b"h2", b"http/1.1"])?));
    let listener = std::net::TcpListener::bind(addr).with_context(|| format!("cannot listen on tcp {addr}"))?;
    listener.set_nonblocking(true)?;
    // (With the client's address, which sign-in rules look at.)
    // (`handle` is how the caller stops it, to listen somewhere else.)
    axum_server::from_tcp_rustls(listener, config).handle(handle).serve(router.into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

/// The QUIC endpoint for HTTP/3 (bound now, so that a port in use is an error at startup).
pub fn bind_udp(addr: SocketAddr, identity: &Identity) -> Result<quinn::Endpoint> {
    // (On the port after the HTTPS one: see `cwbcode_proto::quic_port`.)
    let addr = SocketAddr::new(addr.ip(), cwbcode_proto::quic_port(addr.port()));
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls(identity, &[b"h3"])?)?;
    let mut config = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    // Keep-alives flow both ways, so a connection is only idle when the client is gone.
    transport.max_idle_timeout(Some(std::time::Duration::from_secs(30).try_into()?));
    transport.keep_alive_interval(Some(std::time::Duration::from_secs(5)));
    // A client needs few requests at once; one without the token gets no room to hoard.
    transport.max_concurrent_bidi_streams(16u32.into());
    transport.max_concurrent_uni_streams(8u32.into());
    config.transport_config(Arc::new(transport));
    quinn::Endpoint::server(config, addr).with_context(|| format!("cannot listen on udp {addr}"))
}

/// Serve the router over HTTP/3.
/// Says whether a request (its headers, address, and where it came from) may use the API.
pub type Allowed = Arc<dyn Fn(&http::HeaderMap, &http::Uri, std::net::IpAddr) -> bool + Send + Sync>;

pub async fn serve_udp(endpoint: quinn::Endpoint, router: Router, allowed: Allowed, unsigned_bytes: usize) {
    while let Some(incoming) = endpoint.accept().await {
        let router = router.clone();
        let allowed = allowed.clone();
        tokio::spawn(async move {
            let from = incoming.remote_address();
            let connection = match incoming.await {
                Ok(connection) => connection,
                Err(e) => {
                    // Worth a line: a browser that will not take the self-signed certificate
                    // over QUIC shows up here, and nowhere else.
                    eprintln!("cwbcode: HTTP/3 from {from}: the connection was not set up ({e})");
                    return;
                }
            };
            let remote = connection.remote_address();
            if !remote.ip().is_loopback() {
                eprintln!("cwbcode: HTTP/3 connection from {remote}");
            }
            let Ok(mut h3) = h3::server::Connection::<_, Bytes>::new(h3_quinn::Connection::new(connection)).await else { return };
            while let Ok(Some(resolver)) = h3.accept().await {
                let router = router.clone();
                let allowed = allowed.clone();
                tokio::spawn(async move {
                    // A client that goes away mid-response is not an error worth a line.
                    let _ = request(resolver, router, allowed, remote, unsigned_bytes).await;
                });
            }
        });
    }
}

/// One HTTP/3 request: read it whole, hand it to the router, stream the response back.
async fn request(resolver: h3::server::RequestResolver<h3_quinn::Connection, Bytes>, router: Router, allowed: Allowed, remote: SocketAddr, unsigned_bytes: usize) -> Result<()> {
    let (head, stream) = resolver.resolve_request().await?;
    let (mut send, mut recv) = stream.split();
    // Without credentials no more of a body is kept than a sign-in needs.
    let limit = if allowed(head.headers(), head.uri(), remote.ip()) { cwbcode_proto::MAX_MESSAGE_BYTES } else { unsigned_bytes };
    let mut body = BytesMut::new();
    while let Some(mut chunk) = recv.recv_data().await? {
        if body.len() + chunk.remaining() > limit {
            let status = if limit == unsigned_bytes { http::StatusCode::UNAUTHORIZED } else { http::StatusCode::PAYLOAD_TOO_LARGE };
            send.send_response(http::Response::builder().status(status).body(())?).await?;
            send.send_data(Bytes::from_static(b"sign in first: the token or session is missing or wrong")).await?;
            send.finish().await?;
            return Ok(());
        }
        body.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
    }
    let (parts, ()) = head.into_parts();
    let mut request = http::Request::from_parts(parts, Body::from(body.freeze()));
    request.extensions_mut().insert(axum::extract::ConnectInfo(remote));
    let response = router.oneshot(request).await?;
    let (parts, mut body) = response.into_parts();
    send.send_response(http::Response::from_parts(parts, ())).await?;
    while let Some(frame) = body.frame().await {
        // Dropping `body` on an error here is what tells the handler the client is gone.
        if let Ok(data) = frame.map_err(|e| anyhow::anyhow!("{e}"))?.into_data() {
            send.send_data(data).await?;
        }
    }
    send.finish().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_and_token_are_made_once_and_kept_private() {
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!("cwbcode-net-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let first = identity(&home).unwrap();
        let again = identity(&home).unwrap();
        assert_eq!(first.cert, again.cert);
        assert_eq!(first.fingerprint, again.fingerprint);
        assert_eq!(first.fingerprint.len(), 64);
        assert_eq!(std::fs::metadata(home.join("key.pem")).unwrap().permissions().mode() & 0o777, 0o600);
        let t = token(&home).unwrap();
        assert_eq!(t.len(), 48);
        assert_eq!(token(&home).unwrap(), t);
        assert_eq!(std::fs::metadata(home.join("token")).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(names().contains(&"localhost".to_string()));

        // A certificate that does not name an address the machine now has, or is near its end,
        // or was made before these were recorded, is made again; the names it had are kept.
        let note = home.join("cert.json");
        let recorded: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&note).unwrap()).unwrap();
        let days = (recorded["ends"].as_i64().unwrap() - time::OffsetDateTime::now_utc().unix_timestamp()) / 86_400;
        assert!((390..=398).contains(&days), "good for {days} days");
        std::fs::write(&note, serde_json::json!({ "names": ["old.example"], "ends": recorded["ends"] }).to_string()).unwrap();
        let widened = identity(&home).unwrap();
        assert_ne!(widened.fingerprint, first.fingerprint);
        let recorded: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&note).unwrap()).unwrap();
        let kept: Vec<&str> = recorded["names"].as_array().unwrap().iter().filter_map(|n| n.as_str()).collect();
        assert!(kept.contains(&"old.example") && kept.contains(&"localhost"), "{kept:?}");
        assert_eq!(identity(&home).unwrap().fingerprint, widened.fingerprint);
        let soon = time::OffsetDateTime::now_utc().unix_timestamp() + 5 * 86_400;
        std::fs::write(&note, serde_json::json!({ "names": kept, "ends": soon }).to_string()).unwrap();
        assert_ne!(identity(&home).unwrap().fingerprint, widened.fingerprint);
        std::fs::remove_file(&note).unwrap();
        let before = identity(&home).unwrap().fingerprint;
        assert_eq!(identity(&home).unwrap().fingerprint, before);
        assert_eq!(std::fs::metadata(home.join("key.pem")).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
