use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use bytes::{Buf, Bytes};
use cwbcode_proto::{CALL_PATH, CHAT_PATH, EVENTS_PATH, Event, Frames, Progress, Request, Response};
use futures::StreamExt;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

/// How long to try HTTP/3 before settling for HTTP/2.
const H3_TIMEOUT: Duration = Duration::from_millis(1500);

/// Which server certificate is accepted.
#[derive(Clone, Debug)]
pub enum Trust {
    /// Exactly this certificate (the backend's `cert.pem`), whatever name it was reached by.
    Pinned(CertificateDer<'static>),
    /// Any certificate: the connection is encrypted but the server is not verified.
    Insecure,
}

impl Trust {
    pub fn from_pem(pem: &[u8]) -> Result<Self> {
        let cert = rustls_pemfile::certs(&mut &pem[..]).next().context("the file holds no certificate")??;
        Ok(Self::Pinned(cert))
    }
}

/// Accepts the pinned certificate (or any, when told to); signatures are checked as usual.
#[derive(Debug)]
struct Verifier {
    trust: Trust,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        match &self.trust {
            Trust::Insecure => Ok(ServerCertVerified::assertion()),
            Trust::Pinned(expected) if expected.as_ref() == end_entity.as_ref() => Ok(ServerCertVerified::assertion()),
            Trust::Pinned(_) => Err(rustls::Error::General("the server's certificate is not the one this client trusts".into())),
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &rustls::DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &rustls::DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

fn tls(trust: &Trust, alpn: &[u8]) -> rustls::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(Verifier { trust: trust.clone(), provider: provider.clone() });
    let mut config = rustls::ClientConfig::builder_with_provider(provider).with_safe_default_protocol_versions().expect("default TLS versions").dangerous().with_custom_certificate_verifier(verifier).with_no_client_auth();
    config.alpn_protocols = vec![alpn.to_vec()];
    config
}

type Sender = h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;

#[derive(Clone)]
enum Transport {
    H3 { send: Sender, _endpoint: quinn::Endpoint },
    H2(reqwest::Client),
}

/// The connection was lost (as opposed to the server answering no): worth one new connection.
#[derive(Debug)]
struct Broken(String);

impl std::fmt::Display for Broken {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "the connection broke: {}", self.0)
    }
}

impl std::error::Error for Broken {}

fn broken(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow::Error::new(Broken(e.to_string()))
}

pub struct Client {
    transport: std::sync::Mutex<Transport>,
    address: SocketAddr,
    host: String,
    trust: Trust,
    /// https://host:port
    base: String,
    /// What the Authorization header carries ("" in local mode: nothing).
    token: std::sync::Mutex<String>,
    /// Why HTTP/3 was not used, when it was tried and HTTP/2 was used instead.
    pub fallback: Option<String>,
}

/// The events of a chat (or the messages of the events stream), as they arrive.
pub enum Events {
    H3 { stream: h3::client::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>, frames: Frames },
    H2 { stream: futures::stream::BoxStream<'static, reqwest::Result<Bytes>>, frames: Frames },
}

impl Events {
    /// The next event; None when the stream has ended.
    pub async fn next(&mut self) -> Result<Option<Event>> {
        match self.next_message().await? {
            Some(message) => Ok(Some(Event::decode(&message)?)),
            None => Ok(None),
        }
    }

    /// The next message of the events stream (`Client::progress`); None when it has ended.
    pub async fn next_progress(&mut self) -> Result<Option<Progress>> {
        match self.next_message().await? {
            Some(message) => Ok(Some(Progress::decode(&message)?)),
            None => Ok(None),
        }
    }

