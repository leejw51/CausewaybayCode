//! Settings read from a `.env` file. One of them decides who can reach the backend:
//!
//!   CWBCODE_WALLET=0x...   a wallet address. With it, the backend listens on every interface
//!                          (0.0.0.0); without it, only on this machine (127.0.0.1).
//!
//! A variable set in the environment itself wins over the file. Without either, the wallet
//! may have been set up while the backend ran (settings.rs): that one can be changed again
//! the same way, the other two only where they are written. Lines are `KEY=VALUE`, with
//! an optional `export` in front, optional quotes, and `#` comments.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

pub const WALLET: &str = "CWBCODE_WALLET";

#[derive(Debug, Clone, PartialEq)]
pub struct Wallet {
    /// 0x and 40 hex digits.
    pub address: String,
    /// Where it was found, for the user: "the environment", the .env file's path, or
    /// "settings" (set up while the backend ran).
    pub source: String,
}

impl Wallet {
    /// Set in the environment or a .env file: not changed by setting up.
    pub fn fixed(&self) -> bool {
        self.source != "settings"
    }
}

/// The variables of a `.env` text.
pub fn parse(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        // `export KEY=...` as a shell would have it (and `extern`, a slip seen in the wild).
        let key = key.trim();
        let key = key.strip_prefix("export ").or_else(|| key.strip_prefix("extern ")).unwrap_or(key).trim();
        let mut value = value.trim();
        let quoted = value.len() >= 2 && ((value.starts_with('"') && value.ends_with('"')) || (value.starts_with('\'') && value.ends_with('\'')));
        if quoted {
            value = &value[1..value.len() - 1];
        } else if let Some(at) = value.find(" #") {
            value = value[..at].trim();
        }
        if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            out.insert(key.to_string(), value.to_string());
        }
    }
    out
}

/// A wallet address, or why the value is not one. The value itself is never echoed: if it is
/// not an address it may be something secret.
pub fn address(value: &str, source: &str) -> Result<String> {
    let value = value.trim();
    let hex = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")).unwrap_or("");
    if hex.len() == 40 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(format!("0x{hex}"));
    }
    let words = value.split_whitespace().count();
    let looks_like = if words >= 12 {
        "it looks like a recovery phrase"
    } else if value.trim_start_matches("0x").len() == 64 && value.trim_start_matches("0x").chars().all(|c| c.is_ascii_hexdigit()) {
        "it looks like a private key"
    } else {
        "it is not one"
    };
    bail!("{WALLET} in {source} must be a wallet address (0x and 40 hex digits); {looks_like}. Put only the public address there, never a private key or a recovery phrase.")
}

/// The wallet, if one is set: from the environment, else from `file` (default: `.env` in the
/// current folder, if there is one), else the one set up while running (in `home`).
pub fn wallet(environment: Option<String>, file: Option<&Path>, home: &Path) -> Result<Option<Wallet>> {
    if let Some(found) = fixed_wallet(environment, file)? {
        return Ok(Some(found));
    }
    match crate::settings::wallet(home) {
        Some(value) => Ok(Some(Wallet { address: address(value.as_str(), "settings.jsonl")?, source: "settings".to_string() })),
        None => Ok(None),
    }
}

fn fixed_wallet(environment: Option<String>, file: Option<&Path>) -> Result<Option<Wallet>> {
    if let Some(value) = environment.filter(|v| !v.trim().is_empty()) {
        return Ok(Some(Wallet { address: address(&value, "the environment")?, source: "the environment".to_string() }));
    }
    let path = match file {
        Some(path) => path.to_path_buf(),
        None => PathBuf::from(".env"),
    };
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
    let shown = path.canonicalize().unwrap_or(path).display().to_string();
    match parse(&text).get(WALLET).filter(|v| !v.trim().is_empty()) {
        Some(value) => Ok(Some(Wallet { address: address(value, &shown)?, source: shown })),
        None => Ok(None),
    }
}

