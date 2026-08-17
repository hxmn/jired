//! SQLite storage [M1.5, M1.7].
//!
//! Two databases with deliberately different lifecycles (decision D17):
//! [`session`] holds identity and credentials and is precious; [`state`] is a
//! cache plus the push journal, and deleting it must always be recoverable by
//! re-fetching.

pub mod session;
pub mod state;

use crate::error::{JiredError, Result};
use rusqlite::Connection;
use std::path::Path;

/// One numbered schema step. Applied in order, inside a transaction.
pub struct Migration {
    pub version: i64,
    pub sql: &'static str,
}

/// Bring `conn` up to the latest known schema version.
///
/// A database written by a newer jired is refused rather than downgraded: a
/// silent downgrade would corrupt state the newer version depends on.
pub fn migrate(conn: &Connection, name: &str, migrations: &[Migration]) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let latest = migrations.last().map(|m| m.version).unwrap_or(0);

    if current > latest {
        return Err(JiredError::corrupt(format!(
            "{name} was written by a newer version of jired \
             (schema {current}, this build understands {latest})"
        ))
        .with_detail("database", name)
        .with_detail("found_version", current)
        .with_detail("supported_version", latest)
        .with_hint("upgrade jired"));
    }

    for m in migrations.iter().filter(|m| m.version > current) {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(m.sql).map_err(|e| {
            JiredError::corrupt(format!("migrating {name} to schema {}: {e}", m.version))
        })?;
        // PRAGMA cannot be parameterized.
        tx.pragma_update(None, "user_version", m.version)?;
        tx.commit()?;
        tracing::debug!(database = name, version = m.version, "applied migration");
    }
    Ok(())
}

/// Open a database with the pragmas every jired store wants.
pub fn open_db(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)
        .map_err(|e| JiredError::corrupt(format!("cannot open {}: {e}", path.display())))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIGRATIONS: &[Migration] = &[
        Migration {
            version: 1,
            sql: "CREATE TABLE a (x INTEGER);",
        },
        Migration {
            version: 2,
            sql: "ALTER TABLE a ADD COLUMN y TEXT;",
        },
    ];

    #[test]
    fn migrations_apply_once_and_are_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn, "test", MIGRATIONS).unwrap();
        migrate(&conn, "test", MIGRATIONS).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 2);
        conn.execute("INSERT INTO a (x, y) VALUES (1, 'ok')", [])
            .unwrap();
    }

    #[test]
    fn partial_upgrade_applies_only_new_steps() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn, "test", &MIGRATIONS[..1]).unwrap();
        migrate(&conn, "test", MIGRATIONS).unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 2);
    }

    #[test]
    fn newer_schema_is_refused_with_exit_9() {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", 99i64).unwrap();
        let err = migrate(&conn, "state.db", MIGRATIONS).unwrap_err();
        assert_eq!(err.exit_code().as_i32(), 9);
        assert_eq!(err.details["found_version"], 99);
    }

    #[test]
    fn a_failing_migration_leaves_the_version_untouched() {
        let conn = Connection::open_in_memory().unwrap();
        let broken: &[Migration] = &[Migration {
            version: 1,
            sql: "CREATE TABLE ok (x INTEGER); THIS IS NOT SQL;",
        }];
        assert!(migrate(&conn, "test", broken).is_err());
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 0);
    }
}
