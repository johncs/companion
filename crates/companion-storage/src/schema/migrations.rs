use anyhow::Result;
use rusqlite::{OptionalExtension, Transaction, params};

use crate::entities::PendingDownload;

const V1_TABLES_SQL: &str = "
    CREATE TABLE IF NOT EXISTS projects (
        id          TEXT PRIMARY KEY,
        name        TEXT NOT NULL,
        folder_path TEXT NOT NULL,
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS personas (
        id                  TEXT PRIMARY KEY,
        name                TEXT NOT NULL,
        system_prompt       TEXT NOT NULL,
        default_params_json TEXT NOT NULL DEFAULT '{}'
    );

    CREATE TABLE IF NOT EXISTS models (
        id                  TEXT PRIMARY KEY,
        display_name        TEXT NOT NULL,
        provider_type       TEXT NOT NULL,
        file_path           TEXT,
        mmproj_path         TEXT,
        hf_repo             TEXT,
        quantization        TEXT,
        supports_thinking   INTEGER NOT NULL DEFAULT 0,
        default_params_json TEXT NOT NULL DEFAULT '{}'
    );

    CREATE TABLE IF NOT EXISTS conversations (
        id          TEXT PRIMARY KEY,
        project_id  TEXT REFERENCES projects(id) ON DELETE CASCADE,
        title       TEXT NOT NULL,
        model_id    TEXT,
        persona_id  TEXT,
        pinned      INTEGER NOT NULL DEFAULT 0,
        created_at  INTEGER NOT NULL,
        updated_at  INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_conversations_project_updated
        ON conversations(project_id, updated_at DESC);

    CREATE TABLE IF NOT EXISTS messages (
        id                TEXT PRIMARY KEY,
        conversation_id   TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
        role              TEXT NOT NULL,
        content           TEXT NOT NULL,
        reasoning_content TEXT,
        thinking_level    TEXT,
        token_count       INTEGER NOT NULL DEFAULT 0,
        created_at        INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_messages_conv_created
        ON messages(conversation_id, created_at ASC);
";

const V1_FTS_SQL: &str = "
    CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
        content,
        conversation_id UNINDEXED,
        content='messages',
        content_rowid='rowid'
    );

    CREATE TRIGGER IF NOT EXISTS messages_ai AFTER INSERT ON messages BEGIN
        INSERT INTO messages_fts(rowid, content, conversation_id)
        VALUES (new.rowid, new.content, new.conversation_id);
    END;

    CREATE TRIGGER IF NOT EXISTS messages_ad AFTER DELETE ON messages BEGIN
        INSERT INTO messages_fts(messages_fts, rowid, content, conversation_id)
        VALUES ('delete', old.rowid, old.content, old.conversation_id);
    END;

    CREATE TRIGGER IF NOT EXISTS messages_au AFTER UPDATE ON messages BEGIN
        INSERT INTO messages_fts(messages_fts, rowid, content, conversation_id)
        VALUES ('delete', old.rowid, old.content, old.conversation_id);
        INSERT INTO messages_fts(rowid, content, conversation_id)
        VALUES (new.rowid, new.content, new.conversation_id);
    END;
";

pub(super) fn migrate_v1(conn: &Transaction<'_>) -> Result<()> {
    conn.execute_batch(V1_TABLES_SQL)?;
    conn.execute_batch(V1_FTS_SQL)?;
    seed_default_personas(conn)?;
    Ok(())
}

pub(super) fn migrate_v2(conn: &Transaction<'_>) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS attachments (
            id             TEXT PRIMARY KEY,
            message_id     TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
            file_name      TEXT NOT NULL,
            mime_type      TEXT NOT NULL,
            file_path      TEXT NOT NULL,
            extracted_text TEXT,
            byte_size      INTEGER NOT NULL DEFAULT 0
        );

        CREATE INDEX IF NOT EXISTS idx_attachments_message
            ON attachments(message_id);
        ",
    )?;
    Ok(())
}

pub(super) fn migrate_v3(conn: &Transaction<'_>) -> Result<()> {
    conn.execute_batch("ALTER TABLE conversations ADD COLUMN system_prompt TEXT;")?;
    Ok(())
}

pub(super) fn migrate_v4(conn: &Transaction<'_>) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS tool_calls (
            id                  TEXT PRIMARY KEY,
            message_id          TEXT NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
            tool_name           TEXT NOT NULL,
            arguments_json      TEXT NOT NULL,
            status              TEXT NOT NULL,
            output              TEXT,
            permission_decision TEXT NOT NULL DEFAULT 'ask',
            created_at          INTEGER NOT NULL DEFAULT 0
        );

        CREATE INDEX IF NOT EXISTS idx_tool_calls_message
            ON tool_calls(message_id, created_at ASC);

        CREATE TABLE IF NOT EXISTS project_permissions (
            project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            tool_name  TEXT NOT NULL,
            policy     TEXT NOT NULL,
            PRIMARY KEY (project_id, tool_name)
        );
        ",
    )?;
    Ok(())
}