    /// The next whole message, however it was cut; None when the stream has ended.
    pub async fn next_message(&mut self) -> Result<Option<Vec<u8>>> {
        loop {
            let frames = match self {
                Self::H3 { frames, .. } | Self::H2 { frames, .. } => frames,
            };
            if let Some(message) = frames.next()? {
                return Ok(Some(message));
            }
            let more: Option<Bytes> = match self {
                Self::H3 { stream, .. } => stream.recv_data().await.map_err(|e| anyhow!("the connection broke: {e}"))?.map(|mut chunk| chunk.copy_to_bytes(chunk.remaining())),
                Self::H2 { stream, .. } => stream.next().await.transpose().map_err(|e| anyhow!("the connection broke: {e}"))?,
            };
            let frames = match self {
                Self::H3 { frames, .. } | Self::H2 { frames, .. } => frames,
            };
            match more {
                Some(bytes) => frames.push(&bytes),
                None if frames.pending() > 0 => bail!("the stream ended in the middle of a message"),
                None => return Ok(None),
            }
        }
    }
}

fn host_port(url: &str) -> Result<(String, u16)> {
    let rest = url.strip_prefix("https://").with_context(|| format!("{url}: the server's address starts with https://"))?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => (host, port.parse().with_context(|| format!("{url}: bad port"))?),
        _ => (authority, 7443),
    };
    Ok((host.trim_start_matches('[').trim_end_matches(']').to_string(), port))
}

impl Client {
    /// Connect to the backend at `url` (https://host:port). HTTP/3 is tried first unless
    /// `h3` is false; if the server cannot be reached over QUIC, HTTP/2 is used.
    pub async fn connect(url: &str, token: &str, trust: Trust, h3: bool) -> Result<Self> {
        let (host, port) = host_port(url)?;
        let base = format!("https://{}:{port}", if host.contains(':') { format!("[{host}]") } else { host.clone() });
        let address: SocketAddr = tokio::net::lookup_host((host.as_str(), port)).await.with_context(|| format!("cannot find {host}"))?.next().with_context(|| format!("{host} has no address"))?;
        let mut fallback = None;
        if h3 {
            match tokio::time::timeout(H3_TIMEOUT, Self::quic(address, &host, &trust)).await {
                Ok(Ok((send, endpoint))) => return Ok(Self { transport: std::sync::Mutex::new(Transport::H3 { send, _endpoint: endpoint }), address, host, trust, base, token: std::sync::Mutex::new(token.to_string()), fallback: None }),
                Ok(Err(e)) => fallback = Some(format!("{e:#}")),
                Err(_) => fallback = Some("no answer over QUIC".to_string()),
            }
        }
        let client = Self::tcp(&trust)?;
        Ok(Self { transport: std::sync::Mutex::new(Transport::H2(client)), address, host, trust, base, token: std::sync::Mutex::new(token.to_string()), fallback })
    }

