//! Who may use the backend.
//!
//!   open     local mode (no wallet, listening on this machine only): no wallet sign-in, but
//!            the backend's token is still needed, and only from this machine. It is in
//!            ~/.cwbcode/token, readable by this user alone; the web client gets it with the
//!            one-time code in the link `cwbcode url` prints (or it is typed in). Without
//!            it any program on the machine, another
//!            user's or one behind a port forward (ssh -L, tailscale serve), could run
//!            commands. Requests from another website's page are refused as well.
//!   wallet   CWBCODE_WALLET is set: sign in by signing a one-time challenge with that
//!            wallet's key (the web client derives it from the recovery phrase, in the
//!            browser). A session token is given in return. The backend's own token file
//!            still works, but only from this machine: it is how the terminal client here
//!            gets in without the phrase.
//!   token    no wallet, but listening beyond this machine (an explicit --bind): the
//!            backend's token, as before.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Result, bail};
use axum::http::{HeaderMap, header};
use cwbcode_proto::wallet;
use sha2::{Digest, Sha256};

/// How long a session lasts without signing in again.
const SESSION_SECONDS: u64 = 30 * 24 * 3600;
/// Failed sign-ins allowed per minute, over all clients.
const FAILURES_PER_MINUTE: u32 = 10;
const MAX_CHALLENGES: usize = 1000;

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Open,
    Wallet(String),
    Token,
}

pub struct Auth {
    /// It changes when remote mode is turned on or off while the backend runs.
    mode: std::sync::RwLock<Mode>,
    /// The backend's own token (~/.cwbcode/token).
    token: String,
    /// SHA-256 of the certificate: named in every challenge.
    pub fingerprint: String,
    /// Session tokens (their SHA-256) and when they end.
    sessions: Mutex<HashMap<String, u64>>,
    /// Challenges handed out and not yet answered, and when they end.
    challenges: Mutex<HashMap<String, u64>>,
    /// (the minute, failed sign-ins in it)
    failures: Mutex<(u64, u32)>,
    /// Where sessions are kept across restarts.
    file: PathBuf,
    /// Where the one-time codes of the links `cwbcode url` prints are kept (see `issue_code`).
    codes: PathBuf,
}

/// How long the one-time code in a link from `cwbcode url` is good for.
pub const CODE_SECONDS: u64 = 300;

/// A one-time code for the link `cwbcode url` prints: the page exchanges it for the backend's
/// token (`Auth::redeem`), so the long-lived token itself never stands in an address, where
/// the browser's history would keep it. `cwbcode url` runs as a process of its own, beside the
/// running backend, so the codes are files in `<home>/codes` (readable by this user only),
/// each named by its code's SHA-256 and holding when it ends. Using one up is removing its
/// file, which only one request can do.
pub fn issue_code(home: &Path) -> Result<String> {
    use std::os::unix::fs::PermissionsExt;
    let dir = home.join("codes");
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    // Codes that have ended are cleared away. (A name starting with a dot is one being written.)
    for entry in std::fs::read_dir(&dir)?.flatten() {
        let ended = std::fs::read_to_string(entry.path()).ok().and_then(|t| t.trim().parse::<u64>().ok()).is_none_or(|ends| ends <= now());
        if ended && !entry.file_name().to_string_lossy().starts_with('.') {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let code = random_hex(16)?;
    crate::net::write_private(&dir.join(hash(&code)), &(now() + CODE_SECONDS).to_string())?;
    Ok(code)
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn random_hex(bytes: usize) -> Result<String> {
    let mut buffer = vec![0u8; bytes];
    std::io::Read::read_exact(&mut std::fs::File::open("/dev/urandom")?, &mut buffer)?;
    Ok(buffer.iter().map(|b| format!("{b:02x}")).collect())
}

fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Compare without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ").map(str::trim).filter(|t| !t.is_empty())
}

/// Does the request come from the backend's own page (or from no page at all, as the
/// terminal client's do)? A page of another site, open in the user's browser, could
/// otherwise send requests here; in local mode nothing else would stop it.
pub fn same_origin(headers: &HeaderMap, authority: Option<&str>) -> bool {
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let host = origin.strip_prefix("https://").or_else(|| origin.strip_prefix("http://")).unwrap_or("");
        return !host.is_empty() && authority.is_some_and(|a| a.eq_ignore_ascii_case(host));
    }
    // Browsers that send no Origin still say where a request comes from.
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        Some(site) => site == "same-origin" || site == "none",
        None => true,
    }
}

