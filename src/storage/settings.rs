//! Key-value application settings and grouped personalization snapshots.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{OptionalExtension, params};

use crate::core::AssistantPersonality;
use crate::core::types::SearchEngine;

use super::Database;

/// Snapshot of global personalization settings fetched in a single DB lock acquisition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonalizationSettings {
    pub assistant_name: String,
    pub user_name: Option<String>,
    pub personality: AssistantPersonality,
    pub custom_personality: Option<String>,
    pub user_about: Option<String>,
}

impl Database {
    /// Fetch a raw setting value by key.
    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT value FROM app_settings WHERE key = ?1")?;
            let val = stmt.query_row(params![key], |row| row.get(0)).optional()?;
            Ok(val)
        })
    }

    /// Persist or delete a raw setting value by key.
    pub fn set_setting(&self, key: &str, value: Option<&str>) -> Result<()> {
        self.with_conn(|conn| {
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
        })
    }

    /// Load all personalization settings in one SQLite query.
    pub fn get_personalization_settings(&self) -> Result<PersonalizationSettings> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT key, value FROM app_settings
                 WHERE key IN (
                     'assistant_name',
                     'user_name',
                     'assistant_personality',
                     'assistant_custom_personality',
                     'user_about'
                 )",
            )?;
            let mut map: HashMap<String, String> = HashMap::new();
            let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
            for row in rows {
                let (k, v) = row?;
                map.insert(k, v);
            }

            let assistant_name = map
                .get("assistant_name")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "Companion".to_string());
            let user_name = map
                .get("user_name")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let personality = map
                .get("assistant_personality")
                .map(|s| AssistantPersonality::from_str_lossy(s))
                .unwrap_or_default();
            let custom_personality = map
                .get("assistant_custom_personality")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let user_about = map
                .get("user_about")
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());

            Ok(PersonalizationSettings {
                assistant_name,
                user_name,
                personality,
                custom_personality,
                user_about,
            })
        })
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
        Ok(self
            .get_setting("user_name")?
            .map(|saved| saved.trim().to_string())
            .filter(|trimmed| !trimmed.is_empty()))
    }

    pub fn set_user_name(&self, name: &str) -> Result<()> {
        // Persist empty string explicitly so clearing the field does not fall back to OS name.
        self.set_setting("user_name", Some(name.trim()))
    }

    pub fn get_assistant_personality(&self) -> Result<AssistantPersonality> {
        Ok(self
            .get_setting("assistant_personality")?
            .map(|s| AssistantPersonality::from_str_lossy(&s))
            .unwrap_or_default())
    }

    pub fn set_assistant_personality(&self, personality: AssistantPersonality) -> Result<()> {
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

    pub fn get_search_engine(&self) -> Result<SearchEngine> {
        Ok(self
            .get_setting("web_search_engine")?
            .map(|s| SearchEngine::from_str_lossy(&s))
            .unwrap_or_default())
    }

    pub fn set_search_engine(&self, engine: SearchEngine) -> Result<()> {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn personalization_snapshot_and_web_search_settings_round_trip() {
        let db = Database::open_in_memory().expect("open in-memory db");

        let defaults = db.get_personalization_settings().unwrap();
        assert_eq!(defaults.assistant_name, "Companion");
        assert_eq!(defaults.user_name, None);
        assert_eq!(defaults.personality, AssistantPersonality::default());
        assert_eq!(defaults.custom_personality, None);
        assert_eq!(defaults.user_about, None);

        db.set_assistant_name("Nova").unwrap();
        db.set_user_name("Ada").unwrap();
        db.set_assistant_personality(AssistantPersonality::Custom)
            .unwrap();
        db.set_custom_personality("Speak concisely.").unwrap();
        db.set_user_about("Rust developer").unwrap();

        let snap = db.get_personalization_settings().unwrap();
        assert_eq!(snap.assistant_name, "Nova");
        assert_eq!(snap.user_name.as_deref(), Some("Ada"));
        assert_eq!(snap.personality, AssistantPersonality::Custom);
        assert_eq!(snap.custom_personality.as_deref(), Some("Speak concisely."));
        assert_eq!(snap.user_about.as_deref(), Some("Rust developer"));

        assert!(!db.get_web_search_global_enabled().unwrap());
        db.set_web_search_global_enabled(true).unwrap();
        assert!(db.get_web_search_global_enabled().unwrap());

        assert_eq!(db.get_search_engine().unwrap(), SearchEngine::DuckDuckGo);
        db.set_search_engine(SearchEngine::SearXNG).unwrap();
        assert_eq!(db.get_search_engine().unwrap(), SearchEngine::SearXNG);

        db.set_searxng_url("  https://search.example.org/// ").unwrap();
        assert_eq!(
            db.get_searxng_url().unwrap().as_deref(),
            Some("https://search.example.org")
        );
        db.set_searxng_url("   ").unwrap();
        assert_eq!(db.get_searxng_url().unwrap(), None);
    }
}
