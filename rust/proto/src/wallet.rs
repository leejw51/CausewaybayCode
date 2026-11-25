//! Signing in with a wallet. The backend is told one wallet address (CWBCODE_WALLET); whoever
//! can sign with that wallet's key may use it. The key is derived from the recovery phrase
//! where it is typed (the browser, or the terminal client) and never sent: what goes to the
//! backend is a signature of a one-time challenge.
//!
//! The same derivation and signatures as an Ethereum wallet (and as Fruit Nation's): BIP-39
//! phrase, BIP-32 path m/44'/60'/0'/0/index, EIP-191 `personal_sign`.

use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
use sha3::{Digest, Keccak256};

/// How long a challenge can be answered, in seconds.
pub const CHALLENGE_SECONDS: u64 = 300;

/// The wallet key of a recovery phrase (12 to 24 words), at an account index (0: the first).
pub fn key_from_phrase(phrase: &str, index: u32) -> Result<[u8; 32], String> {
    if index >= 1 << 31 {
        return Err("the wallet index must be below 2147483648".into());
    }
    // As typed: any spacing, any letter case.
    let words = phrase.split_whitespace().map(|w| w.to_lowercase()).collect::<Vec<_>>().join(" ");
    let mnemonic = bip39::Mnemonic::parse(&words).map_err(|_| "that is not a valid recovery phrase (12 to 24 words from the BIP-39 list, with a correct checksum)".to_string())?;
    let path = format!("m/44'/60'/0'/0/{index}").parse().map_err(|_| "invalid derivation path".to_string())?;
    let key = bip32::XPrv::derive_from_path(mnemonic.to_seed(""), &path).map_err(|_| "the wallet key could not be derived".to_string())?;
    Ok(key.private_key().to_bytes().into())
}

fn address_of_key(key: &VerifyingKey) -> String {
    let point = key.to_encoded_point(false);
    let hash = Keccak256::digest(&point.as_bytes()[1..]);
    format!("0x{}", hash[12..].iter().map(|b| format!("{b:02x}")).collect::<String>())
}

/// The address (0x and 40 lowercase hex digits) of a wallet key.
pub fn address(secret: &[u8; 32]) -> Result<String, String> {
    let key = SigningKey::from_slice(secret).map_err(|_| "invalid wallet key".to_string())?;
    Ok(address_of_key(key.verifying_key()))
}

/// Two spellings of one address? (Letter case carries only a checksum.)
pub fn same_address(a: &str, b: &str) -> bool {
    !a.is_empty() && a.trim().eq_ignore_ascii_case(b.trim())
}

fn digest(message: &[u8]) -> Keccak256 {
    let mut d = Keccak256::new();
    d.update(format!("\x19Ethereum Signed Message:\n{}", message.len()));
    d.update(message);
    d
}

/// What a wallet's `personal_sign` returns for `message`: 65 bytes, r ‖ s ‖ v, in hex.
pub fn sign(secret: &[u8; 32], message: &str) -> Result<String, String> {
    let key = SigningKey::from_slice(secret).map_err(|_| "invalid wallet key".to_string())?;
    let (signature, id) = key.sign_digest_recoverable(digest(message.as_bytes())).map_err(|_| "signing failed".to_string())?;
    let mut bytes = signature.to_bytes().to_vec();
    bytes.push(id.to_byte());
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The address whose key made `signature` (hex, 65 bytes, v in {0, 1} or {27, 28}) of `message`.
pub fn signer(signature: &str, message: &str) -> Result<String, String> {
    let hex = signature.trim().trim_start_matches("0x");
    if hex.len() != 130 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("a signature is 65 bytes in hex".into());
    }
    let bytes: Vec<u8> = (0..65).map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap_or(0)).collect();
    let signature = Signature::from_slice(&bytes[..64]).map_err(|_| "invalid signature".to_string())?;
    let v = bytes[64];
    let id = RecoveryId::from_byte(if v >= 27 { v - 27 } else { v }).ok_or("invalid signature")?;
    let key = VerifyingKey::recover_from_digest(digest(message.as_bytes()), &signature, id).map_err(|_| "the signature does not verify".to_string())?;
    Ok(address_of_key(&key))
}