pub(super) fn migrate_v5(conn: &Transaction<'_>) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE conversations ADD COLUMN thinking_level TEXT NOT NULL DEFAULT 'default';",
    )?;
    Ok(())
}

pub(super) fn migrate_v6(conn: &Transaction<'_>) -> Result<()> {
    conn.execute_batch("ALTER TABLE messages ADD COLUMN generation_ms INTEGER;")?;
    Ok(())
}

pub(super) fn migrate_v7(conn: &Transaction<'_>) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS app_settings (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        ",
    )?;
    Ok(())
}

pub(super) fn migrate_v8(conn: &Transaction<'_>) -> Result<()> {
    conn.execute_batch("ALTER TABLE messages ADD COLUMN model_name TEXT;")?;
    Ok(())
}

pub(super) fn migrate_v9(conn: &Transaction<'_>) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE models ADD COLUMN avg_tokens_per_sec REAL;
         ALTER TABLE models ADD COLUMN speed_sample_count INTEGER NOT NULL DEFAULT 0;",
    )?;
    Ok(())
}

pub(super) fn migrate_v10(conn: &Transaction<'_>) -> Result<()> {
    conn.execute_batch("ALTER TABLE messages ADD COLUMN execution_device TEXT;")?;
    Ok(())
}

pub(super) fn migrate_v11(conn: &Transaction<'_>) -> Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS conversation_folders (
            id              TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
            folder_path     TEXT NOT NULL,
            created_at      INTEGER NOT NULL,
            UNIQUE(conversation_id, folder_path)
        );
        CREATE INDEX IF NOT EXISTS idx_conversation_folders_conv
            ON conversation_folders(conversation_id, created_at ASC);
        ",
    )?;
    Ok(())
}

pub(super) fn migrate_v12(conn: &Transaction<'_>) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE conversation_folders ADD COLUMN permission_mode TEXT NOT NULL DEFAULT 'read';",
    )?;
    Ok(())
}

pub(super) fn migrate_v13(conn: &Transaction<'_>) -> Result<()> {
    conn.execute_batch(
        "
        INSERT OR IGNORE INTO conversation_folders (id, conversation_id, folder_path, permission_mode, created_at)
        SELECT lower(hex(randomblob(16))), c.id, p.folder_path, 'read_write', CAST(strftime('%s','now') AS INTEGER) * 1000
        FROM conversations c
        JOIN projects p ON p.id = c.project_id
        WHERE trim(p.folder_path) != '';

        UPDATE projects SET folder_path = '';
        ",
    )?;
    Ok(())
}

pub(super) fn migrate_v14(conn: &Transaction<'_>) -> Result<()> {
    // Flatten any legacy grouped conversations into the unified chat list.
    conn.execute(
        "UPDATE conversations SET project_id = NULL WHERE project_id IS NOT NULL",
        [],
    )?;
    Ok(())
}

pub(super) fn migrate_v15(conn: &Transaction<'_>) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE conversations ADD COLUMN web_search_enabled INTEGER NOT NULL DEFAULT 0;",
    )?;
    Ok(())
}

pub(super) fn migrate_v16(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS pending_downloads (
            repo_id         TEXT NOT NULL,
            filename        TEXT NOT NULL,
            mmproj_filename TEXT,
            display_name    TEXT NOT NULL,
            total_bytes     INTEGER,
            created_at      INTEGER NOT NULL,
            PRIMARY KEY (repo_id, filename)
        );
        ",
    )?;

    let legacy_json: Option<String> = tx
        .query_row(
            "SELECT value FROM app_settings WHERE key = 'pending_downloads'",
            [],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(raw) = legacy_json {
        if let Ok(items) = serde_json::from_str::<Vec<PendingDownload>>(&raw) {
            let now = chrono::Utc::now().timestamp_millis();
            for item in items {
                tx.execute(
                    "INSERT OR IGNORE INTO pending_downloads (repo_id, filename, mmproj_filename, display_name, total_bytes, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        item.repo_id,
                        item.filename,
                        item.mmproj_filename,
                        item.display_name,
                        item.total_bytes.map(|b| b as i64),
                        now,
                    ],
                )?;
            }
        }
        tx.execute(
            "DELETE FROM app_settings WHERE key = 'pending_downloads'",
            [],
        )?;
    }

    Ok(())
}

fn seed_default_personas(conn: &Transaction<'_>) -> Result<()> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM personas", [], |row| row.get(0))?;
    if count > 0 {
        return Ok(());
    }

    conn.execute(
        "INSERT INTO personas (id, name, system_prompt) VALUES (?1, ?2, ?3)",
        params![
            "default-assistant",
            "General Assistant",
            "You are Companion, a helpful, concise, and accurate AI assistant."
        ],
    )?;
    conn.execute(
        "INSERT INTO personas (id, name, system_prompt) VALUES (?1, ?2, ?3)",
        params![
            "project-engineer",
            "Project Collaborator",
            "You are Companion, an AI collaborator working with the user on their project folder. Be precise, explain changes clearly, and verify details."
        ],
    )?;
    Ok(())
}