/// Where the backend listens: what was asked for, else every interface with a wallet and
/// this machine only without one.
pub fn bind(asked: Option<SocketAddr>, wallet: &Option<Wallet>, port: u16) -> SocketAddr {
    asked.unwrap_or_else(|| SocketAddr::new(IpAddr::V4(if wallet.is_some() { Ipv4Addr::UNSPECIFIED } else { Ipv4Addr::LOCALHOST }), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: &str = "0x52908400098527886E0F7030069857D2E4169EE7";

    #[test]
    fn a_dotenv_file_is_read_as_a_shell_would_write_it() {
        let vars = parse(&format!("# settings\n\nexport CWBCODE_WALLET={ADDRESS}\nextern OTHER = \"two words\"  \nQUOTED='x # y'\nPLAIN=value # a comment\nnot a line\n=empty\nBAD KEY=1\n"));
        assert_eq!(vars.get("CWBCODE_WALLET").map(String::as_str), Some(ADDRESS));
        assert_eq!(vars.get("OTHER").map(String::as_str), Some("two words"));
        assert_eq!(vars.get("QUOTED").map(String::as_str), Some("x # y"));
        assert_eq!(vars.get("PLAIN").map(String::as_str), Some("value"));
        assert_eq!(vars.len(), 4);
    }

    #[test]
    fn the_wallet_decides_who_can_reach_the_backend() {
        let dir = std::env::temp_dir().join(format!("cwbcode-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(".env");
        // No file, or a file without the wallet: this machine only.
        assert_eq!(wallet(None, Some(&file), &dir).unwrap(), None);
        std::fs::write(&file, "OTHER=1\nCWBCODE_WALLET=\n").unwrap();
        let none = wallet(None, Some(&file), &dir).unwrap();
        assert_eq!(none, None);
        assert_eq!(bind(None, &none, 7443).to_string(), "127.0.0.1:7443");
        // With the wallet: every interface, and the address is known.
        std::fs::write(&file, format!("extern CWBCODE_WALLET={ADDRESS}\n")).unwrap();
        let found = wallet(None, Some(&file), &dir).unwrap();
        let w = found.clone().unwrap();
        assert_eq!(w.address, ADDRESS);
        assert!(w.source.ends_with(".env"));
        assert_eq!(bind(None, &found, 7443).to_string(), "0.0.0.0:7443");
        // An address asked for outright wins either way; so does the environment over the file.
        assert_eq!(bind(Some("127.0.0.1:9000".parse().unwrap()), &found, 7443).to_string(), "127.0.0.1:9000");
        assert_eq!(bind(Some("0.0.0.0:9000".parse().unwrap()), &none, 7443).to_string(), "0.0.0.0:9000");
        assert!(found.as_ref().unwrap().fixed());
        // Set up while running: used when nothing else sets one, and not fixed.
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        crate::settings::set(&home, "wallet", "0x8ba1f109551bD432803012645Ac136ddd64DBA72").unwrap();
        let empty = dir.join("none.env");
        let set_up = wallet(None, Some(&empty), &home).unwrap().unwrap();
        assert_eq!((set_up.address.as_str(), set_up.source.as_str(), set_up.fixed()), ("0x8ba1f109551bD432803012645Ac136ddd64DBA72", "settings", false));
        assert_eq!(wallet(None, Some(&file), &home).unwrap().unwrap().address, ADDRESS, "the .env file wins");
        let other = "0xde709f2102306220921060314715629080e2fb77";
        assert_eq!(wallet(Some(other.to_string()), Some(&file), &dir).unwrap().unwrap(), Wallet { address: other.to_string(), source: "the environment".to_string() });
    }

    #[test]
    fn a_secret_in_place_of_the_address_is_refused_and_not_repeated() {
        let key = "4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318";
        let phrase = "legal winner thank year wave sausage worth useful legal winner thank yellow";
        for (secret, hint) in [(key.to_string(), "private key"), (format!("0x{key}"), "private key"), (phrase.to_string(), "recovery phrase"), ("0x1234".to_string(), "not one")] {
            let error = wallet(Some(secret.clone()), None, std::path::Path::new("/nonexistent")).unwrap_err().to_string();
            assert!(error.contains(hint), "{error}");
            assert!(!error.contains(&secret) && !error.contains("sausage") && !error.contains(&key[..16]), "the secret was printed: {error}");
        }
    }
}
