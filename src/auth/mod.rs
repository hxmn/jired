//! Credential storage [M1.5, D11].
//!
//! Two independent questions, kept apart on purpose:
//!
//! **Where** the credential lives — [`CredStore`], chosen with `creds.storage`:
//! the OS keychain, `.jired/session.db` in this workspace, or
//! `~/.config/jired/credentials.toml` shared by every workspace on this machine.
//! Unset means "keychain if this machine has one, otherwise the workspace".
//!
//! **What protects it** — [`SecretStorage`]: the keychain protects its own
//! entries; the other two backends store a blob encrypted with a scrypt-derived
//! key from `JIRED_SECRET_KEY`, or — only with `--insecure-store` — the raw
//! token. Encryption protects a backup, a snapshot or a stray `.jired/`; it does
//! *not* protect against an attacker who can read the environment or run code as
//! this user. That trade-off is documented rather than hidden.

pub mod crypt;
pub mod user_store;

use crate::error::{JiredError, Result};
use secrecy::{ExposeSecret, SecretString};

pub const KEYRING_SERVICE: &str = "jired";
pub const SECRET_KEY_ENV: &str = "JIRED_SECRET_KEY";
/// Set by CI and tests, which must never write into a developer's real keychain.
pub const NO_KEYRING_ENV: &str = "JIRED_NO_KEYRING";

fn keyring_disabled() -> bool {
    std::env::var(NO_KEYRING_ENV).is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Where a credential is kept — the `creds.storage` setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredStore {
    /// The operating system's keychain; `session.db` holds only a reference.
    Keychain,
    /// `.jired/session.db`, so the credential belongs to this workspace alone.
    LocalDb,
    /// `~/.config/jired/credentials.toml`, shared by every workspace here.
    UserConfig,
}

impl CredStore {
    pub fn as_str(self) -> &'static str {
        match self {
            CredStore::Keychain => "keychain",
            CredStore::LocalDb => "local_db",
            CredStore::UserConfig => "user_config",
        }
    }

    /// Where this puts the secret, in words a person can act on.
    pub fn describe(self) -> &'static str {
        match self {
            CredStore::Keychain => "the OS keychain",
            CredStore::LocalDb => ".jired/session.db in this workspace",
            CredStore::UserConfig => "credentials.toml in your user config directory",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s
            .trim()
            .to_ascii_lowercase()
            .replace(['-', ' '], "_")
            .as_str()
        {
            "keychain" | "keyring" | "os" | "os_keychain" => Ok(CredStore::Keychain),
            "local_db" | "localdb" | "local" | "workspace" | "session_db" => Ok(CredStore::LocalDb),
            "user_config" | "userconfig" | "user" | "global" | "config" => {
                Ok(CredStore::UserConfig)
            }
            other => Err(JiredError::usage(format!(
                "unknown creds.storage `{other}` (expected keychain, local_db, or user_config)"
            ))
            .with_detail("key", "creds.storage")),
        }
    }
}

/// What protects a stored credential, as reported by `whoami` and `init`.
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
/// unless storage is [`SecretStorage::Plaintext`] and the store is
/// [`CredStore::LocalDb`].
#[derive(Debug, Clone)]
pub struct StoredSecret {
    pub store: CredStore,
    /// Entry name in the keychain or the user credential file.
    pub reference: Option<String>,
    /// The sealed bytes themselves — only for [`CredStore::LocalDb`].
    pub blob: Option<Vec<u8>>,
    /// Encryption parameters; absent means the bytes are the raw token.
    pub kdf: Option<String>,
}

impl Default for StoredSecret {
    fn default() -> Self {
        Self {
            store: CredStore::LocalDb,
            reference: None,
            blob: None,
            kdf: None,
        }
    }
}

impl StoredSecret {
    pub fn storage(&self) -> SecretStorage {
        match self.store {
            CredStore::Keychain => SecretStorage::Keyring,
            _ if self.kdf.is_some() => SecretStorage::EncryptedBlob,
            _ => SecretStorage::Plaintext,
        }
    }

    pub fn location(&self) -> CredStore {
        self.store
    }

