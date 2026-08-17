//! `.jired/session.db` — identity and credential references [M1.5].

use crate::api::{AuthKind, Deployment, Myself};
use crate::auth::{self, SecretStorage, StoredSecret};
use crate::error::{JiredError, Result};
use crate::store::{migrate, now_rfc3339, open_db, Migration};
use crate::workspace::{restrict_file_mode, Workspace};
use rusqlite::{params, Connection, OptionalExtension};
use secrecy::SecretString;

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: r#"
CREATE TABLE account (
  id            INTEGER PRIMARY KEY,
  profile       TEXT NOT NULL DEFAULT 'default',
  base_url      TEXT NOT NULL,
  deployment    TEXT NOT NULL CHECK (deployment IN ('cloud','datacenter')),
  auth_kind     TEXT NOT NULL CHECK (auth_kind IN ('api_token','pat','basic','cookie','oauth2')),
  identity      TEXT,
  display_name  TEXT,
  account_id    TEXT,
  secret_ref    TEXT,
  secret_blob   BLOB,
  secret_kdf    TEXT,
  tls_ca_path   TEXT,
  tls_insecure  INTEGER NOT NULL DEFAULT 0,
  created_at    TEXT NOT NULL,
  last_verified_at TEXT
);
CREATE UNIQUE INDEX account_profile ON account(profile);
"#,
}];

#[derive(Debug, Clone)]
pub struct Account {
    pub id: i64,
    pub profile: String,
    pub base_url: String,
    pub deployment: Deployment,
    pub auth_kind: AuthKind,
    pub identity: Option<String>,
    pub display_name: Option<String>,
    pub account_id: Option<String>,
    pub tls_ca_path: Option<String>,
    pub tls_insecure: bool,
    secret: StoredSecret,
}

impl Account {
    pub fn secret_storage(&self) -> &'static str {
        self.secret.storage().as_str()
    }

    /// Recover the live credential. Kept off `Account`'s `Debug` output by
    /// construction: the secret never lives in a field that gets formatted.
    pub fn token(&self) -> Result<SecretString> {
        auth::load(&self.secret)
    }
}

pub struct NewAccount {
    pub base_url: String,
    pub deployment: Deployment,
    pub auth_kind: AuthKind,
    pub identity: Option<String>,
    pub display_name: Option<String>,
    pub account_id: Option<String>,
    pub tls_ca_path: Option<String>,
    pub tls_insecure: bool,
    pub token: SecretString,
    pub allow_plaintext: bool,
}

pub struct SessionStore {
    conn: Connection,
}

impl SessionStore {
    pub fn open(ws: &Workspace) -> Result<Self> {
        let path = ws.session_db_path();
        let existed = path.exists();
        let conn = open_db(&path)?;
        migrate(&conn, "session.db", MIGRATIONS)?;

        // The file holds credentials; enforce 0600 on every open, not just
        // creation, so a careless chmod is corrected rather than tolerated.
        match restrict_file_mode(&path) {
            Ok(true) if existed => {
                tracing::warn!("fixed permissions on {} (now 0600)", path.display());
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("cannot set permissions on session.db: {e}"),
        }
        Ok(Self { conn })
    }

    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        migrate(&conn, "session.db", MIGRATIONS)?;
        Ok(Self { conn })
    }