    fn tcp(trust: &Trust) -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder().use_preconfigured_tls(tls(trust, b"h2")).http2_keep_alive_interval(Duration::from_secs(10)).connect_timeout(Duration::from_secs(5)).build()?)
    }

    /// Use this token (a session from signing in, or the backend's own) from now on.
    pub fn set_token(&self, token: &str) {
        *self.token.lock().unwrap() = token.to_string();
    }

    fn token(&self) -> String {
        self.token.lock().unwrap().clone()
    }

    fn transport(&self) -> Transport {
        self.transport.lock().unwrap().clone()
    }

    /// After a lost HTTP/3 connection (the backend was restarted, say): connect again, or
    /// go on over HTTP/2 if QUIC no longer answers.
    async fn reconnect(&self) -> Result<Transport> {
        let fresh = match tokio::time::timeout(H3_TIMEOUT, Self::quic(self.address, &self.host, &self.trust)).await {
            Ok(Ok((send, endpoint))) => Transport::H3 { send, _endpoint: endpoint },
            _ => Transport::H2(Self::tcp(&self.trust)?),
        };
        *self.transport.lock().unwrap() = fresh.clone();
        Ok(fresh)
    }

    async fn quic(address: SocketAddr, host: &str, trust: &Trust) -> Result<(Sender, quinn::Endpoint)> {
        // HTTP/3 is on the port after the HTTPS one.
        let address = SocketAddr::new(address.ip(), cwbcode_proto::quic_port(address.port()));
        let bind: SocketAddr = if address.is_ipv6() { "[::]:0".parse()? } else { "0.0.0.0:0".parse()? };
        let mut endpoint = quinn::Endpoint::client(bind)?;
        let mut config = quinn::ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(tls(trust, b"h3"))?));
        let mut transport = quinn::TransportConfig::default();
        // Keep-alives flow all the time, so silence this long means the backend is gone
        // (restarted, say), however long the model itself stays quiet.
        transport.max_idle_timeout(Some(Duration::from_secs(8).try_into()?));
        transport.keep_alive_interval(Some(Duration::from_secs(2)));
        config.transport_config(Arc::new(transport));
        endpoint.set_default_client_config(config);
        let connection = endpoint.connect(address, host)?.await?;
        let (mut driver, send) = h3::client::new(h3_quinn::Connection::new(connection)).await?;
        tokio::spawn(async move {
            let _ = std::future::poll_fn(|cx| driver.poll_close(cx)).await;
        });
        Ok((send, endpoint))
    }

    /// The server's address: https://host:port
    pub fn base(&self) -> &str {
        &self.base
    }

    /// "h3" or "h2".
    pub fn protocol(&self) -> &'static str {
        match *self.transport.lock().unwrap() {
            Transport::H3 { .. } => "h3",
            Transport::H2(_) => "h2",
        }
    }

    fn refused(status: http::StatusCode, body: &[u8]) -> anyhow::Error {
        let said = String::from_utf8_lossy(body);
        match status {
            http::StatusCode::UNAUTHORIZED => anyhow!("the server refused the token: not signed in ({})", said.trim()),
            _ => anyhow!("the server answered {status}: {}", said.trim()),
        }
    }

    async fn h3_post(&self, send: &Sender, path: &str, body: Vec<u8>) -> Result<h3::client::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>> {
        let mut request = http::Request::post(format!("{}{path}", self.base));
        let token = self.token();
        if !token.is_empty() {
            request = request.header(http::header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let request = request.body(())?;
        let mut stream = send.clone().send_request(request).await.map_err(broken)?;
        stream.send_data(Bytes::from(body)).await.map_err(broken)?;
        stream.finish().await.map_err(broken)?;
        let response = stream.recv_response().await.map_err(broken)?;
        if !response.status().is_success() {
            let mut said = Vec::new();
            while let Ok(Some(mut chunk)) = stream.recv_data().await {
                said.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
            }
            return Err(Self::refused(response.status(), &said));
        }
        Ok(stream)
    }

    /// POST over the current transport; a lost HTTP/3 connection is replaced once.
    async fn post(&self, path: &str, body: Vec<u8>) -> Result<Posted> {
        let mut transport = self.transport();
        for attempt in 0..2 {
            match &transport {
                Transport::H3 { send, .. } => match self.h3_post(send, path, body.clone()).await {
                    Ok(stream) => return Ok(Posted::H3(stream)),
                    Err(e) if attempt == 0 && e.downcast_ref::<Broken>().is_some() => transport = self.reconnect().await?,
                    Err(e) => return Err(e),
                },
                Transport::H2(client) => return Ok(Posted::H2(self.h2_post(client, path, body).await?)),
            }
        }
        unreachable!("the second attempt returns")
    }

    async fn h2_post(&self, client: &reqwest::Client, path: &str, body: Vec<u8>) -> Result<reqwest::Response> {
        let mut request = client.post(format!("{}{path}", self.base)).body(body);
        let token = self.token();
        if !token.is_empty() {
            request = request.bearer_auth(token);
        }
        let response = request.send().await.map_err(|e| anyhow!("cannot reach the server at {}: {}", self.base, root_cause(&e)))?;
        if !response.status().is_success() {
            let status = response.status();
            return Err(Self::refused(status, &response.bytes().await.unwrap_or_default()));
        }
        Ok(response)
    }

    /// Fetch a page or file of the web client: its status and bytes. (How a browser asks,
    /// for checking that both transports serve the same thing.)
    pub async fn get(&self, path: &str) -> Result<(u16, Vec<u8>)> {
        match self.transport() {
            Transport::H3 { send, .. } => {
                let request = http::Request::get(format!("{}{path}", self.base)).body(())?;
                let mut stream = send.clone().send_request(request).await.map_err(broken)?;
                stream.finish().await.map_err(broken)?;
                let response = stream.recv_response().await.map_err(broken)?;
                let mut body = Vec::new();
                while let Some(mut chunk) = stream.recv_data().await.map_err(broken)? {
                    body.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
                }
                Ok((response.status().as_u16(), body))
            }
            Transport::H2(client) => {
                let response = client.get(format!("{}{path}", self.base)).send().await?;
                Ok((response.status().as_u16(), response.bytes().await?.to_vec()))
            }
        }
    }

    /// A file of the web client over HTTP/2, with its type and how long it may be kept.
    pub async fn raw_get(&self, path: &str) -> Result<(u16, String, String, Vec<u8>)> {
        let Transport::H2(client) = self.transport() else { bail!("raw_get is for the HTTP/2 transport") };
        let response = client.get(format!("{}{path}", self.base)).send().await?;
        let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let (status, kind, cache) = (response.status().as_u16(), header("content-type"), header("cache-control"));
        Ok((status, kind, cache, response.bytes().await?.to_vec()))
    }

    /// One call and its answer. An error the server reports is an error here.
    pub async fn call(&self, request: &Request) -> Result<Response> {
        let (response, _) = self.call_raw(request).await?;
        response.ok().map_err(|e| anyhow!("{e}"))
    }

    /// One call, its answer as sent, and the protocol the server says the request arrived by.
    pub async fn call_raw(&self, request: &Request) -> Result<(Response, String)> {
        let (bytes, protocol) = match self.post(CALL_PATH, request.encode()).await? {
            Posted::H3(mut stream) => {
                let mut body = Vec::new();
                while let Some(mut chunk) = stream.recv_data().await.map_err(broken)? {
                    body.extend_from_slice(&chunk.copy_to_bytes(chunk.remaining()));
                }
                (body, "h3".to_string())
            }
            Posted::H2(response) => {
                let protocol = response.headers().get(cwbcode_proto::PROTOCOL_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                (response.bytes().await?.to_vec(), protocol)
            }
        };
        Ok((Response::decode(&bytes).map_err(|e| anyhow!("the server's answer could not be read: {e}"))?, protocol))
    }

    /// Send a chat message; its events arrive as the model works.
    pub async fn chat(&self, request: &Request) -> Result<Events> {
        self.stream(CHAT_PATH, request.encode()).await
    }

    /// Follow the model's state and its download: a Progress message on every change (read
    /// them with `Events::next_progress`), until the model is ready.
    pub async fn progress(&self) -> Result<Events> {
        self.stream(EVENTS_PATH, Vec::new()).await
    }

    async fn stream(&self, path: &str, body: Vec<u8>) -> Result<Events> {
        Ok(match self.post(path, body).await? {
            Posted::H3(stream) => Events::H3 { stream, frames: Frames::new() },
            Posted::H2(response) => Events::H2 { stream: response.bytes_stream().boxed(), frames: Frames::new() },
        })
    }
}

enum Posted {
    H3(h3::client::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>),
    H2(reqwest::Response),
}

fn root_cause(error: &dyn std::error::Error) -> String {
    let mut cause = error;
    while let Some(next) = cause.source() {
        cause = next;
    }
    cause.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_read() {
        assert_eq!(host_port("https://localhost:7443").unwrap(), ("localhost".to_string(), 7443));
        assert_eq!(host_port("https://192.168.0.7:9000/").unwrap(), ("192.168.0.7".to_string(), 9000));
        assert_eq!(host_port("https://mac.local").unwrap(), ("mac.local".to_string(), 7443));
        assert_eq!(host_port("https://[::1]:7443").unwrap(), ("::1".to_string(), 7443));
        assert!(host_port("http://localhost:7443").is_err());
        assert!(host_port("localhost").is_err());
    }
}
