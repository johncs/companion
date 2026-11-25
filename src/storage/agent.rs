//! Persistence for agent tool calls.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Row, params};

use super::Database;
use super::entities::ToolCallRecord;

impl Database {
    /// Persist a tool call record linked to `message_id`.
    pub fn insert_tool_call(&self, tc: &ToolCallRecord) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute(
            "INSERT INTO tool_calls (id, message_id, tool_name, arguments_json, status, output, permission_decision, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                tc.id,
                tc.message_id,
                tc.tool_name,
                tc.arguments_json,
                tc.status,
                tc.output,
                tc.permission_decision,
                tc.created_at,
            ],
        )?;
        Ok(())
    }

    /// Batch-fetch tool calls for many `message_ids` in one query (avoids N+1 queries).
    pub fn list_tool_calls_for_messages(
        &self,
        message_ids: &[&str],
    ) -> Result<HashMap<String, Vec<ToolCallRecord>>> {
        let mut map: HashMap<String, Vec<ToolCallRecord>> = HashMap::new();
        if message_ids.is_empty() {
            return Ok(map);
        }

        let placeholders = vec!["?"; message_ids.len()].join(",");
        let sql = format!(
            "SELECT id, message_id, tool_name, arguments_json, status, output, permission_decision, created_at
             FROM tool_calls
             WHERE message_id IN ({placeholders})
             ORDER BY created_at ASC"
        );

        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(message_ids.iter()), map_tool_call)?;
        for row in rows {
            let tc = row?;
            map.entry(tc.message_id.clone()).or_default().push(tc);
        }
        Ok(map)
    }
}

fn map_tool_call(row: &Row<'_>) -> rusqlite::Result<ToolCallRecord> {
    Ok(ToolCallRecord {
        id: row.get(0)?,
        message_id: row.get(1)?,
        tool_name: row.get(2)?,
        arguments_json: row.get(3)?,
        status: row.get(4)?,
        output: row.get(5)?,
        permission_decision: row.get(6)?,
        created_at: row.get(7)?,
    })
}
