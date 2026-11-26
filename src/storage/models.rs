//! Pure SQL CRUD persistence for registered AI models.

use anyhow::Result;
use rusqlite::{OptionalExtension, Row, params};

use crate::core::model_params::ModelParams;

use super::Database;
use super::entities::ModelRecord;

impl Database {
    /// Return all registered models ordered by display name.
    pub fn list_models(&self) -> Result<Vec<ModelRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, display_name, provider_type, file_path, mmproj_path, hf_repo, quantization, supports_thinking, default_params_json, avg_tokens_per_sec
                 FROM models
                 ORDER BY display_name ASC",
            )?;
            let rows = stmt.query_map([], map_model)?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        })
    }

    /// Fetch a single model record by ID.
    pub fn get_model(&self, id: &str) -> Result<Option<ModelRecord>> {
        self.with_conn(|conn| {
            conn.query_row(
                "SELECT id, display_name, provider_type, file_path, mmproj_path, hf_repo, quantization, supports_thinking, default_params_json, avg_tokens_per_sec
                 FROM models WHERE id = ?1",
                params![id],
                map_model,
            )
            .optional()
            .map_err(Into::into)
        })
    }

    /// Insert or update a model record.
    pub fn upsert_model(&self, model: &ModelRecord) -> Result<()> {
        let default_params_json = model.default_params.to_json()?;
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO models (id, display_name, provider_type, file_path, mmproj_path, hf_repo, quantization, supports_thinking, default_params_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET
                    display_name = excluded.display_name,
                    provider_type = excluded.provider_type,
                    file_path = excluded.file_path,
                    mmproj_path = excluded.mmproj_path,
                    hf_repo = excluded.hf_repo,
                    quantization = excluded.quantization,
                    supports_thinking = excluded.supports_thinking,
                    default_params_json = excluded.default_params_json",
                params![
                    model.id,
                    model.display_name,
                    model.provider_type,
                    model.file_path,
                    model.mmproj_path,
                    model.hf_repo,
                    model.quantization,
                    model.supports_thinking as i64,
                    default_params_json,
                ],
            )?;
            Ok(())
        })
    }

    /// Delete a model record from SQLite.
    pub fn delete_model(&self, id: &str) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute("DELETE FROM models WHERE id = ?1", params![id])?;
            Ok(())
        })
    }

    /// Record a measured token-generation speed (`tokens_per_sec`) and update the model's
    /// capped running average (window up to 20 samples).
    pub fn record_model_speed(&self, id: &str, tokens_per_sec: f64) -> Result<()> {
        if !tokens_per_sec.is_finite() || tokens_per_sec <= 0.0 {
            return Ok(());
        }
        self.with_conn(|conn| {
            let existing: Option<(Option<f64>, i64)> = conn
                .query_row(
                    "SELECT avg_tokens_per_sec, speed_sample_count FROM models WHERE id = ?1",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;

            let Some((prev_avg, prev_count)) = existing else {
                return Ok(());
            };
            let new_count = prev_count.saturating_add(1);
            let weight = new_count.min(20) as f64;
            let new_avg = match prev_avg {
                Some(avg) if prev_count > 0 => ((avg * (weight - 1.0)) + tokens_per_sec) / weight,
                _ => tokens_per_sec,
            };
            conn.execute(
                "UPDATE models SET avg_tokens_per_sec = ?1, speed_sample_count = ?2 WHERE id = ?3",
                params![new_avg, new_count, id],
            )?;
            Ok(())
        })
    }
}

fn map_model(row: &Row<'_>) -> rusqlite::Result<ModelRecord> {
    let supports_thinking: i64 = row.get(7)?;
    let raw_params: String = row.get(8)?;
    Ok(ModelRecord {
        id: row.get(0)?,
        display_name: row.get(1)?,
        provider_type: row.get(2)?,
        file_path: row.get(3)?,
        mmproj_path: row.get(4)?,
        hf_repo: row.get(5)?,
        quantization: row.get(6)?,
        supports_thinking: supports_thinking != 0,
        default_params: ModelParams::from_json_lossy(&raw_params),
        avg_tokens_per_sec: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_averages_model_speed() {
        let db = Database::open_in_memory().expect("open in-memory db");

        let model = ModelRecord {
            id: "m1".to_string(),
            display_name: "Test Model".to_string(),
            provider_type: "llama_cpp".to_string(),
            file_path: None,
            mmproj_path: None,
            hf_repo: None,
            quantization: None,
            supports_thinking: false,
            default_params: ModelParams::default(),
            avg_tokens_per_sec: None,
        };
        db.upsert_model(&model).unwrap();

        db.record_model_speed("m1", 30.0).unwrap();
        let after_first = db.get_model("m1").unwrap().unwrap();
        assert_eq!(after_first.avg_tokens_per_sec, Some(30.0));

        db.record_model_speed("m1", 50.0).unwrap();
        let after_second = db.get_model("m1").unwrap().unwrap();
        assert_eq!(after_second.avg_tokens_per_sec, Some(40.0));
    }
}