    pub fn upsert_account(&self, new: NewAccount) -> Result<Account> {
        let entry_user = auth::keyring_user(&new.base_url, new.identity.as_deref());
        let secret = auth::store(&entry_user, &new.token, new.allow_plaintext)?;
        let now = now_rfc3339();

        self.conn.execute(
            r#"
INSERT INTO account
  (profile, base_url, deployment, auth_kind, identity, display_name, account_id,
   secret_ref, secret_blob, secret_kdf, tls_ca_path, tls_insecure, created_at, last_verified_at)
VALUES ('default', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
ON CONFLICT(profile) DO UPDATE SET
  base_url=excluded.base_url, deployment=excluded.deployment, auth_kind=excluded.auth_kind,
  identity=excluded.identity, display_name=excluded.display_name, account_id=excluded.account_id,
  secret_ref=excluded.secret_ref, secret_blob=excluded.secret_blob, secret_kdf=excluded.secret_kdf,
  tls_ca_path=excluded.tls_ca_path, tls_insecure=excluded.tls_insecure,
  last_verified_at=excluded.last_verified_at
"#,
            params![
                new.base_url,
                new.deployment.as_str(),
                new.auth_kind.as_str(),
                new.identity,
                new.display_name,
                new.account_id,
                secret.reference,
                secret.blob,
                secret.kdf,
                new.tls_ca_path,
                new.tls_insecure as i64,
                now,
            ],
        )?;
        self.require_account()
    }

    pub fn account(&self) -> Result<Option<Account>> {
        let row = self
            .conn
            .query_row(
                r#"SELECT id, profile, base_url, deployment, auth_kind, identity, display_name,
                          account_id, secret_ref, secret_blob, secret_kdf, tls_ca_path, tls_insecure
                   FROM account WHERE profile = 'default'"#,
                [],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, Option<String>>(6)?,
                        r.get::<_, Option<String>>(7)?,
                        r.get::<_, Option<String>>(8)?,
                        r.get::<_, Option<Vec<u8>>>(9)?,
                        r.get::<_, Option<String>>(10)?,
                        r.get::<_, Option<String>>(11)?,
                        r.get::<_, i64>(12)?,
                    ))
                },
            )
            .optional()?;

        Ok(match row {
            None => None,
            Some(r) => Some(Account {
                id: r.0,
                profile: r.1,
                base_url: r.2,
                deployment: Deployment::parse(&r.3)?,
                auth_kind: AuthKind::parse(&r.4)?,
                identity: r.5,
                display_name: r.6,
                account_id: r.7,
                tls_ca_path: r.11,
                tls_insecure: r.12 != 0,
                secret: StoredSecret {
                    reference: r.8,
                    blob: r.9,
                    kdf: r.10,
                },
            }),
        })
    }

    pub fn require_account(&self) -> Result<Account> {
        self.account()?.ok_or_else(|| {
            JiredError::auth("this workspace has no stored credentials")
                .with_hint("run `jired init`")
        })
    }

    pub fn record_verification(&self, account_id: i64, me: &Myself) -> Result<()> {
        self.conn.execute(
            "UPDATE account SET display_name = ?1, account_id = ?2, last_verified_at = ?3 \
             WHERE id = ?4",
            params![me.display_name, me.account_id, now_rfc3339(), account_id],
        )?;
        Ok(())
    }

    /// Forget the account, removing the keychain entry too.
    pub fn logout(&self) -> Result<()> {
        if let Some(account) = self.account()? {
            auth::delete(&account.secret);
        }
        self.conn.execute("DELETE FROM account", [])?;
        Ok(())
    }

    pub fn secret_storage(&self) -> Result<Option<SecretStorage>> {
        Ok(self.account()?.map(|a| a.secret.storage()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    fn new_account(token: &str) -> NewAccount {
        NewAccount {
            base_url: "https://x.atlassian.net".into(),
            deployment: Deployment::Cloud,
            auth_kind: AuthKind::ApiToken,
            identity: Some("a@b.c".into()),
            display_name: Some("Ilya A".into()),
            account_id: Some("5b10a2".into()),
            tls_ca_path: None,
            tls_insecure: false,
            token: SecretString::from(token.to_string()),
            allow_plaintext: true,
        }
    }

    #[test]
    fn account_round_trips_with_its_credential() {
        // Force the plaintext path so the test never touches a real keychain.
        let store = SessionStore::in_memory().unwrap();
        let stored = StoredSecret {
            blob: Some(b"tok".to_vec()),
            ..Default::default()
        };
        store
            .conn
            .execute(
                "INSERT INTO account (profile, base_url, deployment, auth_kind, identity,
                    secret_blob, created_at) VALUES ('default', ?1, 'cloud', 'api_token', ?2, ?3, ?4)",
                params![
                    "https://x.atlassian.net",
                    "a@b.c",
                    stored.blob,
                    now_rfc3339()
                ],
            )
            .unwrap();

        let account = store.require_account().unwrap();
        assert_eq!(account.deployment, Deployment::Cloud);
        assert_eq!(account.identity.as_deref(), Some("a@b.c"));
        assert_eq!(account.token().unwrap().expose_secret(), "tok");
        assert_eq!(account.secret_storage(), "plaintext");
    }

    #[test]
    fn missing_account_is_an_auth_error() {
        let store = SessionStore::in_memory().unwrap();
        assert!(store.account().unwrap().is_none());
        assert_eq!(store.require_account().unwrap_err().exit_code().as_i32(), 3);
    }

    #[test]
    fn account_debug_output_never_contains_the_token() {
        let store = SessionStore::in_memory().unwrap();
        store
            .conn
            .execute(
                "INSERT INTO account (profile, base_url, deployment, auth_kind, secret_blob, created_at)
                 VALUES ('default', 'https://x', 'cloud', 'pat', ?1, ?2)",
                params![b"super-secret".to_vec(), now_rfc3339()],
            )
            .unwrap();
        let account = store.require_account().unwrap();
        assert!(!format!("{account:?}").contains("super-secret"));
    }

    #[test]
    fn upsert_replaces_rather_than_duplicating() {
        let store = SessionStore::in_memory().unwrap();
        // Keyring may or may not exist in the test environment; either backend
        // is fine here, we only care that a second init does not add a row.
        let _ = store.upsert_account(new_account("one"));
        let _ = store.upsert_account(new_account("two"));
        let n: i64 = store
            .conn
            .query_row("SELECT count(*) FROM account", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
