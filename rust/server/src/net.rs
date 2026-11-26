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
    /// The certificate authority that issued it (this machine's own), sent after it.
    pub ca: CertificateDer<'static>,
    /// The authority's certificate as PEM, for a device to install and trust.
    pub ca_pem: String,
    /// SHA-256 of the authority's certificate, in hex.
    pub ca_fingerprint: String,
}

/// How long this machine's certificate authority is good for. A device trusts it once; the
/// server's own certificate, which it signs, is made again far more often.
const CA_DAYS: i64 = 3650;

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// What this machine's certificate authority may sign for, and nothing beyond it: this
/// machine's own names (localhost; names under `.local`, which is how other devices on the
/// network find it; names under `.ts.net`, its Tailscale name; and its host name), the
/// loopback addresses, and the private and Tailscale address ranges. A device that trusts the
/// authority then trusts it for these alone: should its key ever leak, it could not be used to
/// pass for a website on the internet. (A name in a constraint covers itself and every name
/// under it.)
const CA_DNS: [&str; 3] = ["localhost", "local", "ts.net"];
const CA_NETS: [([u8; 4], u8); 5] = [([127, 0, 0, 0], 8), ([10, 0, 0, 0], 8), ([172, 16, 0, 0], 12), ([192, 168, 0, 0], 16), ([100, 64, 0, 0], 10)];
/// The DER of the name-constraints extension's identifier (2.5.29.30): an authority whose
/// certificate lacks it was made before authorities were limited, and is replaced.
const NAME_CONSTRAINTS_OID: [u8; 5] = [0x06, 0x03, 0x55, 0x1d, 0x1e];

/// May the authority sign for this name or address? The same rules as its constraints, so
/// that a certificate never names what the authority may not vouch for: a device checks every
/// name in a certificate, and one name outside the constraints makes the whole of it fail.
fn permitted(name: &str, host: &str) -> bool {
    match name.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => CA_NETS.iter().any(|(net, bits)| u32::from(ip) >> (32 - bits) == u32::from_be_bytes(*net) >> (32 - bits)),
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback(),
        Err(_) => {
            let name = name.to_ascii_lowercase();
            CA_DNS.iter().copied().chain(Some(host).filter(|h| !h.is_empty())).any(|allowed| {
                let allowed = allowed.to_ascii_lowercase();
                name == allowed || name.ends_with(&format!(".{allowed}"))
            })
        }
    }
}

/// This machine's host name, when it is one a certificate can carry.
fn hostname() -> Option<String> {
    let out = std::process::Command::new("/bin/hostname").output().ok()?;
    let host = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')).then_some(host)
}

/// The authority's parameters: rebuilt from its name (and the host name it may sign for)
/// whenever it signs, so that what it issues names it exactly as the certificate devices
/// installed does.
fn ca_params(name: &str, host: &str, now: time::OffsetDateTime) -> Result<rcgen::CertificateParams> {
    let mut params = rcgen::CertificateParams::new(Vec::<String>::new())?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.distinguished_name.push(rcgen::DnType::CommonName, name);
    params.distinguished_name.push(rcgen::DnType::OrganizationName, "Causewaybay Code");
    // It may sign server certificates and nothing below them.
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
    params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign, rcgen::KeyUsagePurpose::CrlSign, rcgen::KeyUsagePurpose::DigitalSignature];
    // Only for this machine's names and addresses (see CA_DNS). rcgen marks the extension
    // critical: a program that does not understand it refuses the authority altogether,
    // rather than trusting it for every name.
    let mut permitted: Vec<rcgen::GeneralSubtree> = CA_DNS.iter().copied().chain(Some(host).filter(|h| !h.is_empty())).map(|dns| rcgen::GeneralSubtree::DnsName(dns.to_string())).collect();
    permitted.extend(CA_NETS.iter().map(|(net, bits)| rcgen::GeneralSubtree::IpAddress(rcgen::CidrSubnet::from_v4_prefix(*net, *bits))));
    permitted.push(rcgen::GeneralSubtree::IpAddress(rcgen::CidrSubnet::from_v6_prefix(std::net::Ipv6Addr::LOCALHOST.octets(), 128)));
    params.name_constraints = Some(rcgen::NameConstraints { permitted_subtrees: permitted, excluded_subtrees: vec![] });
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(CA_DAYS);
    Ok(params)
}