/// The message a wallet signs to sign in. It names the server (by its certificate) and the
/// wallet, so a signature cannot be used for another server, and it is good once, briefly.
pub fn challenge(wallet: &str, fingerprint: &str, nonce: &str, expires: u64) -> String {
    format!("Causewaybay Code sign-in\n\nSigning this message signs you in to the cwbcode backend below. It moves no funds.\n\nwallet: {}\nserver: {fingerprint}\nnonce: {nonce}\nexpires: {expires}", wallet.to_lowercase())
}

/// A field of a challenge ("wallet", "server", "nonce", "expires").
pub fn challenge_field<'a>(message: &'a str, name: &str) -> Option<&'a str> {
    message.lines().find_map(|line| line.strip_prefix(name)?.strip_prefix(": "))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The well-known test phrase and its first two addresses.
    const PHRASE: &str = "test test test test test test test test test test test junk";

    #[test]
    fn a_phrase_gives_the_wallet_every_ethereum_wallet_gives() {
        let key = key_from_phrase(PHRASE, 0).unwrap();
        assert_eq!(address(&key).unwrap(), "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266");
        assert_eq!(address(&key_from_phrase(PHRASE, 1).unwrap()).unwrap(), "0x70997970c51812dc3a010c7d01b50e0d17dc79c8");
        // As typed: extra spaces, capitals, line breaks.
        assert_eq!(key_from_phrase("  Test test\ttest TEST test test\n test test test test test   junk ", 0).unwrap(), key);
        for bad in ["", "invalid words", "test test test test test test test test test test test test", &format!("{PHRASE} extra")] {
            assert!(key_from_phrase(bad, 0).is_err(), "{bad:?}");
        }
        assert!(key_from_phrase(PHRASE, 1 << 31).is_err());
        assert!(same_address("0xF39Fd6e51aad88F6F4ce6aB8827279cffFb92266", " 0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"));
        assert!(!same_address("", "") && !same_address("0xf39f", "0x7099"));
    }

    #[test]
    fn a_signature_names_its_signer_and_nothing_else_passes() {
        let (key, other) = (key_from_phrase(PHRASE, 0).unwrap(), key_from_phrase(PHRASE, 1).unwrap());
        let message = challenge("0xF39Fd6e51aad88F6F4ce6aB8827279cffFb92266", "ab12", "00ff", 1_800_000_000);
        let signature = sign(&key, &message).unwrap();
        assert_eq!(signature.len(), 130);
        assert_eq!(signer(&signature, &message).unwrap(), "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266");
        // The same key signs the same message the same way (RFC 6979), and wallets' v = 27/28 is read too.
        assert_eq!(sign(&key, &message).unwrap(), signature);
        let v = u8::from_str_radix(&signature[128..], 16).unwrap() + 27;
        assert_eq!(signer(&format!("0x{}{v:02x}", &signature[..128]), &message).unwrap(), "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266");
        // Another key, another message, a damaged signature: another signer or none.
        assert_ne!(signer(&sign(&other, &message).unwrap(), &message).unwrap(), "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266");
        assert_ne!(signer(&signature, &format!("{message} ")).ok().as_deref(), Some("0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"));
        for bad in ["", "zz", &signature[..128], &format!("{}05", &signature[..128]), &"0".repeat(130)] {
            assert!(signer(bad, &message).is_err(), "{bad}");
        }
        // The challenge says what it is for.
        assert_eq!(challenge_field(&message, "wallet"), Some("0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"));
        assert_eq!((challenge_field(&message, "server"), challenge_field(&message, "nonce"), challenge_field(&message, "expires")), (Some("ab12"), Some("00ff"), Some("1800000000")));
        assert_eq!(challenge_field(&message, "missing"), None);
    }
}
