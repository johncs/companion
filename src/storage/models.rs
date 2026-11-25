//! Model registry persistence queries.

use anyhow::Result;
use rusqlite::{OptionalExtension, Row, params};

use super::Database;
use super::entities::{ModelRecord, PendingDownload};

impl Database {
    /// Return all registered models ordered by display name.
    pub fn list_models(&self) -> Result<Vec<ModelRecord>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn.prepare(
            "SELECT id, display_name, provider_type, file_path, mmproj_path, hf_repo, quantization, supports_thinking, default_params_json, avg_tokens_per_sec
             FROM models
             ORDER BY display_name ASC",
        )?;
        let rows = stmt.query_map([], map_model)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// Fetch a single model record by ID.
    pub fn get_model(&self, id: &str) -> Result<Option<ModelRecord>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.query_row(
            "SELECT id, display_name, provider_type, file_path, mmproj_path, hf_repo, quantization, supports_thinking, default_params_json, avg_tokens_per_sec
             FROM models WHERE id = ?1",
            params![id],
            map_model,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Insert or update a model record.
    pub fn upsert_model(&self, model: &ModelRecord) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
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
                model.default_params_json,
            ],
        )?;
        Ok(())
    }

    /// Delete a model record from SQLite.
    pub fn delete_model(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        conn.execute("DELETE FROM models WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Record a measured token-generation speed (`tokens_per_sec`) and update the model's
    /// capped running average (window up to 20 samples).
    pub fn record_model_speed(&self, id: &str, tokens_per_sec: f64) -> Result<()> {
        if !tokens_per_sec.is_finite() || tokens_per_sec <= 0.0 {
            return Ok(());
        }
        let conn = self.conn.lock().expect("database mutex poisoned");
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
    }

    /// Register a local `.gguf` model in SQLite (used by HF downloads, local imports, and recovery).
    pub fn register_local_gguf(
        &self,
        display_name: Option<String>,
        file_path: &std::path::Path,
        mmproj_path: Option<&std::path::Path>,
        hf_repo: Option<String>,
    ) -> Result<ModelRecord> {
        let filename = file_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("model.gguf");
        let name = display_name.unwrap_or_else(|| crate::models::friendly_model_name(filename));
        let is_starter = hf_repo.as_deref()
            == Some(crate::models::huggingface::RECOMMENDED_STARTER_REPO);
        let default_params_json = if is_starter {
            r#"{"n_gpu_layers":0}"#.to_string()
        } else {
            "{}".to_string()
        };
        let record = ModelRecord {
            id: uuid::Uuid::new_v4().to_string(),
            display_name: name,
            provider_type: "llama_cpp".to_string(),
            file_path: Some(file_path.to_string_lossy().to_string()),
            mmproj_path: mmproj_path.map(|p| p.to_string_lossy().to_string()),
            hf_repo,
            quantization: crate::models::infer_quantization(filename),
            supports_thinking: crate::models::detect_gguf_supports_thinking(file_path),
            default_params_json,
            avg_tokens_per_sec: None,
        };
        self.upsert_model(&record)?;
        Ok(record)
    }

    /// Synchronize the `models` table with `models_dir`:
    /// 1. Prune any model record whose `file_path` is missing on disk.
    /// 2. Register any `.gguf` model found in `models_dir` that is not yet in SQLite.
    /// 3. Ensure `active_model_id` always points to an existing registered model (or `None`).
    pub fn sync_managed_models(&self, models_dir: &std::path::Path) -> Result<()> {
        let mut valid_models = Vec::new();
        let mut seen_paths = std::collections::HashSet::new();

        for m in self.list_models()? {
            let exists = m
                .file_path
                .as_deref()
                .is_some_and(|p| std::path::Path::new(p).exists());
            if exists {
                if let Some(p) = &m.file_path {
                    seen_paths.insert(p.clone());
                }
                valid_models.push(m);
            } else {
                self.delete_model(&m.id)?;
            }
        }

        for item in crate::models::discover_managed_models_in(models_dir) {
            let path_str = item.file_path.to_string_lossy().to_string();
            if !seen_paths.contains(&path_str) {
                let record = self.register_local_gguf(
                    Some(item.display_name),
                    &item.file_path,
                    item.mmproj_path.as_deref(),
                    item.hf_repo,
                )?;
                seen_paths.insert(path_str);
                valid_models.push(record);
            }
        }

        let active_id = self.get_active_model_id()?;
        let active_is_valid = active_id
            .as_deref()
            .is_some_and(|aid| valid_models.iter().any(|m| m.id == aid));
        if !active_is_valid {
            let fallback = valid_models.first().map(|m| m.id.as_str());
            self.set_active_model_id(fallback)?;
        }

        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        let mut stmt = conn.prepare("SELECT value FROM app_settings WHERE key = ?1")?;
        let val = stmt.query_row(params![key], |row| row.get(0)).optional()?;
        Ok(val)
    }

    pub fn set_setting(&self, key: &str, value: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().expect("database mutex poisoned");
        match value {
            Some(v) => {
                conn.execute(
                    "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    params![key, v],
                )?;
            }
            None => {
                conn.execute("DELETE FROM app_settings WHERE key = ?1", params![key])?;
            }
        }
        Ok(())
    }

    pub fn get_active_model_id(&self) -> Result<Option<String>> {
        self.get_setting("active_model_id")
    }

    pub fn set_active_model_id(&self, model_id: Option<&str>) -> Result<()> {
        self.set_setting("active_model_id", model_id)
    }

    pub fn get_assistant_name(&self) -> Result<String> {
        Ok(self
            .get_setting("assistant_name")?
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Companion".to_string()))
    }

    pub fn set_assistant_name(&self, name: &str) -> Result<()> {
        let trimmed = name.trim();
        let val = (!trimmed.is_empty() && trimmed != "Companion").then_some(trimmed);
        self.set_setting("assistant_name", val)
    }

    pub fn get_user_name(&self) -> Result<Option<String>> {
        match self.get_setting("user_name")? {
            Some(saved) => {
                let trimmed = saved.trim().to_string();
                Ok((!trimmed.is_empty()).then_some(trimmed))
            }
            None => Ok(os_real_name()),
        }
    }

    pub fn set_user_name(&self, name: &str) -> Result<()> {
        // Persist empty string explicitly so clearing the field does not fall back to OS name.
        self.set_setting("user_name", Some(name.trim()))
    }

    pub fn get_assistant_personality(&self) -> Result<crate::core::AssistantPersonality> {
        Ok(self
            .get_setting("assistant_personality")?
            .map(|s| crate::core::AssistantPersonality::from_str_lossy(&s))
            .unwrap_or_default())
    }

    pub fn set_assistant_personality(
        &self,
        personality: crate::core::AssistantPersonality,
    ) -> Result<()> {
        self.set_setting("assistant_personality", Some(personality.as_str()))
    }

    pub fn get_custom_personality(&self) -> Result<Option<String>> {
        Ok(self
            .get_setting("assistant_custom_personality")?
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()))
    }

    pub fn set_custom_personality(&self, text: &str) -> Result<()> {
        let trimmed = text.trim();
        self.set_setting(
            "assistant_custom_personality",
            (!trimmed.is_empty()).then_some(trimmed),
        )
    }

    pub fn get_user_about(&self) -> Result<Option<String>> {
        Ok(self
            .get_setting("user_about")?
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()))
    }

    pub fn set_user_about(&self, about: &str) -> Result<()> {
        let trimmed = about.trim();
        self.set_setting("user_about", (!trimmed.is_empty()).then_some(trimmed))
    }

    pub fn get_web_search_global_enabled(&self) -> Result<bool> {
        Ok(matches!(
            self.get_setting("web_search_global_enabled")?.as_deref(),
            Some("true" | "1")
        ))
    }

    pub fn set_web_search_global_enabled(&self, enabled: bool) -> Result<()> {
        self.set_setting(
            "web_search_global_enabled",
            Some(if enabled { "true" } else { "false" }),
        )
    }

    pub fn get_search_engine(&self) -> Result<crate::agent::web_search::SearchEngine> {
        Ok(self
            .get_setting("web_search_engine")?
            .map(|s| crate::agent::web_search::SearchEngine::from_str_lossy(&s))
            .unwrap_or_default())
    }

    pub fn set_search_engine(
        &self,
        engine: crate::agent::web_search::SearchEngine,
    ) -> Result<()> {
        self.set_setting("web_search_engine", Some(engine.as_str()))
    }

    pub fn get_searxng_url(&self) -> Result<Option<String>> {
        Ok(self
            .get_setting("web_search_searxng_url")?
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty()))
    }

    pub fn set_searxng_url(&self, url: &str) -> Result<()> {
        let trimmed = url.trim().trim_end_matches('/');
        self.set_setting(
            "web_search_searxng_url",
            (!trimmed.is_empty()).then_some(trimmed),
        )
    }

    pub fn list_pending_downloads(&self) -> Result<Vec<PendingDownload>> {
        let raw = self.get_setting("pending_downloads")?;
        Ok(raw
            .and_then(|s| serde_json::from_str::<Vec<PendingDownload>>(&s).ok())
            .unwrap_or_default())
    }

    fn save_pending_downloads(&self, items: &[PendingDownload]) -> Result<()> {
        if items.is_empty() {
            self.set_setting("pending_downloads", None)
        } else {
            let json = serde_json::to_string(items)?;
            self.set_setting("pending_downloads", Some(&json))
        }
    }

    pub fn upsert_pending_download(&self, item: PendingDownload) -> Result<()> {
        let mut items = self.list_pending_downloads()?;
        if let Some(existing) = items
            .iter_mut()
            .find(|x| x.repo_id == item.repo_id && x.filename == item.filename)
        {
            *existing = item;
        } else {
            items.push(item);
        }
        self.save_pending_downloads(&items)
    }

    pub fn update_pending_download_total(
        &self,
        repo_id: &str,
        filename: &str,
        total_bytes: u64,
    ) -> Result<()> {
        let mut items = self.list_pending_downloads()?;
        if let Some(existing) = items
            .iter_mut()
            .find(|x| x.repo_id == repo_id && x.filename == filename)
            && existing.total_bytes != Some(total_bytes)
        {
            existing.total_bytes = Some(total_bytes);
            self.save_pending_downloads(&items)?;
        }
        Ok(())
    }

    pub fn remove_pending_download(&self, repo_id: &str, filename: &str) -> Result<()> {
        let mut items = self.list_pending_downloads()?;
        let before = items.len();
        items.retain(|x| !(x.repo_id == repo_id && x.filename == filename));
        if items.len() != before {
            self.save_pending_downloads(&items)?;
        }
        Ok(())
    }
}