/// This machine's certificate authority, as kept in the server's folder.
struct Authority {
    key: rcgen::KeyPair,
    /// A certificate to sign with: the same name and key as the one devices installed.
    signer: rcgen::Certificate,
    /// The host name it may sign for, besides the names every authority may.
    host: String,
    /// The certificate devices install, as it is in `ca.pem`.
    der: CertificateDer<'static>,
    pem: String,
    /// Whether it is limited to this machine's names (one from before is replaced).
    limited: bool,
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Does a certificate for `name`, signed by `signer` with `key`, chain to the authority `der`?
fn issues(signer: &rcgen::Certificate, key: &rcgen::KeyPair, der: &CertificateDer<'static>, name: &str) -> bool {
    use rustls::client::danger::ServerCertVerifier;
    let leaf = (|| {
        let mut params = rcgen::CertificateParams::new(vec![name.to_string()]).ok()?;
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        params.use_authority_key_identifier_extension = true;
        params.signed_by(&rcgen::KeyPair::generate().ok()?, signer, key).ok()
    })();
    let Some(leaf) = leaf else { return false };
    let mut roots = rustls::RootCertStore::empty();
    if roots.add(der.clone()).is_err() {
        return false;
    }
    let Ok(verifier) = rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), Arc::new(rustls::crypto::ring::default_provider())).build() else { return false };
    let Ok(server) = rustls::pki_types::ServerName::try_from(name.to_string()) else { return false };
    verifier.verify_server_cert(leaf.der(), &[], &server, &[], rustls::pki_types::UnixTime::now()).is_ok()
}

/// This machine's certificate authority (`ca.pem`; `ca-key.pem`, readable by its owner only;
/// `ca.json`, its name and the host name it may sign for), read without changing anything.
/// None when there is none yet. One that is there but cannot be used (its key or its name
/// missing, or not its own) is an error, not a reason to make another: devices trust that
/// one, and a new one would quietly stop them trusting the server.
fn read_authority(home: &Path, now: time::OffsetDateTime) -> Result<Option<Authority>> {
    let (pem_path, key_path, note_path) = (home.join("ca.pem"), home.join("ca-key.pem"), home.join("ca.json"));
    let pem = match std::fs::read_to_string(&pem_path) {
        Ok(pem) => pem,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", pem_path.display())),
    };
    let unusable = |why: &str| {
        anyhow::anyhow!(
            "this machine's certificate authority ({}) cannot be used: {why}. Restore ca-key.pem and ca.json beside it from a backup; or, to make a new authority (every device that trusts the old one must then trust the new one), remove ca.pem",
            pem_path.display()
        )
    };
    let der = rustls_pemfile::certs(&mut pem.as_bytes()).next().ok_or_else(|| unusable("ca.pem holds no certificate"))?.map_err(|e| unusable(&format!("ca.pem cannot be read ({e})")))?;
    let key_pem = std::fs::read_to_string(&key_path).map_err(|e| unusable(&format!("ca-key.pem cannot be read ({e})")))?;
    let key = rcgen::KeyPair::from_pem(&key_pem).map_err(|e| unusable(&format!("ca-key.pem holds no key ({e})")))?;
    // The certificate carries its public key whole: the key pair's must be in it.
    if !contains(&der, &key.public_key_der()) {
        return Err(unusable("ca-key.pem is not the key of the certificate in ca.pem"));
    }
    let note: serde_json::Value = std::fs::read_to_string(&note_path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
    let name = note["name"].as_str().ok_or_else(|| unusable("ca.json, which holds its name, is missing or cannot be read"))?;
    let host = note["host"].as_str().unwrap_or("").to_string();
    let signer = ca_params(name, &host, now)?.self_signed(&key)?;
    // What it signs must chain to the certificate devices installed: the name in ca.json must
    // be that certificate's.
    if !issues(&signer, &key, &der, "localhost") {
        return Err(unusable("what it signs does not chain to ca.pem (ca.json names another authority)"));
    }
    let limited = contains(&der, &NAME_CONSTRAINTS_OID);
    Ok(Some(Authority { key, signer, host, der, pem, limited }))
}

/// Make this machine's certificate authority and keep it: devices that trust it keep trusting
/// the server through every new certificate. Whatever authority was there is replaced.
fn make_authority(home: &Path, now: time::OffsetDateTime) -> Result<Authority> {
    let (pem_path, key_path, note_path) = (home.join("ca.pem"), home.join("ca-key.pem"), home.join("ca.json"));
    // A name of its own, so that two machines' authorities are told apart on a device.
    let host = hostname().unwrap_or_default();
    let mut id = [0u8; 4];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut id)?;
    let name = format!("Causewaybay Code CA {}{}", if host.is_empty() { String::new() } else { format!("{host} ") }, id.iter().map(|b| format!("{b:02x}")).collect::<String>());
    let key = rcgen::KeyPair::generate()?;
    let cert = ca_params(&name, &host, now)?.self_signed(&key)?;
    // ca.pem is what says there is an authority, so it goes first and comes back last: a stop
    // halfway leaves no ca.pem beside a key that is not its own, and the next start makes one.
    if let Err(e) = std::fs::remove_file(&pem_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(e).with_context(|| format!("cannot replace {}", pem_path.display()));
        }
    }
    write_private(&key_path, &key.serialize_pem())?;
    write_file(&note_path, &serde_json::json!({ "name": name, "host": host }).to_string(), 0o644)?;
    write_file(&pem_path, &cert.pem(), 0o644)?;
    let (der, pem) = (cert.der().clone(), cert.pem());
    Ok(Authority { key, signer: cert, host, der, pem, limited: true })
}