    /// Reconstruct from a `session.db` row, tolerating rows written before the
    /// location was recorded: a reference meant the keychain, a blob meant here.
    pub fn from_row(
        store: Option<&str>,
        reference: Option<String>,
        blob: Option<Vec<u8>>,
        kdf: Option<String>,
    ) -> Result<Self> {
        let store = match store {
            Some(s) => CredStore::parse(s)?,
            None if reference.is_some() => CredStore::Keychain,
            None => CredStore::LocalDb,
        };
        Ok(Self {
            store,
            reference,
            blob,
            kdf,
        })
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

/// Persist a token where `want` says, or in the best place available.
///
/// An explicit `want` is honoured or fails: silently writing the credential
/// somewhere other than where the user asked would make `creds.storage` a lie.
pub fn store(
    entry_user: &str,
    token: &SecretString,
    allow_plaintext: bool,
    want: Option<CredStore>,
) -> Result<StoredSecret> {
    match want {
        Some(CredStore::Keychain) => keychain(entry_user, token),
        Some(CredStore::LocalDb) => {
            let (blob, kdf) = seal(token, allow_plaintext, CredStore::LocalDb)?;
            Ok(StoredSecret {
                store: CredStore::LocalDb,
                reference: None,
                blob: Some(blob),
                kdf,
            })
        }
        Some(CredStore::UserConfig) => {
            let (blob, kdf) = seal(token, allow_plaintext, CredStore::UserConfig)?;
            let path = user_store::save(entry_user, &blob, kdf.as_deref())?;
            tracing::debug!("stored credential in {}", path.display());
            Ok(StoredSecret {
                store: CredStore::UserConfig,
                reference: Some(entry_user.to_string()),
                blob: None,
                kdf,
            })
        }
        // Unset: prefer the keychain, fall back to this workspace.
        None => match keychain(entry_user, token) {
            Ok(stored) => Ok(stored),
            Err(e) => {
                tracing::debug!("keychain unavailable: {}", e.message);
                store(entry_user, token, allow_plaintext, Some(CredStore::LocalDb))
            }
        },
    }
}

fn keychain(entry_user: &str, token: &SecretString) -> Result<StoredSecret> {
    if keyring_disabled() {
        return Err(
            JiredError::auth(format!("the OS keychain is disabled by {NO_KEYRING_ENV}"))
                .with_hint("set creds.storage to local_db or user_config"),
        );
    }
    match keyring_set(entry_user, token) {
        Ok(()) => {
            tracing::debug!("stored credential in the OS keychain");
            Ok(StoredSecret {
                store: CredStore::Keychain,
                reference: Some(entry_user.to_string()),
                blob: None,
                kdf: None,
            })
        }
        Err(e) => Err(
            JiredError::auth(format!("no OS keychain is available here: {e}")).with_hint(
                "set creds.storage to local_db (this workspace) or user_config \
                 (shared by every workspace)",
            ),
        ),
    }
}

/// Encrypt the token for a file-backed store, or refuse to write it in the clear.
fn seal(
    token: &SecretString,
    allow_plaintext: bool,
    into: CredStore,
) -> Result<(Vec<u8>, Option<String>)> {
    match std::env::var(SECRET_KEY_ENV) {
        Ok(key) if !key.is_empty() => {
            let (blob, kdf) = crypt::seal(token.expose_secret().as_bytes(), &key)?;
            tracing::debug!("sealed credential for {}", into.as_str());
            Ok((blob, Some(kdf)))
        }
        _ if allow_plaintext => {
            eprintln!(
                "warning: storing the credential UNENCRYPTED in {} \
                 (--insecure-store). Anyone who can read it gets your token.",
                into.describe()
            );
            Ok((token.expose_secret().as_bytes().to_vec(), None))
        }
        _ => Err(JiredError::auth(format!(
            "storing the credential in {} needs an encryption key",
            into.describe()
        ))
        .with_detail("env", SECRET_KEY_ENV)
        .with_detail("creds.storage", into.as_str())
        .with_hint(format!(
            "set {SECRET_KEY_ENV} to a passphrase, or pass --insecure-store to \
             accept unencrypted storage"
        ))),
    }
}

/// Look for a credential this machine already holds for `entry_user`.
///
/// A keychain entry and the user credential file are both account-scoped rather
/// than workspace-scoped, so a second workspace pointing at the same instance
/// and identity does not need to be told the token again. `LocalDb` is
/// deliberately absent: it belongs to one workspace, and the account in *this*
/// workspace is read from `session.db` long before this is reached.
pub fn existing(entry_user: &str, want: Option<CredStore>) -> Option<SecretString> {
    let candidates: &[CredStore] = match want {
        Some(CredStore::Keychain) => &[CredStore::Keychain],
        Some(CredStore::UserConfig) => &[CredStore::UserConfig],
        Some(CredStore::LocalDb) => &[],
        None => &[CredStore::Keychain, CredStore::UserConfig],
    };
    for store in candidates {
        let stored = StoredSecret {
            store: *store,
            reference: Some(entry_user.to_string()),
            blob: None,
            kdf: None,
        };
        // Only a credential that can actually be read back counts: an entry we
        // cannot decrypt is worse than no entry, because it would be reported
        // as reuse and then fail at the first request.
        if *store == CredStore::UserConfig
            && matches!(user_store::load(entry_user), Ok(None) | Err(_))
        {
            continue;
        }
        if let Ok(token) = load(&stored) {
            return Some(token);
        }
    }
    None
}

/// Recover a token previously persisted with [`store`].
pub fn load(stored: &StoredSecret) -> Result<SecretString> {
    if stored.store == CredStore::Keychain {
        let reference = stored
            .reference
            .as_ref()
            .ok_or_else(|| JiredError::auth("no keychain entry recorded for this account"))?;
        let value = keyring_get(reference).map_err(|e| {
            JiredError::auth(format!("cannot read the credential from the keychain: {e}"))
                .with_hint("run `jired init --force` to re-authenticate")
        })?;
        return Ok(SecretString::from(value));
    }

    // A user-config credential lives in the file; the row only points at it.
    let from_file = match (stored.store, &stored.reference) {
        (CredStore::UserConfig, Some(name)) => user_store::load(name)?.ok_or_else(|| {
            JiredError::auth(format!(
                "no credential for {name} in the user credential file"
            ))
            .with_hint("run `jired init --force` to store it again")
        })?,
        _ => user_store::Entry {
            blob: stored
                .blob
                .clone()
                .ok_or_else(|| JiredError::auth("no credential stored for this account"))?,
            kdf: stored.kdf.clone(),
        },
    };
    let blob = &from_file.blob;

    match &from_file.kdf {
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
    let Some(reference) = &stored.reference else {
        return;
    };
    match stored.store {
        CredStore::Keychain => {
            if let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, reference) {
                let _ = entry.delete_credential();
            }
        }
        CredStore::UserConfig => user_store::remove(reference),
        CredStore::LocalDb => {}
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
    fn storage_kind_is_derived_from_where_it_went_and_how_it_was_sealed() {
        let keychain = StoredSecret {
            store: CredStore::Keychain,
            reference: Some("x".into()),
            ..Default::default()
        };
        assert_eq!(keychain.storage(), SecretStorage::Keyring);

        for store in [CredStore::LocalDb, CredStore::UserConfig] {
            assert_eq!(
                StoredSecret {
                    store,
                    kdf: Some("{}".into()),
                    blob: Some(vec![1]),
                    ..Default::default()
                }
                .storage(),
                SecretStorage::EncryptedBlob,
                "{store:?}"
            );
            assert_eq!(
                StoredSecret {
                    store,
                    blob: Some(vec![1]),
                    ..Default::default()
                }
                .storage(),
                SecretStorage::Plaintext,
                "{store:?}"
            );
        }
    }

    /// Rows written before `secret_store` existed still have to resolve.
    #[test]
    fn a_row_without_a_recorded_location_is_read_the_way_it_was_written() {
        let legacy_keychain =
            StoredSecret::from_row(None, Some("host:me".into()), None, None).unwrap();
        assert_eq!(legacy_keychain.location(), CredStore::Keychain);

        let legacy_blob =
            StoredSecret::from_row(None, None, Some(vec![1]), Some("{}".into())).unwrap();
        assert_eq!(legacy_blob.location(), CredStore::LocalDb);

        let recorded =
            StoredSecret::from_row(Some("user_config"), Some("host:me".into()), None, None)
                .unwrap();
        assert_eq!(recorded.location(), CredStore::UserConfig);
    }

    #[test]
    fn storage_names_round_trip_through_their_spellings() {
        for (input, expected) in [
            ("keychain", CredStore::Keychain),
            ("keyring", CredStore::Keychain),
            ("local_db", CredStore::LocalDb),
            ("local-db", CredStore::LocalDb),
            ("workspace", CredStore::LocalDb),
            ("user_config", CredStore::UserConfig),
            ("USER-CONFIG", CredStore::UserConfig),
        ] {
            assert_eq!(CredStore::parse(input).unwrap(), expected, "{input}");
        }
        for store in [
            CredStore::Keychain,
            CredStore::LocalDb,
            CredStore::UserConfig,
        ] {
            assert_eq!(CredStore::parse(store.as_str()).unwrap(), store);
            assert!(!store.describe().is_empty());
        }
        assert_eq!(
            CredStore::parse("mailbox")
                .unwrap_err()
                .exit_code()
                .as_i32(),
            2
        );
    }

    #[test]
    fn plaintext_blob_round_trips() {
        let stored = StoredSecret {
            store: CredStore::LocalDb,
            blob: Some(b"token-value".to_vec()),
            ..Default::default()
        };
        assert_eq!(load(&stored).unwrap().expose_secret(), "token-value");
    }
}