fn os_real_name() -> Option<String> {
    let name = gtk::glib::real_name().to_string_lossy().trim().to_string();
    (!name.is_empty() && name != "Unknown").then_some(name)
}

fn map_model(row: &Row<'_>) -> rusqlite::Result<ModelRecord> {
    let supports_thinking: i64 = row.get(7)?;
    Ok(ModelRecord {
        id: row.get(0)?,
        display_name: row.get(1)?,
        provider_type: row.get(2)?,
        file_path: row.get(3)?,
        mmproj_path: row.get(4)?,
        hf_repo: row.get(5)?,
        quantization: row.get(6)?,
        supports_thinking: supports_thinking != 0,
        default_params_json: row.get(8)?,
        avg_tokens_per_sec: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_averages_model_speed() {
        let dir = std::env::temp_dir().join(format!("companion_speed_{}", uuid::Uuid::new_v4()));
        let db = Database::open(&dir.join("test.db")).expect("open test db");

        let model = ModelRecord {
            id: "m1".to_string(),
            display_name: "Test Model".to_string(),
            provider_type: "llama_cpp".to_string(),
            file_path: None,
            mmproj_path: None,
            hf_repo: None,
            quantization: None,
            supports_thinking: false,
            default_params_json: "{}".to_string(),
            avg_tokens_per_sec: None,
        };
        db.upsert_model(&model).unwrap();

        db.record_model_speed("m1", 30.0).unwrap();
        let after_first = db.get_model("m1").unwrap().unwrap();
        assert_eq!(after_first.avg_tokens_per_sec, Some(30.0));

        db.record_model_speed("m1", 50.0).unwrap();
        let after_second = db.get_model("m1").unwrap().unwrap();
        assert_eq!(after_second.avg_tokens_per_sec, Some(40.0));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sync_managed_models_handles_all_cases_and_active_invariant() {
        let dir = std::env::temp_dir().join(format!("companion_sync_{}", uuid::Uuid::new_v4()));
        let models_dir = dir.join("models");
        std::fs::create_dir_all(&models_dir).unwrap();
        let db = Database::open(&dir.join("test.db")).expect("open test db");

        // 1. Empty DB + empty directory -> stays empty, active_model_id is None.
        db.sync_managed_models(&models_dir).unwrap();
        assert!(db.list_models().unwrap().is_empty());
        assert_eq!(db.get_active_model_id().unwrap(), None);

        // 2. Empty DB + model in directory -> recovers model and selects it as active.
        let repo_dir = models_dir.join("google--gemma-4-E2B-it-qat-q4_0-gguf");
        std::fs::create_dir_all(&repo_dir).unwrap();
        let model_path = repo_dir.join("gemma-4-e2b-it-q4_0.gguf");
        std::fs::write(&model_path, b"dummy").unwrap();
        std::fs::write(repo_dir.join("gemma-4-E2B-it-mmproj.gguf"), b"dummy").unwrap();

        db.sync_managed_models(&models_dir).unwrap();
        let models = db.list_models().unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(
            db.get_active_model_id().unwrap().as_deref(),
            Some(models[0].id.as_str())
        );

        // 3. Model in DB + same file on disk -> preserves record and active selection.
        db.record_model_speed(&models[0].id, 42.0).unwrap();
        db.sync_managed_models(&models_dir).unwrap();
        let kept = db.list_models().unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, models[0].id);
        assert_eq!(kept[0].avg_tokens_per_sec, Some(42.0));

        // 4. Model in DB + file deleted from directory -> prunes DB and clears active_model_id.
        std::fs::remove_dir_all(&repo_dir).unwrap();
        db.sync_managed_models(&models_dir).unwrap();
        assert!(db.list_models().unwrap().is_empty());
        assert_eq!(db.get_active_model_id().unwrap(), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persists_and_removes_pending_downloads() {
        let dir = std::env::temp_dir().join(format!("companion_pend_{}", uuid::Uuid::new_v4()));
        let db = Database::open(&dir.join("test.db")).expect("open test db");

        db.upsert_pending_download(PendingDownload {
            repo_id: "google/gemma-4-E2B-it-qat-q4_0-gguf".to_string(),
            filename: "gemma-4-E2B-it-Q4_0.gguf".to_string(),
            mmproj_filename: Some("gemma-4-E2B-it-mmproj.gguf".to_string()),
            display_name: "Gemma 4 E2B".to_string(),
            total_bytes: None,
        })
        .unwrap();
        db.update_pending_download_total(
            "google/gemma-4-E2B-it-qat-q4_0-gguf",
            "gemma-4-E2B-it-Q4_0.gguf",
            3_000_000_000,
        )
        .unwrap();

        let list = db.list_pending_downloads().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].total_bytes, Some(3_000_000_000));

        db.remove_pending_download(
            "google/gemma-4-E2B-it-qat-q4_0-gguf",
            "gemma-4-E2B-it-Q4_0.gguf",
        )
        .unwrap();
        assert!(db.list_pending_downloads().unwrap().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn starter_model_defaults_to_cpu_mode() {
        let dir = std::env::temp_dir().join(format!("companion_starter_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Database::open(&dir.join("test.db")).expect("open test db");
        let dummy_gguf = dir.join("model.gguf");
        std::fs::write(&dummy_gguf, b"GGUF").unwrap();

        let starter = db
            .register_local_gguf(
                None,
                &dummy_gguf,
                None,
                Some(crate::models::huggingface::RECOMMENDED_STARTER_REPO.to_string()),
            )
            .unwrap();
        assert_eq!(starter.default_params_json, r#"{"n_gpu_layers":0}"#);

        let other = db
            .register_local_gguf(None, &dummy_gguf, None, Some("other/model".to_string()))
            .unwrap();
        assert_eq!(other.default_params_json, "{}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