/// Names and addresses this machine answers to, for the certificate.
fn names() -> Vec<String> {
    let mut names = vec!["localhost".to_string(), "127.0.0.1".to_string(), "::1".to_string()];
    names.extend(hostname());
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

/// Write a file whole: to a temporary file beside it, then moved over it, so that a reader
/// (or a stop halfway) never sees half of it. The mode is set explicitly, whatever a file
/// left there before allowed.
fn write_file(path: &Path, contents: &str, mode: u32) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = path.with_file_name(format!(".{name}.tmp"));
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(mode).open(&temporary).with_context(|| format!("cannot write {}", temporary.display()))?;
    // (`mode` above applies only to a file made now; one left over keeps its own until this.)
    file.set_permissions(std::fs::Permissions::from_mode(mode))?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temporary, path).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

/// Write a file only its owner may read.
pub fn write_private(path: &Path, contents: &str) -> Result<()> {
    write_file(path, contents, 0o600)
}

/// How long a certificate is good for, and how long before its end a new one is made. Apple's
/// browsers refuse to load a page (after the first document) from a server whose certificate
/// runs longer than 398 days, lacks the server-authentication purpose, or does not name the
/// address it was reached at, even once the user has chosen to visit it.
const CERT_DAYS: i64 = 397;
const CERT_RENEW_DAYS: i64 = 30;

