//! Shared application service container constructed once at startup in `main.rs`.

use std::sync::Arc;

use crate::agent::web_search::WebSearchService;
use crate::core::paths::AppPaths;
use crate::inference::llama_cpp::LlamaCppProvider;
use crate::models::download_manager::{DownloadManager, HfModelFetcher};
use crate::models::gguf::GgufMetadataCache;
use crate::models::huggingface::HfClient;
use crate::models::registry::ModelRegistryService;
use crate::storage::{Database, ModelRecord};

/// Shared, cheaply cloneable handles to all application services and caches.
#[derive(Clone)]
pub struct AppServices {
    pub db: Database,
    pub rt_handle: tokio::runtime::Handle,
    pub paths: AppPaths,
    pub provider: Arc<LlamaCppProvider>,
    pub download_manager: Arc<DownloadManager>,
    pub hf_client: Arc<HfClient>,
    pub web_search: Arc<WebSearchService>,
    pub gguf_cache: Arc<GgufMetadataCache>,
}

impl AppServices {
    /// Construct the single shared service graph for the application process.
    pub fn new(
        db: Database,
        rt_handle: tokio::runtime::Handle,
        paths: AppPaths,
    ) -> anyhow::Result<Self> {
        let hf_client = Arc::new(HfClient::new());
        let fetcher = Arc::new(HfModelFetcher::with_client((*hf_client).clone()));
        let download_manager = Arc::new(DownloadManager::with_fetcher(
            db.clone(),
            paths.models_dir.clone(),
            rt_handle.clone(),
            fetcher,
        ));
        let provider = Arc::new(LlamaCppProvider::new(rt_handle.clone()));
        let web_search = Arc::new(WebSearchService::new()?);
        let gguf_cache = Arc::new(GgufMetadataCache::new());
        Ok(Self {
            db,
            rt_handle,
            paths,
            provider,
            download_manager,
            hf_client,
            web_search,
            gguf_cache,
        })
    }

    /// Run blocking startup initialization (user-name default, model sync, cache warmups).
    pub fn run_startup_sync(&self, real_name: &str) -> (Option<ModelRecord>, Option<String>) {
        if !real_name.is_empty()
            && real_name != "Unknown"
            && self.db.get_setting("user_name").ok().flatten().is_none()
            && let Err(err) = self.db.set_user_name(real_name)
        {
            tracing::warn!("Failed to persist default user name: {err}");
        }
        if let Err(err) = ModelRegistryService::sync_managed_models_with_cache(
            &self.db,
            &self.paths.models_dir,
            &self.gguf_cache,
        ) {
            tracing::warn!("Failed to synchronize managed models: {err}");
        }
        let color_scheme = self.db.get_setting("color_scheme").ok().flatten();
        let all_models = self.db.list_models().unwrap_or_default();
        let saved_id = self.db.get_active_model_id().ok().flatten();
        let active_model = saved_id
            .as_deref()
            .and_then(|id| all_models.iter().find(|m| m.id == id).cloned())
            .or_else(|| all_models.first().cloned());
        let paths = all_models
            .into_iter()
            .filter_map(|m| m.file_path.map(Into::into))
            .collect();
        self.gguf_cache.warm(paths, &self.rt_handle);
        crate::format::markdown::warm_syntax_highlighting();
        crate::ui::composer::cleanup_orphaned_clipboard_files(&self.paths.clipboard_dir);
        (active_model, color_scheme)
    }
}
