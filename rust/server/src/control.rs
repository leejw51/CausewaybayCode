//! Local mode and remote mode, and switching between them while the backend runs.
//!
//!   local    no wallet: the backend listens on this machine only and asks for its token
//!            (~/.cwbcode/token), from this machine only.
//!   remote   a wallet is set: it listens on every interface and its owner signs in.
//!
//! Setting a wallet up (from the web client's dashboard or `cwbcodeclient remote on`) writes
//! it to ~/.cwbcode/settings.jsonl and takes effect at once: the sign-in rule changes and
//! the listeners move to the new address.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Result, bail};

use crate::auth::{Auth, Mode};
use crate::env::{self, Wallet};
use crate::settings;

pub struct Control {
    pub home: PathBuf,
    /// The address asked for with --bind, which setting up does not change.
    pub asked: Option<SocketAddr>,
    pub port: u16,
    pub require_token: bool,
    /// What fixes the wallet (the .env file or the environment), if anything does.
    pub fixed: Option<String>,
    /// Where the backend listens now, and where the wallet in force was set.
    pub bind: Mutex<SocketAddr>,
    pub source: Mutex<String>,
    /// Tells the listeners to move.
    pub rebind: tokio::sync::watch::Sender<SocketAddr>,
}

/// How clients sign in: with the wallet when there is one; not at all in local mode (no
/// wallet, this machine only); with the backend's token otherwise.
pub fn mode(wallet: &Option<Wallet>, bind: SocketAddr, require_token: bool) -> Mode {
    match wallet {
        Some(w) => Mode::Wallet(w.address.clone()),
        None if bind.ip().is_loopback() && !require_token => Mode::Open,
        None => Mode::Token,
    }
}

impl Control {
    /// Turn remote mode on (a wallet address) or off (""), now and for later starts.
    /// Returns what was done, for the user.
    pub fn setup(&self, auth: &Auth, value: &str) -> Result<String> {
        if let Some(source) = &self.fixed {
            bail!("the wallet is set in {source}: change or remove CWBCODE_WALLET there and restart the backend");
        }
        let value = value.trim();
        let wallet = if value.is_empty() { None } else { Some(Wallet { address: env::address(value, "the setup")?, source: "settings".to_string() }) };
        settings::set(&self.home, "wallet", wallet.as_ref().map_or("", |w| w.address.as_str()))?;
        let bind = env::bind(self.asked, &wallet, self.port);
        auth.set_mode(mode(&wallet, bind, self.require_token));
        *self.source.lock().unwrap() = if wallet.is_some() { "settings".to_string() } else { String::new() };
        let moved = {
            let mut current = self.bind.lock().unwrap();
            let moved = *current != bind;
            *current = bind;
            moved
        };
        if moved {
            let _ = self.rebind.send(bind);
        }
        Ok(match &wallet {
            Some(w) => format!("Remote mode is on: wallet {}, listening on {bind}. Sign in with the wallet's recovery phrase.", w.address),
            None => format!("Remote mode is off: listening on {bind}, with the backend's token from this machine."),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADDRESS: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

    fn control(name: &str, asked: Option<SocketAddr>, fixed: Option<String>) -> (Control, Auth, tokio::sync::watch::Receiver<SocketAddr>) {
        let home = std::env::temp_dir().join(format!("cwbcode-control-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let bind = env::bind(asked, &None, 7443);
        let (rebind, moved) = tokio::sync::watch::channel(bind);
        let auth = Auth::new(mode(&None, bind, false), "file-token".into(), "f1".into(), &home);
        (Control { home, asked, port: 7443, require_token: false, fixed, bind: Mutex::new(bind), source: Mutex::new(String::new()), rebind }, auth, moved)
    }

    #[test]
    fn remote_mode_is_turned_on_and_off_at_once_and_remembered() {
        let (control, auth, mut moved) = control("toggle", None, None);
        assert_eq!((auth.label(), control.bind.lock().unwrap().to_string()), ("open", "127.0.0.1:7443".to_string()));
        // On: the wallet signs in, the listeners are told to move, the setting is on disk.
        let said = control.setup(&auth, &format!("  {ADDRESS} ")).unwrap();
        assert!(said.contains("Remote mode is on") && said.contains("0.0.0.0:7443"), "{said}");
        assert_eq!((auth.label(), auth.wallet().as_str()), ("wallet", ADDRESS));
        assert!(moved.has_changed().unwrap());
        assert_eq!(moved.borrow_and_update().to_string(), "0.0.0.0:7443");
        assert_eq!(settings::wallet(&control.home).as_deref(), Some(ADDRESS));
        assert_eq!(*control.source.lock().unwrap(), "settings");
        // What a restart would read.
        let read = env::wallet(None, Some(&control.home.join("no.env")), &control.home).unwrap().unwrap();
        assert_eq!((read.address.as_str(), read.fixed()), (ADDRESS, false));
        // Off again.
        let said = control.setup(&auth, "").unwrap();
        assert!(said.contains("Remote mode is off") && said.contains("127.0.0.1:7443"), "{said}");
        assert_eq!(auth.label(), "open");
        assert_eq!(moved.borrow_and_update().to_string(), "127.0.0.1:7443");
        assert_eq!(settings::wallet(&control.home), None);
        assert_eq!(std::fs::read_to_string(control.home.join(settings::FILE)).unwrap().lines().count(), 2);
    }

    #[test]
    fn what_is_not_an_address_changes_nothing_and_a_fixed_wallet_stays() {
        let (control, auth, moved) = control("refuse", None, None);
        for bad in ["0x1234", "4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318", "legal winner thank year wave sausage worth useful legal winner thank yellow"] {
            let refused = control.setup(&auth, bad).unwrap_err().to_string();
            assert!(!refused.contains("sausage") && !refused.contains("4c0883a6"), "{refused}");
        }
        assert_eq!(auth.label(), "open");
        assert!(!moved.has_changed().unwrap());
        assert!(!control.home.join(settings::FILE).exists());
        // Set in .env: not for setting up to change.
        let (control, auth, _) = super::tests::control("fixed", None, Some("/repo/.env".into()));
        let refused = control.setup(&auth, ADDRESS).unwrap_err().to_string();
        assert!(refused.contains("/repo/.env"), "{refused}");
        // With an address asked for outright, only the sign-in rule changes.
        let (control, auth, moved) = super::tests::control("asked", Some("127.0.0.1:9000".parse().unwrap()), None);
        control.setup(&auth, ADDRESS).unwrap();
        assert_eq!((auth.label(), control.bind.lock().unwrap().to_string()), ("wallet", "127.0.0.1:9000".to_string()));
        assert!(!moved.has_changed().unwrap());
    }
}