/// The server's certificate and key, kept in its folder (`cert.pem`, `key.pem`, and
/// `cert.json`: the names it was made for, when it ends, and which authority signed it). It
/// is signed by this machine's own certificate authority (see `read_authority`), so a device that
/// trusts that once trusts every certificate after. It is made on first use, and made again
/// when the machine has an address it does not name (a network joined since), it is about to
/// end, or another authority signed it; the names it had are kept. It names only what the
/// authority may sign for (see `permitted`). Clients on this machine trust exactly this
/// certificate by reading `cert.pem`; others see a new fingerprint when it is remade.
///
/// An authority made before authorities were limited to this machine's names is replaced,
/// once, and the certificate with it. Only `serve` calls this: `current` says what is there
/// without writing anything.
pub fn identity(home: &Path) -> Result<Identity> {
    let (cert_path, key_path, note_path) = (home.join("cert.pem"), home.join("key.pem"), home.join("cert.json"));
    let now = time::OffsetDateTime::now_utc();
    let ca = match read_authority(home, now)? {
        Some(ca) if ca.limited => ca,
        found => {
            if found.is_some() {
                eprintln!(
                    "cwbcode: this machine's certificate authority ({}) may sign for any name: it was made before authorities were limited to this machine's own. It is replaced by one that is; a device that trusted the old one must trust the new one (`cwbcode url` says where to get it), and should remove the old one.",
                    home.join("ca.pem").display()
                );
            }
            make_authority(home, now)?
        }
    };
    let ca_fingerprint = sha256_hex(&ca.der);
    if let Some(all) = renewal(home, &ca_fingerprint, &ca.host, now) {
        let mut params = rcgen::CertificateParams::new(all.clone())?;
        params.distinguished_name = rcgen::DistinguishedName::new();
        params.distinguished_name.push(rcgen::DnType::CommonName, "Causewaybay Code");
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(CERT_DAYS);
        params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let ends = params.not_after.unix_timestamp();
        params.use_authority_key_identifier_extension = true;
        let key = rcgen::KeyPair::generate()?;
        let cert = params.signed_by(&key, &ca.signer, &ca.key)?;
        // The record goes first and comes back last: a stop between the key and the
        // certificate then leaves no record, and the next start makes both again.
        let _ = std::fs::remove_file(&note_path);
        write_private(&key_path, &key.serialize_pem())?;
        write_file(&cert_path, &cert.pem(), 0o644)?;
        write_file(&note_path, &serde_json::json!({ "names": all, "ends": ends, "ca": ca_fingerprint }).to_string(), 0o644)?;
    }
    let cert = rustls_pemfile::certs(&mut std::fs::read(&cert_path)?.as_slice()).next().context("cert.pem holds no certificate")??;
    let key = rustls_pemfile::private_key(&mut std::fs::read(&key_path)?.as_slice())?.context("key.pem holds no key")?;
    let fingerprint = sha256_hex(&cert);
    Ok(Identity { cert, key, fingerprint, ca: ca.der, ca_pem: ca.pem, ca_fingerprint })
}

/// Whether the certificate on disk must be made again, and if so the names the new one
/// carries: those it had and those the machine has now, as far as the authority may sign for
/// them. (A certificate from before there was an authority, self-signed, is made again once.)
fn renewal(home: &Path, ca_fingerprint: &str, host: &str, now: time::OffsetDateTime) -> Option<Vec<String>> {
    let note: serde_json::Value = std::fs::read_to_string(home.join("cert.json")).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
    let had: Vec<String> = note["names"].as_array().map(|list| list.iter().filter_map(|n| n.as_str().map(String::from)).collect()).unwrap_or_default();
    let ends = note["ends"].as_i64().unwrap_or(0);
    let wanted: Vec<String> = names().into_iter().filter(|name| permitted(name, host)).collect();
    let good = home.join("cert.pem").exists()
        && home.join("key.pem").exists()
        && note["ca"].as_str() == Some(ca_fingerprint)
        && had.iter().all(|name| permitted(name, host))
        && wanted.iter().all(|name| had.contains(name))
        && ends - now.unix_timestamp() > CERT_RENEW_DAYS * 86_400;
    if good {
        return None;
    }
    let mut all: Vec<String> = had.into_iter().filter(|name| permitted(name, host)).collect();
    for name in wanted {
        if !all.contains(&name) {
            all.push(name);
        }
    }
    Some(all)
}

/// The certificates as they are on disk, for `cwbcode url` to show, and whether the next
/// start of the backend changes them. Nothing is written: a backend may be running on these
/// files, and its certificate is changed only by itself, when it starts.
pub struct Current {
    /// SHA-256 of `cert.pem` (None: there is none yet).
    pub fingerprint: Option<String>,
    /// The next start makes a new certificate (it is near its end, lacks a name the machine
    /// now has, or its authority is replaced).
    pub reissue: bool,
    /// SHA-256 of `ca.pem` (None: there is none yet).
    pub ca_fingerprint: Option<String>,
    /// The next start replaces the authority: it may sign for any name.
    pub replace_ca: bool,
}

