//! Versioned SQLite schema migrations and initial seed data.

mod migrations;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, Transaction};

/// Current database schema version tracked via `PRAGMA user_version`.
pub const CURRENT_SCHEMA_VERSION: i32 = 17;

pub(crate) type MigrationFn = fn(&Transaction<'_>) -> Result<()>;

pub(crate) const MIGRATIONS: &[(i32, MigrationFn)] = &[
    (1, migrations::migrate_v1),
    (2, migrations::migrate_v2),
    (3, migrations::migrate_v3),
    (4, migrations::migrate_v4),
    (5, migrations::migrate_v5),
    (6, migrations::migrate_v6),
    (7, migrations::migrate_v7),
    (8, migrations::migrate_v8),
    (9, migrations::migrate_v9),
    (10, migrations::migrate_v10),
    (11, migrations::migrate_v11),
    (12, migrations::migrate_v12),
    (13, migrations::migrate_v13),
    (14, migrations::migrate_v14),
    (15, migrations::migrate_v15),
    (16, migrations::migrate_v16),
    (17, migrations::migrate_v17),
];

/// Apply any pending migrations to bring the database up to `CURRENT_SCHEMA_VERSION`.
pub fn run_migrations(conn: &mut Connection) -> Result<()> {
    run_migration_list(conn, MIGRATIONS)
}

/// Apply an ordered slice of `(version, migrate_fn)` atomically, updating `user_version` per step.
pub(crate) fn run_migration_list(
    conn: &mut Connection,
    migrations: &[(i32, MigrationFn)],
) -> Result<()> {
    let mut version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let max_supported = migrations.last().map(|&(v, _)| v).unwrap_or(0);
    if version > max_supported {
        bail!("Database schema version {version} is newer than supported version {max_supported}");
    }

    for &(target_version, migrate_fn) in migrations {
        if version < target_version {
            let tx = conn.transaction()?;
            migrate_fn(&tx)
                .with_context(|| format!("Database migration v{target_version} failed"))?;
            tx.pragma_update(None, "user_version", target_version)?;
            tx.commit()?;
            version = target_version;
        }
    }

    Ok(())
}