impl Auth {
    pub fn new(mode: Mode, token: String, fingerprint: String, home: &Path) -> Self {
        let file = home.join("sessions");
        let mut sessions = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(&file) {
            for line in text.lines() {
                if let Some((hash, expires)) = line.split_once(' ') {
                    if let Ok(expires) = expires.parse::<u64>() {
                        if expires > now() {
                            sessions.insert(hash.to_string(), expires);
                        }
                    }
                }
            }
        }
        Self { mode: std::sync::RwLock::new(mode), token, fingerprint, sessions: Mutex::new(sessions), challenges: Mutex::new(HashMap::new()), failures: Mutex::new((0, 0)), file, codes: home.join("codes") }
    }

    /// Exchange a link's one-time code (see `issue_code`) for the backend's token: only from
    /// this machine, where the link was printed and where the token works; once; and while it
    /// is good. A request whose address is not known is not from this machine.
    pub fn redeem(&self, code: &str, peer: Option<IpAddr>) -> Result<String> {
        if !peer.is_some_and(|ip| ip.is_loopback()) {
            bail!("a link from `cwbcode url` works only on the machine the backend runs on");
        }
        // (Named by its hash: whatever is sent, it names no file outside the folder.)
        let file = self.codes.join(hash(code.trim()));
        let ends = std::fs::read_to_string(&file).ok().and_then(|text| text.trim().parse::<u64>().ok());
        // Used up whatever comes next; of two requests with the same code, one removes it.
        let removed = std::fs::remove_file(&file).is_ok();
        match ends {
            Some(ends) if removed && ends > now() => Ok(self.token.clone()),
            _ => bail!("that link has expired or was used already: run `cwbcode url` for a new one"),
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode.read().unwrap().clone()
    }

    /// Change how clients sign in. Sessions and pending sign-ins of the mode before end.
    pub fn set_mode(&self, mode: Mode) {
        let mut current = self.mode.write().unwrap();
        if *current != mode {
            *current = mode;
            let mut sessions = self.sessions.lock().unwrap();
            sessions.clear();
            self.save(&sessions);
            self.challenges.lock().unwrap().clear();
        }
    }

    /// "open", "wallet" or "token".
    pub fn label(&self) -> &'static str {
        match self.mode() {
            Mode::Open => "open",
            Mode::Wallet(_) => "wallet",
            Mode::Token => "token",
        }
    }

    pub fn wallet(&self) -> String {
        match self.mode() {
            Mode::Wallet(address) => address,
            _ => String::new(),
        }
    }

