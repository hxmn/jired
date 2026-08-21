//! Envelope encryption for the headless credential fallback.
//!
//! scrypt(passphrase, random salt) → XChaCha20-Poly1305. The KDF parameters and
//! salt travel with the ciphertext as JSON so a future parameter change stays
//! backward compatible.

use crate::error::{JiredError, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};

const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct KdfParams {
    pub algorithm: String,
    pub log_n: u8,
    pub r: u32,
    pub p: u32,
    pub salt: String,
    pub nonce: String,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self {
            algorithm: "scrypt-xchacha20poly1305".into(),
            // ~16 MiB of memory: enough to make offline guessing costly without
            // making a CI run feel stalled.
            log_n: 14,
            r: 8,
            p: 1,
            salt: String::new(),
            nonce: String::new(),
        }
    }
}

fn random_bytes(len: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    getrandom(&mut buf)?;
    Ok(buf)
}

fn getrandom(buf: &mut [u8]) -> Result<()> {
    use rand::TryRngCore;
    rand::rngs::OsRng.try_fill_bytes(buf).map_err(|e| {
        JiredError::generic(format!("no secure randomness available: {e}"))
            .with_hint("jired will not store a credential without it")
    })
}

fn derive(passphrase: &str, params: &KdfParams) -> Result<[u8; KEY_LEN]> {
    let salt = hex::decode(&params.salt).map_err(|_| {
        JiredError::auth("the stored credential is damaged (malformed salt)")
            .with_hint("run `jired init --force` to store it again")
    })?;
    let sp = scrypt::Params::new(params.log_n, params.r, params.p, KEY_LEN)
        .map_err(|e| JiredError::auth(format!("invalid key-derivation parameters: {e}")))?;
    let mut key = [0u8; KEY_LEN];
    scrypt::scrypt(passphrase.as_bytes(), &salt, &sp, &mut key)
        .map_err(|e| JiredError::auth(format!("key derivation failed: {e}")))?;
    Ok(key)
}

/// Encrypt `plaintext`, returning the ciphertext and its JSON parameters.
pub fn seal(plaintext: &[u8], passphrase: &str) -> Result<(Vec<u8>, String)> {
    let mut params = KdfParams {
        salt: hex::encode(random_bytes(SALT_LEN)?),
        nonce: hex::encode(random_bytes(NONCE_LEN)?),
        ..Default::default()
    };
    let key = derive(passphrase, &params)?;
    let cipher = XChaCha20Poly1305::new((&key).into());
    let nonce_bytes = hex::decode(&params.nonce).expect("just encoded");
    let ciphertext = cipher
        .encrypt(XNonce::from_slice(&nonce_bytes), plaintext)
        .map_err(|_| {
            JiredError::generic("failed to encrypt the credential")
                .with_hint("report this: encryption should not fail once a key is derived")
        })?;
    params.algorithm = "scrypt-xchacha20poly1305".into();
    Ok((ciphertext, serde_json::to_string(&params)?))
}

/// Decrypt a blob produced by [`seal`].
pub fn open(ciphertext: &[u8], kdf_json: &str, passphrase: &str) -> Result<Vec<u8>> {
    let params: KdfParams = serde_json::from_str(kdf_json)
        .map_err(|e| JiredError::auth(format!("stored credential metadata is unreadable: {e}")))?;
    if params.algorithm != "scrypt-xchacha20poly1305" {
        return Err(JiredError::auth(format!(
            "unsupported credential encryption `{}`",
            params.algorithm
        )));
    }
    let key = derive(passphrase, &params)?;
    let nonce = hex::decode(&params.nonce).map_err(|_| {
        JiredError::auth("the stored credential is damaged (malformed nonce)")
            .with_hint("run `jired init --force` to store it again")
    })?;
    let cipher = XChaCha20Poly1305::new((&key).into());
    cipher
        .decrypt(XNonce::from_slice(&nonce), ciphertext)
        .map_err(|_| {
            JiredError::auth("cannot decrypt the stored credential").with_hint(format!(
                "check {} — it must match the value used at init",
                super::SECRET_KEY_ENV
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let (blob, kdf) = seal(b"my-token", "hunter2").unwrap();
        assert_ne!(blob, b"my-token");
        assert_eq!(open(&blob, &kdf, "hunter2").unwrap(), b"my-token");
    }

    #[test]
    fn wrong_passphrase_is_an_auth_error_not_garbage() {
        let (blob, kdf) = seal(b"my-token", "right").unwrap();
        let err = open(&blob, &kdf, "wrong").unwrap_err();
        assert_eq!(err.exit_code().as_i32(), 3);
    }

    #[test]
    fn tampering_is_detected() {
        let (mut blob, kdf) = seal(b"my-token", "pw").unwrap();
        blob[0] ^= 0xff;
        assert!(open(&blob, &kdf, "pw").is_err());
    }

    #[test]
    fn each_seal_uses_a_fresh_salt_and_nonce() {
        let (a, ka) = seal(b"same", "pw").unwrap();
        let (b, kb) = seal(b"same", "pw").unwrap();
        assert_ne!(a, b, "identical ciphertexts leak equality of secrets");
        assert_ne!(ka, kb);
    }
}