pub fn current(home: &Path) -> Result<Current> {
    let now = time::OffsetDateTime::now_utc();
    let ca = read_authority(home, now)?;
    let cert = std::fs::read(home.join("cert.pem")).ok().and_then(|bytes| rustls_pemfile::certs(&mut bytes.as_slice()).next()?.ok());
    let ca_fingerprint = ca.as_ref().map(|ca| sha256_hex(&ca.der));
    let replace_ca = ca.as_ref().is_some_and(|ca| !ca.limited);
    let reissue = match (&ca, &ca_fingerprint) {
        (Some(ca), Some(fingerprint)) => replace_ca || renewal(home, fingerprint, &ca.host, now).is_some(),
        _ => true,
    };
    Ok(Current { fingerprint: cert.map(|c| sha256_hex(&c)), reissue, ca_fingerprint, replace_ca })
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
    // The certificate, then the authority that signed it.
    let mut config = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(vec![identity.cert.clone(), identity.ca.clone()], identity.key.clone_key())?;
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

        // The certificate is signed by this machine's authority, whose key is private too.
        assert_eq!(std::fs::metadata(home.join("ca-key.pem")).unwrap().permissions().mode() & 0o777, 0o600);
        let issued_by = |id: &Identity| {
            let mut roots = rustls::RootCertStore::empty();
            roots.add(id.ca.clone()).unwrap();
            let verifier = rustls::client::WebPkiServerVerifier::builder(Arc::new(roots)).build().unwrap();
            let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
            verifier.verify_server_cert(&id.cert, &[], &name, &[], rustls::pki_types::UnixTime::now()).is_ok()
        };
        use rustls::client::danger::ServerCertVerifier;
        assert!(issued_by(&first), "the certificate does not chain to the authority");
        assert_ne!(first.ca_fingerprint, first.fingerprint);
        assert!(first.ca_pem.starts_with("-----BEGIN CERTIFICATE-----"));

        // A certificate that does not name an address the machine now has, or is near its end,
        // or was made before these were recorded, is made again; the names it had are kept.
        let note = home.join("cert.json");
        let recorded: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&note).unwrap()).unwrap();
        let days = (recorded["ends"].as_i64().unwrap() - time::OffsetDateTime::now_utc().unix_timestamp()) / 86_400;
        assert!((390..=398).contains(&days), "good for {days} days");
        // (A name the authority may not sign for is dropped: it would make the whole
        // certificate fail on a device.)
        std::fs::write(&note, serde_json::json!({ "names": ["old-name.local", "old.example"], "ends": recorded["ends"] }).to_string()).unwrap();
        let widened = identity(&home).unwrap();
        assert_ne!(widened.fingerprint, first.fingerprint);
        // A new certificate, the same authority: a device that trusts it still trusts the server.
        assert_eq!(widened.ca_fingerprint, first.ca_fingerprint);
        assert!(issued_by(&widened), "the new certificate does not chain to the same authority");
        let recorded: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&note).unwrap()).unwrap();
        let kept: Vec<&str> = recorded["names"].as_array().unwrap().iter().filter_map(|n| n.as_str()).collect();
        assert!(kept.contains(&"old-name.local") && kept.contains(&"localhost") && !kept.contains(&"old.example"), "{kept:?}");
        assert_eq!(identity(&home).unwrap().fingerprint, widened.fingerprint);
        let soon = time::OffsetDateTime::now_utc().unix_timestamp() + 5 * 86_400;
        std::fs::write(&note, serde_json::json!({ "names": kept, "ends": soon }).to_string()).unwrap();
        assert_ne!(identity(&home).unwrap().fingerprint, widened.fingerprint);
        std::fs::remove_file(&note).unwrap();
        let before = identity(&home).unwrap();
        assert_eq!(identity(&home).unwrap().fingerprint, before.fingerprint);
        assert_eq!(before.ca_fingerprint, first.ca_fingerprint);
        assert!(issued_by(&before));
        assert_eq!(std::fs::metadata(home.join("key.pem")).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&home);
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let home = std::env::temp_dir().join(format!("cwbcode-net-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    #[test]
    fn the_authority_signs_for_this_machines_names_only() {
        let home = scratch("limited");
        identity(&home).unwrap();
        let now = time::OffsetDateTime::now_utc();
        let ca = read_authority(&home, now).unwrap().unwrap();
        assert!(ca.limited);
        // What a device sees: a certificate the authority signed for a name, checked against
        // the authority alone.
        for name in ["localhost", "mac.local", "my-mac.tail1234.ts.net", "127.0.0.1", "192.168.1.20", "10.1.2.3", "172.20.0.5", "100.101.102.103", "::1"] {
            assert!(issues(&ca.signer, &ca.key, &ca.der, name), "{name} was refused");
            assert!(permitted(name, &ca.host), "{name}");
        }
        for name in ["example.com", "www.google.com", "local.example.com", "ts.net.example.com", "8.8.8.8", "172.32.0.1", "100.128.0.1", "2001:db8::1"] {
            assert!(!issues(&ca.signer, &ca.key, &ca.der, name), "the authority signed for {name}");
            assert!(!permitted(name, &ca.host), "{name}");
        }
        if let Some(host) = hostname() {
            assert!(issues(&ca.signer, &ca.key, &ca.der, &host), "the host name {host} was refused");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn an_authority_from_before_the_limits_is_replaced_once_and_a_broken_one_is_an_error() {
        let home = scratch("legacy");
        // An authority as earlier versions made it: no name constraints.
        let now = time::OffsetDateTime::now_utc();
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = ca_params("Causewaybay Code CA old", "", now).unwrap();
        params.name_constraints = None;
        let old = params.self_signed(&key).unwrap();
        std::fs::write(home.join("ca.pem"), old.pem()).unwrap();
        write_private(&home.join("ca-key.pem"), &key.serialize_pem()).unwrap();
        std::fs::write(home.join("ca.json"), serde_json::json!({ "name": "Causewaybay Code CA old" }).to_string()).unwrap();
        assert!(!read_authority(&home, now).unwrap().unwrap().limited);
        let old_fingerprint = sha256_hex(old.der());
        // `cwbcode url` only says what the next start will do.
        let shown = current(&home).unwrap();
        assert_eq!((shown.ca_fingerprint.as_deref(), shown.replace_ca, shown.fingerprint.is_none()), (Some(old_fingerprint.as_str()), true, true));
        assert_eq!(std::fs::read_to_string(home.join("ca.pem")).unwrap(), old.pem());
        // The backend replaces it, with its certificate; after that it is kept.
        let replaced = identity(&home).unwrap();
        assert_ne!(replaced.ca_fingerprint, old_fingerprint);
        assert!(read_authority(&home, now).unwrap().unwrap().limited);
        assert_eq!(identity(&home).unwrap().ca_fingerprint, replaced.ca_fingerprint);
        let shown = current(&home).unwrap();
        assert_eq!((shown.replace_ca, shown.reissue, shown.fingerprint.as_deref()), (false, false, Some(replaced.fingerprint.as_str())));

        // A certificate due to be made again is only reported by `current`, not made.
        let note = home.join("cert.json");
        let mut recorded: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&note).unwrap()).unwrap();
        recorded["ends"] = (now.unix_timestamp() + 86_400).into();
        std::fs::write(&note, recorded.to_string()).unwrap();
        let before = std::fs::read(home.join("cert.pem")).unwrap();
        assert!(current(&home).unwrap().reissue);
        assert_eq!(std::fs::read(home.join("cert.pem")).unwrap(), before, "cwbcode url wrote the certificate");

        // An authority whose key or name is missing, or not its own, stops the backend rather
        // than being quietly replaced (devices trust it).
        let pem = std::fs::read_to_string(home.join("ca.pem")).unwrap();
        let json = std::fs::read_to_string(home.join("ca.json")).unwrap();
        let refused = |home: &Path| match identity(home) {
            Ok(_) => panic!("a broken authority was used or replaced"),
            Err(e) => e.to_string(),
        };
        std::fs::remove_file(home.join("ca.json")).unwrap();
        assert!(refused(&home).contains("ca.json"));
        assert!(current(&home).is_err());
        std::fs::write(home.join("ca.json"), serde_json::json!({ "name": "another name", "host": "" }).to_string()).unwrap();
        assert!(refused(&home).contains("does not chain"));
        std::fs::write(home.join("ca.json"), &json).unwrap();
        write_private(&home.join("ca-key.pem"), &rcgen::KeyPair::generate().unwrap().serialize_pem()).unwrap();
        assert!(refused(&home).contains("not the key"));
        assert_eq!(std::fs::read_to_string(home.join("ca.pem")).unwrap(), pem, "a broken authority was replaced");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_private_file_is_made_private_even_over_a_looser_one() {
        use std::os::unix::fs::PermissionsExt;
        let home = scratch("private");
        let path = home.join("token");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        // (A temporary file left over from a stop halfway, readable by others, too.)
        let left = home.join(".token.tmp");
        std::fs::write(&left, "").unwrap();
        std::fs::set_permissions(&left, std::fs::Permissions::from_mode(0o666)).unwrap();
        write_private(&path, "newer").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&home);
    }
}
