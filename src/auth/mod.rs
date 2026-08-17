//! Credential storage [M1.5, D11].
//!
//! Preference order:
//!
//! 1. **OS keychain** — `session.db` stores only a reference.
//! 2. **Encrypted blob** — key derived with scrypt from `JIRED_SECRET_KEY`.
//!    Protects backups and casual exfiltration of `.jired/`; it does *not*
//!    protect against an attacker who has the env var or can run code as this
//!    user. That trade-off is documented rather than hidden.
//! 3. **Plaintext** — only with `--insecure-store`, and it says so loudly.

pub mod crypt;

use crate::error::{JiredError, Result};
use secrecy::{ExposeSecret, SecretString};

pub const KEYRING_SERVICE: &str = "jired";
pub const SECRET_KEY_ENV: &str = "JIRED_SECRET_KEY";
/// Set by CI and tests, which must never write into a developer's real keychain.
pub const NO_KEYRING_ENV: &str = "JIRED_NO_KEYRING";

fn keyring_disabled() -> bool {
    std::env::var(NO_KEYRING_ENV).is_ok_and(|v| !v.is_empty() && v != "0")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretStorage {
    Keyring,
    EncryptedBlob,
    Plaintext,
}

impl SecretStorage {
    pub fn as_str(self) -> &'static str {
        match self {
            SecretStorage::Keyring => "keyring",
            SecretStorage::EncryptedBlob => "encrypted_blob",
            SecretStorage::Plaintext => "plaintext",
        }
    }
}

/// What `session.db` persists for a credential. Never contains a live secret
/// unless storage is [`SecretStorage::Plaintext`].
#[derive(Debug, Clone, Default)]
pub struct StoredSecret {
    pub reference: Option<String>,
    pub blob: Option<Vec<u8>>,
    pub kdf: Option<String>,
}

impl StoredSecret {
    pub fn storage(&self) -> SecretStorage {
        match (&self.reference, &self.kdf) {
            (Some(_), _) => SecretStorage::Keyring,
            (None, Some(_)) => SecretStorage::EncryptedBlob,
            _ => SecretStorage::Plaintext,
        }
    }
}

/// Stable keyring entry name for an account.
pub fn keyring_user(base_url: &str, identity: Option<&str>) -> String {
    let host = base_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    match identity {
        Some(id) => format!("{host}:{id}"),
        None => host.to_string(),
    }
}

/// Persist a token, choosing the strongest available backend.
pub fn store(
    entry_user: &str,
    token: &SecretString,
    allow_plaintext: bool,
) -> Result<StoredSecret> {
    if !keyring_disabled() {
        match keyring_set(entry_user, token) {
            Ok(()) => {
                tracing::debug!("stored credential in the OS keychain");
                return Ok(StoredSecret {
                    reference: Some(entry_user.to_string()),
                    ..Default::default()
                });
            }
            Err(e) => tracing::debug!("keychain unavailable: {e}"),
        }
    }

    match std::env::var(SECRET_KEY_ENV) {
        Ok(key) if !key.is_empty() => {
            let (blob, kdf) = crypt::seal(token.expose_secret().as_bytes(), &key)?;
            tracing::debug!("stored credential as an encrypted blob");
            Ok(StoredSecret {
                reference: None,
                blob: Some(blob),
                kdf: Some(kdf),
            })
        }
        _ if allow_plaintext => {
            eprintln!(
                "warning: storing the credential UNENCRYPTED in .jired/session.db \
                 (--insecure-store). Anyone who can read that file gets your token."
            );
            Ok(StoredSecret {
                reference: None,
                blob: Some(token.expose_secret().as_bytes().to_vec()),
                kdf: None,
            })
        }
        _ => Err(JiredError::auth(
            "no OS keychain is available and no encryption key was provided",
        )
        .with_detail("env", SECRET_KEY_ENV)
        .with_hint(format!(
            "set {SECRET_KEY_ENV} to a passphrase, or pass --insecure-store to \
             accept plaintext storage"
        ))),
    }
}

/// Recover a token previously persisted with [`store`].
pub fn load(stored: &StoredSecret) -> Result<SecretString> {
    if let Some(reference) = &stored.reference {
        let value = keyring_get(reference).map_err(|e| {
            JiredError::auth(format!("cannot read the credential from the keychain: {e}"))
                .with_hint("run `jired init --force` to re-authenticate")
        })?;
        return Ok(SecretString::from(value));
    }
    let blob = stored
        .blob
        .as_ref()
        .ok_or_else(|| JiredError::auth("no credential stored for this account"))?;

    match &stored.kdf {
        Some(kdf) => {
            let key = std::env::var(SECRET_KEY_ENV).map_err(|_| {
                JiredError::auth("the stored credential is encrypted but no key is set")
                    .with_detail("env", SECRET_KEY_ENV)
                    .with_hint(format!(
                        "set {SECRET_KEY_ENV} to the passphrase used at init"
                    ))
            })?;
            let plain = crypt::open(blob, kdf, &key)?;
            Ok(SecretString::from(String::from_utf8(plain).map_err(
                |_| JiredError::auth("stored credential is not valid UTF-8"),
            )?))
        }
        None => Ok(SecretString::from(
            String::from_utf8(blob.clone())
                .map_err(|_| JiredError::auth("stored credential is not valid UTF-8"))?,
        )),
    }
}

/// Best-effort removal; a missing entry is not an error.
pub fn delete(stored: &StoredSecret) {
    if let Some(reference) = &stored.reference {
        if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, reference) {
            let _ = entry.delete_credential();
        }
    }
}

fn keyring_set(user: &str, token: &SecretString) -> std::result::Result<(), keyring::Error> {
    keyring::Entry::new(KEYRING_SERVICE, user)?.set_password(token.expose_secret())
}

fn keyring_get(user: &str) -> std::result::Result<String, keyring::Error> {
    keyring::Entry::new(KEYRING_SERVICE, user)?.get_password()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyring_user_is_host_scoped() {
        assert_eq!(
            keyring_user("https://x.atlassian.net", Some("a@b.c")),
            "x.atlassian.net:a@b.c"
        );
        assert_eq!(
            keyring_user("https://jira.corp/jira", None),
            "jira.corp/jira"
        );
    }

    #[test]
    fn storage_kind_is_derived_from_what_was_persisted() {
        assert_eq!(
            StoredSecret {
                reference: Some("x".into()),
                ..Default::default()
            }
            .storage(),
            SecretStorage::Keyring
        );
        assert_eq!(
            StoredSecret {
                blob: Some(vec![1]),
                kdf: Some("{}".into()),
                ..Default::default()
            }
            .storage(),
            SecretStorage::EncryptedBlob
        );
        assert_eq!(
            StoredSecret {
                blob: Some(vec![1]),
                ..Default::default()
            }
            .storage(),
            SecretStorage::Plaintext
        );
    }

    #[test]
    fn plaintext_blob_round_trips() {
        let stored = StoredSecret {
            blob: Some(b"token-value".to_vec()),
            ..Default::default()
        };
        assert_eq!(load(&stored).unwrap().expose_secret(), "token-value");
    }
}