    fn save(&self, sessions: &HashMap<String, u64>) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let text: String = sessions.iter().map(|(hash, expires)| format!("{hash} {expires}\n")).collect();
        if let Ok(mut file) = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&self.file) {
            let _ = file.write_all(text.as_bytes());
        }
    }

    fn session(&self, token: &str) -> bool {
        self.sessions.lock().unwrap().get(&hash(token)).is_some_and(|expires| *expires > now())
    }

    /// May this request use the API? `peer` is where it came from, when known.
    pub fn allowed(&self, headers: &HeaderMap, authority: Option<&str>, peer: Option<IpAddr>) -> bool {
        if !same_origin(headers, authority) {
            return false;
        }
        let local = peer.is_none_or(|ip| ip.is_loopback());
        let own = |given: &str| same(given.as_bytes(), self.token.as_bytes());
        match &self.mode() {
            Mode::Open => local && bearer(headers).is_some_and(own),
            Mode::Token => bearer(headers).is_some_and(own),
            Mode::Wallet(_) => bearer(headers).is_some_and(|given| self.session(given) || (local && own(given))),
        }
    }

    /// A one-time message for the wallet to sign.
    pub fn challenge(&self) -> Result<String> {
        let Mode::Wallet(address) = self.mode() else { bail!("this backend does not sign in with a wallet") };
        let (nonce, expires) = (random_hex(16)?, now() + wallet::CHALLENGE_SECONDS);
        let mut challenges = self.challenges.lock().unwrap();
        challenges.retain(|_, ends| *ends > now());
        if challenges.len() >= MAX_CHALLENGES {
            bail!("too many sign-ins are in progress; try again in a few minutes");
        }
        challenges.insert(nonce.clone(), expires);
        Ok(wallet::challenge(&address, &self.fingerprint, &nonce, expires))
    }

    /// Sign in with a challenge and the wallet's signature of it. Returns a session token.
    pub fn login(&self, message: &str, signature: &str) -> Result<String> {
        let Mode::Wallet(address) = self.mode() else { bail!("this backend does not sign in with a wallet") };
        {
            let mut failures = self.failures.lock().unwrap();
            let minute = now() / 60;
            if failures.0 != minute {
                *failures = (minute, 0);
            }
            if failures.1 >= FAILURES_PER_MINUTE {
                bail!("too many failed sign-ins; wait a minute");
            }
        }
        let result = self.check(&address, message, signature);
        if result.is_err() {
            self.failures.lock().unwrap().1 += 1;
        }
        result?;
        let token = random_hex(32)?;
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, ends| *ends > now());
        sessions.insert(hash(&token), now() + SESSION_SECONDS);
        self.save(&sessions);
        Ok(token)
    }

    fn check(&self, address: &str, message: &str, signature: &str) -> Result<()> {
        // The challenge must be one this server gave out, for this server and wallet, still
        // good; it is used up whatever comes next.
        let nonce = wallet::challenge_field(message, "nonce").unwrap_or("");
        let issued = self.challenges.lock().unwrap().remove(nonce);
        let expected = issued.map(|expires| (expires, wallet::challenge(address, &self.fingerprint, nonce, expires)));
        match expected {
            Some((expires, text)) if expires > now() && text == message => {}
            _ => bail!("that sign-in request has expired or was already used; try again"),
        }
        let signer = wallet::signer(signature, message).map_err(|e| anyhow::anyhow!("{e}"))?;
        if !wallet::same_address(&signer, address) {
            bail!("signed by wallet {signer}, but this backend belongs to {}", address.to_lowercase());
        }
        Ok(())
    }

    /// End the session a request carries.
    pub fn logout(&self, headers: &HeaderMap) {
        if let Some(token) = bearer(headers) {
            let mut sessions = self.sessions.lock().unwrap();
            if sessions.remove(&hash(token)).is_some() {
                self.save(&sessions);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PHRASE: &str = "test test test test test test test test test test test junk";
    const ADDRESS: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

    fn auth(mode: Mode, name: &str) -> (Auth, PathBuf) {
        let home = std::env::temp_dir().join(format!("cwbcode-auth-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        (Auth::new(mode, "the-file-token".into(), "f1f2".into(), &home), home)
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(), value.parse().unwrap());
        }
        map
    }

    fn sign_in(auth: &Auth, phrase: &str, index: u32) -> Result<String> {
        let message = auth.challenge()?;
        let key = wallet::key_from_phrase(phrase, index).unwrap();
        auth.login(&message, &wallet::sign(&key, &message).unwrap())
    }

    const HERE: Option<IpAddr> = Some(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    const ELSEWHERE: Option<IpAddr> = Some(IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 50)));

    #[test]
    fn local_mode_takes_the_token_from_this_machine_and_its_own_page_only() {
        let (auth, _) = auth(Mode::Open, "open");
        let host = Some("127.0.0.1:7443");
        let token = ("authorization", "Bearer the-file-token");
        // The terminal client (no page), and the backend's own page, with the token.
        assert!(auth.allowed(&headers(&[token]), host, HERE));
        assert!(auth.allowed(&headers(&[token, ("origin", "https://127.0.0.1:7443")]), host, HERE));
        assert!(auth.allowed(&headers(&[token, ("sec-fetch-site", "same-origin")]), host, HERE));
        // Without it, or with a wrong one, nothing: any program on this machine could ask.
        assert!(!auth.allowed(&headers(&[]), host, HERE));
        assert!(!auth.allowed(&headers(&[("authorization", "Bearer a-guess")]), host, HERE));
        // A page of another site in the same browser, however it asks.
        for foreign in [vec![token, ("origin", "https://evil.example")], vec![token, ("origin", "null")], vec![token, ("origin", "https://127.0.0.1:9999")], vec![token, ("sec-fetch-site", "cross-site")], vec![token, ("sec-fetch-site", "same-site")]] {
            assert!(!auth.allowed(&headers(&foreign), host, HERE), "{foreign:?}");
        }
        // Another machine, should the port ever be reachable from one, even with the token.
        assert!(!auth.allowed(&headers(&[token]), host, ELSEWHERE));
        assert!(auth.challenge().is_err());
    }

    #[test]
    fn with_a_wallet_only_its_signature_signs_in() {
        let (auth, home) = auth(Mode::Wallet(ADDRESS.into()), "wallet");
        let host = Some("mac.local:7443");
        let with = |token: &str| headers(&[("authorization", &format!("Bearer {token}"))]);
        assert!(!auth.allowed(&headers(&[]), host, ELSEWHERE));
        assert!(!auth.allowed(&headers(&[]), host, HERE));
        // The right phrase signs in; the session works from anywhere, until it is ended.
        let session = sign_in(&auth, PHRASE, 0).unwrap();
        assert_eq!(session.len(), 64);
        assert!(auth.allowed(&with(&session), host, ELSEWHERE));
        assert!(auth.allowed(&headers(&[("authorization", &format!("Bearer {session}")), ("origin", "https://mac.local:7443")]), host, ELSEWHERE));
        assert!(!auth.allowed(&headers(&[("authorization", &format!("Bearer {session}")), ("origin", "https://evil.example")]), host, ELSEWHERE));
        // It survives a restart (only its hash is on disk), and the file is private.
        use std::os::unix::fs::PermissionsExt;
        assert!(!std::fs::read_to_string(home.join("sessions")).unwrap().contains(&session));
        assert_eq!(std::fs::metadata(home.join("sessions")).unwrap().permissions().mode() & 0o777, 0o600);
        let restarted = Auth::new(Mode::Wallet(ADDRESS.into()), "the-file-token".into(), "f1f2".into(), &home);
        assert!(restarted.allowed(&with(&session), host, ELSEWHERE));
        auth.logout(&with(&session));
        assert!(!auth.allowed(&with(&session), host, ELSEWHERE));
        // Another wallet (the same phrase's second account, or another phrase) does not.
        let refused = sign_in(&auth, PHRASE, 1).unwrap_err().to_string();
        assert!(refused.contains("0x70997970c51812dc3a010c7d01b50e0d17dc79c8") && refused.contains("belongs to"), "{refused}");
        assert!(sign_in(&auth, "legal winner thank year wave sausage worth useful legal winner thank yellow", 0).is_err());
        // The backend's own token: from this machine only.
        assert!(auth.allowed(&with("the-file-token"), host, HERE));
        assert!(!auth.allowed(&with("the-file-token"), host, ELSEWHERE));
        assert!(!auth.allowed(&with("a-guess"), host, HERE));
    }

    #[test]
    fn a_challenge_is_good_once_for_this_server_and_guessing_is_slowed() {
        let (auth, _) = auth(Mode::Wallet(ADDRESS.into()), "challenge");
        let key = wallet::key_from_phrase(PHRASE, 0).unwrap();
        // Used once.
        let message = auth.challenge().unwrap();
        let signature = wallet::sign(&key, &message).unwrap();
        assert!(auth.login(&message, &signature).is_ok());
        assert!(auth.login(&message, &signature).unwrap_err().to_string().contains("already used"));
        // A message this server did not issue: made up, altered, or another server's.
        let forged = wallet::challenge(ADDRESS, "f1f2", "00112233445566778899aabbccddeeff", now() + 100);
        assert!(auth.login(&forged, &wallet::sign(&key, &forged).unwrap()).is_err());
        let real = auth.challenge().unwrap();
        let altered = real.replace("f1f2", "0000");
        assert!(auth.login(&altered, &wallet::sign(&key, &altered).unwrap()).is_err());
        // (That attempt used the challenge up as well.)
        assert!(auth.login(&real, &wallet::sign(&key, &real).unwrap()).is_err());
        // After enough failures, even the right signature waits.
        for _ in 0..FAILURES_PER_MINUTE {
            let _ = auth.login("nonsense", "00");
        }
        let message = auth.challenge().unwrap();
        assert!(auth.login(&message, &wallet::sign(&key, &message).unwrap()).unwrap_err().to_string().contains("too many"));
    }

    #[test]
    fn a_links_code_gives_the_token_once_on_this_machine_while_it_is_good() {
        use std::os::unix::fs::PermissionsExt;
        let (auth, home) = auth(Mode::Open, "code");
        let code = issue_code(&home).unwrap();
        assert_eq!(code.len(), 32);
        // Kept by its hash only, readable by this user only.
        let names: Vec<String> = std::fs::read_dir(home.join("codes")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec![hash(&code)]);
        assert_eq!(std::fs::metadata(home.join("codes")).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(std::fs::metadata(home.join("codes").join(hash(&code))).unwrap().permissions().mode() & 0o777, 0o600);
        // From another machine, or from nowhere known: nothing, and the code is not used up.
        assert!(auth.redeem(&code, ELSEWHERE).is_err());
        assert!(auth.redeem(&code, None).is_err());
        // From this machine: the token, once.
        assert_eq!(auth.redeem(&code, HERE).unwrap(), "the-file-token");
        assert!(auth.redeem(&code, HERE).unwrap_err().to_string().contains("used already"));
        assert!(auth.redeem("a-guess", HERE).is_err());
        assert!(auth.redeem("../sessions", HERE).is_err());
        // A code that has ended does not work, and is cleared away with the next one issued.
        let old = "00112233445566778899aabbccddeeff";
        std::fs::write(home.join("codes").join(hash(old)), (now() - 1).to_string()).unwrap();
        assert!(auth.redeem(old, HERE).is_err());
        std::fs::write(home.join("codes").join(hash(old)), (now() - 1).to_string()).unwrap();
        let fresh = issue_code(&home).unwrap();
        assert!(!home.join("codes").join(hash(old)).exists());
        assert_eq!(auth.redeem(&fresh, HERE).unwrap(), "the-file-token");
    }

    #[test]
    fn the_token_mode_takes_the_token_from_anywhere() {
        let (auth, _) = auth(Mode::Token, "token");
        let with = |token: &str| headers(&[("authorization", &format!("Bearer {token}"))]);
        assert!(auth.allowed(&with("the-file-token"), None, ELSEWHERE));
        assert!(!auth.allowed(&with("the-file-toke"), None, ELSEWHERE));
        assert!(!auth.allowed(&headers(&[]), None, HERE));
        assert_eq!((auth.label(), auth.wallet().as_str()), ("token", ""));
    }

    #[test]
    fn changing_the_mode_ends_the_sessions_of_the_one_before() {
        let (auth, _) = auth(Mode::Wallet(ADDRESS.into()), "switch");
        let with = |token: &str| headers(&[("authorization", &format!("Bearer {token}"))]);
        let session = sign_in(&auth, PHRASE, 0).unwrap();
        assert!(auth.allowed(&with(&session), None, ELSEWHERE));
        // Remote mode off: local, with the token from this machine; nobody from elsewhere.
        auth.set_mode(Mode::Open);
        assert_eq!(auth.label(), "open");
        assert!(auth.allowed(&with("the-file-token"), None, HERE));
        assert!(!auth.allowed(&headers(&[]), None, HERE));
        assert!(!auth.allowed(&with(&session), None, ELSEWHERE));
        // On again, for another wallet: the old session is not a way in.
        auth.set_mode(Mode::Wallet("0x70997970C51812dc3A010C7d01b50e0d17dc79C8".into()));
        assert!(!auth.allowed(&with(&session), None, ELSEWHERE));
        assert!(sign_in(&auth, PHRASE, 0).is_err());
        assert!(auth.allowed(&with(&sign_in(&auth, PHRASE, 1).unwrap()), None, ELSEWHERE));
    }
}
