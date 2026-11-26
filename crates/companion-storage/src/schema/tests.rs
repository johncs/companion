use rusqlite::{Connection, Transaction, params};

use super::*;

#[test]
fn fresh_database_migrates_to_current_version() {
    let mut conn = Connection::open_in_memory().unwrap();
    run_migrations(&mut conn).unwrap();

    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);

    let table_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'pending_downloads'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(table_exists, 1);
}

#[test]
fn upgrades_v15_with_legacy_pending_downloads_json_to_v16() {
    let mut conn = Connection::open_in_memory().unwrap();
    run_migration_list(&mut conn, &MIGRATIONS[..15]).unwrap();

    let legacy_json = serde_json::json!([
        {
            "repo_id": "org/repo",
            "filename": "model.gguf",
            "mmproj_filename": "mmproj.gguf",
            "display_name": "Test Model",
            "total_bytes": 123456
        }
    ])
    .to_string();
    conn.execute(
        "INSERT INTO app_settings (key, value) VALUES ('pending_downloads', ?1)",
        params![legacy_json],
    )
    .unwrap();

    run_migrations(&mut conn).unwrap();

    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);

    let legacy_left: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM app_settings WHERE key = 'pending_downloads'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(legacy_left, 0);

    let (repo, file, mmproj, total): (String, String, Option<String>, Option<i64>) = conn
        .query_row(
            "SELECT repo_id, filename, mmproj_filename, total_bytes FROM pending_downloads",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(repo, "org/repo");
    assert_eq!(file, "model.gguf");
    assert_eq!(mmproj.as_deref(), Some("mmproj.gguf"));
    assert_eq!(total, Some(123456));
}

#[test]
fn failed_migration_rolls_back_atomically() {
    fn ok_v1(tx: &Transaction<'_>) -> Result<()> {
        tx.execute_batch("CREATE TABLE test_t1 (id INTEGER PRIMARY KEY);")?;
        Ok(())
    }
    fn failing_v2(tx: &Transaction<'_>) -> Result<()> {
        tx.execute_batch("CREATE TABLE test_t2 (id INTEGER PRIMARY KEY);")?;
        anyhow::bail!("simulated migration failure")
    }

    let mut conn = Connection::open_in_memory().unwrap();
    let err = run_migration_list(&mut conn, &[(1, ok_v1), (2, failing_v2)]).unwrap_err();
    assert!(format!("{err:#}").contains("simulated migration failure"));

    let version: i32 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);

    let t2_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'test_t2'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(t2_exists, 0);
}

#[test]
fn rejects_future_schema_version() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION + 1)
        .unwrap();
    let err = run_migrations(&mut conn).unwrap_err();
    assert!(err.to_string().contains("newer than supported version"));
}
