# Companion — Architecture & Code Quality Refactor Plan

> **Engineering Standards Reference:** [AGENTS.md](./AGENTS.md)  
> **Feature Roadmap Reference:** [SPRINT.md](./SPRINT.md)  
> **Approach:** Incremental, behavior-preserving vertical slices — every sprint ends with a compiling, runnable app (`cargo test && cargo run`) and measurably cleaner architecture, stronger types, and higher unit-test coverage.

---

## Executive Summary & Baseline Metrics

Companion's leaf utilities (`ThinkStreamParser`, `agent::parser`, `privacy_guard`, `path_guard`, `download_state`, `trim_turns_to_budget`) are cleanly written and tested, with **zero `.unwrap()` calls** in production code. However, as features grew rapidly, core application workflows became coupled to the GTK/Relm4 UI layer and a monolithic `Database` struct.

| Quality Metric | Current Baseline | Refactor Target |
|---|---|---|
| Total Rust LOC (`src/`) | ~20,193 lines across 78 files | Modular, cohesive files |
| Files > 400 lines | **12 files** (max: 619 lines in `ui/chat_view/mod.rs`) | **0 files** > 400 lines |
| Functions > 50 lines | **78 functions** (max: 374 lines in `agent_runner.rs`) | **0 core/domain fns** > 50 lines; UI view builders split cleanly |
| `#[allow(clippy::too_many_arguments)]` | **12 occurrences** | **0 occurrences** |
| `.expect(...)` on DB mutexes | **29 occurrences** in `src/storage/` | **0** (centralized `with_conn` / `with_tx` returning `Result`) |
| Non-UI modules importing `gtk::glib` | **3** (`core`, `models`, `storage`) | **0** |
| Dependency graph cycles | `core ↔ storage`, `storage → agent/models` | **Strict acyclic DAG** (compiler-enforced via Cargo workspace) |
| Unit tests | **73 tests** (0 covering agent loop, tool runner, prompt builder, download worker, or model prompt formatting) | **150+ fast headless unit tests** runnable without GTK4 or `llama.cpp` |

### Target Layered Architecture (Strict Acyclic DAG)

```text
┌──────────────────────────────────────────────────────────┐
│                     companion-app (UI)                   │
│   Relm4 components, Adw dialogs, GTK ListView rendering  │
└──────────────┬──────────────┬──────────────┬─────────────┘
               │              │              │
               ▼              ▼              ▼
┌──────────────────┐ ┌────────────────┐ ┌──────────────────┐
│  companion-agent │ │companion-models│ │companion-infer   │
│  AgentRunner,    │ │DownloadManager,│ │ModelProvider,    │
│  ToolRegistry,   │ │ModelRegistry,  │ │LlamaCppProvider, │
│  Permissions,    │ │GGUF, HF, HW    │ │Think/Stop/Prompt │
│  WebSearch       │ │                │ │                  │
└────────┬─────────┘ └───────┬────────┘ └────────┬─────────┘
         │                   │                   │
         └───────────────────┼───────────────────┘
                             ▼
                  ┌─────────────────────┐
                  │  companion-storage  │
                  │  SQLite repos only  │
                  │  (no domain logic)  │
                  └──────────┬──────────┘
                             ▼
                  ┌─────────────────────┐
                  │   companion-core    │
                  │  Domain types/enums │
                  │  Markdown, Prompt,  │
                  │  Runtime, AppPaths  │
                  └─────────────────────┘
```

---

## Refactor Sprint 1: Pure Domain Types & Zero-GTK Core Boundaries

**Goal:** Eliminate every `gtk::glib` dependency from non-UI modules and replace stringly-typed fields and ad-hoc JSON parsing with strongly-typed domain structs and enums.

### 1.1 Remove `gtk::glib` from `core`, `models`, and `storage`
- [x] **Pure-Rust Pango/XML escaper in `src/core/markdown.rs`:**
  - Remove `use gtk::glib;` (currently used solely for `glib::markup_escape_text`).
  - Add `pub fn escape_pango_markup(input: &str) -> String` (escaping `&`, `<`, `>`, `"`, `'`) in `src/core/markdown.rs` and reuse it everywhere non-UI code escapes text.
- [x] **Inject application paths (`AppPaths`) instead of calling `glib::user_data_dir()` inside domain modules:**
  - Create `src/core/paths.rs` with `pub struct AppPaths { pub db_path: PathBuf, pub models_dir: PathBuf, pub clipboard_dir: PathBuf }`.
  - Construct `AppPaths` once in `src/main.rs` (from `glib::user_data_dir()`) and pass it explicitly into `models`, `storage`, and `ui` components.
  - Remove `use gtk::glib;` and global `managed_models_dir()` from `src/models/mod.rs`.
- [x] **Inject default OS username instead of calling `gtk::glib::real_name()` in `src/storage/models.rs`:**
  - Remove `os_real_name()` from `src/storage/models.rs`.
  - Have `get_user_name()` return raw persisted `Option<String>` (distinguishing unset vs. explicitly cleared) and apply the OS real-name fallback at the application/UI boundary (`main.rs` / settings service).

### 1.2 Replace `default_params_json: String` with Typed `ModelParams`
Currently `ModelRecord.default_params_json` is parsed ad-hoc with `serde_json::Value` in **5 separate places** (`storage/entities.rs`, `ui/chat_view/page_loader.rs`, `ui/model_config_dialog.rs`, `ui/composer/input_helpers.rs`, `ui/composer/mod.rs`).
- [x] Define `ModelParams` in `src/core/model_params.rs`:
  ```rust
  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
  #[serde(default)]
  pub struct ModelParams {
      pub n_ctx: u32,
      pub n_gpu_layers: u32,
      pub max_tokens: u32,
      pub temperature: f32,
      pub thinking_level: ThinkingLevel,
  }
  ```
- [x] Add `ModelParams::from_json_lossy(raw: &str) -> Self` and `ModelParams::to_json(&self) -> Result<String>` (preserving forward-compatibility if unknown fields exist or malformed JSON is stored).
- [x] Store `pub default_params: ModelParams` directly on `ModelRecord` (serializing/deserializing strictly inside `storage/models.rs`).
- [x] Delete duplicate parsers (`parse_model_params` in `page_loader.rs`, inline `serde_json::from_str` blocks in `model_config_dialog.rs`, `composer/input_helpers.rs`, `composer/mod.rs`, and `ModelRecord::default_thinking_level()`).

### 1.3 Replace Stringly-Typed Entity Fields with Domain Enums
- [x] Move/define shared value enums in `src/core/types.rs` (each with `as_str()`, `from_str_lossy()`, and `Serialize`/`Deserialize`):
  - `MessageRole` (`System`, `User`, `Assistant`) — unify with `inference::provider::ChatRole`.
  - `FolderPermission` (`Read`, `ReadWrite`).
  - `ToolCallStatus` (`Ok`, `Denied`, `Error`).
  - `PermissionDecision` (`Deny`, `AllowOnce`, `AllowChat`) — shared by `agent::permissions` and `storage`.
  - `ExecutionDevice` (`Cpu`, `Gpu`).
  - `SearchEngine` (`DuckDuckGo`, `SearXNG`) — move from `agent::web_search` to `core` so `storage` does not depend on `agent`.
- [x] Update `Conversation`, `ConversationFolder`, `Message`, and `ToolCallRecord` in `src/storage/entities.rs` to use these typed enums instead of raw `String`s.
- [x] Replace the 10-argument `Database::insert_message_with_metrics` (`#[allow(clippy::too_many_arguments)]` in `src/storage/messages.rs`) with a clean `NewMessage<'a>` builder/struct (`db.insert_message(NewMessage { ... })`).
- [x] Remove dead `#[allow(dead_code)]` `Persona` struct in `src/storage/entities.rs` if unused, or document its planned sprint.

### 1.4 Tests & Verification
- [x] Add unit tests for `escape_pango_markup` (special chars, unicode, empty strings).
- [x] Add unit tests for `ModelParams` round-trip serialization, partial JSON defaults, and invalid JSON fallback.
- [x] Add round-trip unit tests for all new domain enums (`MessageRole`, `FolderPermission`, `ToolCallStatus`, `ExecutionDevice`).
- [x] **Verification:** `grep -rn 'gtk::glib' src/core src/models src/storage` returns zero matches; `cargo test` passes.

---

## Refactor Sprint 2: Storage Hardening, Atomic Migrations & Responsibility Split

**Goal:** Turn `src/storage/` into a pure, panic-free, transaction-safe persistence layer with zero business logic, zero filesystem scanning, and zero main-thread blocking calls.

### 2.1 Centralize Locking & Eliminate Mutex `.expect()` Calls
- [ ] Add helper methods on `Database` in `src/storage/mod.rs`:
  - `fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T>`
  - `fn with_tx<T>(&self, f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T>) -> Result<T>`
- [ ] Return a clean `anyhow::Error` if the mutex is ever poisoned instead of panicking.
- [ ] Replace all **29** `.conn.lock().expect("database mutex poisoned")` calls across `storage/*.rs` with `self.with_conn(...)` / `self.with_tx(...)`.

### 2.2 Make Schema Migrations Atomic & Crash-Safe
- [ ] Refactor `run_migrations` in `src/storage/schema.rs`:
  - Represent migrations as an ordered slice `const MIGRATIONS: &[(i32, fn(&Transaction<'_>) -> Result<()>)]`.
  - Run **each** pending migration `(version, migrate_fn)` together with `PRAGMA user_version = <version>` inside its own SQLite transaction.
  - A failure on migration `N` rolls back cleanly to `N - 1` instead of leaving a half-migrated database.
- [ ] Add **Migration v16**: create a proper `pending_downloads` table:
  ```sql
  CREATE TABLE IF NOT EXISTS pending_downloads (
      repo_id         TEXT NOT NULL,
      filename        TEXT NOT NULL,
      mmproj_filename TEXT,
      display_name    TEXT NOT NULL,
      total_bytes     INTEGER,
      created_at      INTEGER NOT NULL,
      PRIMARY KEY (repo_id, filename)
  );
  ```
  and migrate any existing JSON blob from `app_settings` (`key = 'pending_downloads'`) into the new table during v16.

### 2.3 Split `storage/models.rs` (549 lines) & Extract Business Logic
- [ ] **Extract `src/storage/settings.rs`:**
  - Move `get_setting`, `set_setting`, `get/set_active_model_id`, `get/set_assistant_name`, `get/set_user_name`, `get/set_assistant_personality`, `get/set_custom_personality`, `get/set_user_about`, `get/set_web_search_global_enabled`, `get/set_search_engine`, and `get/set_searxng_url` into `src/storage/settings.rs`.
  - Add a grouped `PersonalizationSettings` snapshot struct (`db.get_personalization_settings()`) loaded in **one** transaction instead of 5 separate lock acquisitions on every chat turn.
- [ ] **Extract `src/storage/downloads.rs`:**
  - Move `list_pending_downloads`, `upsert_pending_download`, `update_pending_download_total`, and `remove_pending_download` to SQL queries against the new `pending_downloads` table (eliminating the read-modify-write lost-update race on the JSON blob).
- [ ] **Move model disk sync out of `storage` → `src/models/registry.rs`:**
  - Move `register_local_gguf` and `sync_managed_models` out of `src/storage/models.rs` into `src/models/registry.rs` (`ModelRegistryService`).
  - `storage/models.rs` shrinks to pure SQL CRUD (`list_models`, `get_model`, `upsert_model`, `delete_model`, `record_model_speed`) with **zero** imports from `crate::models`.
- [ ] **Move prompt composition out of `storage/conversations.rs` → `src/core/prompt.rs`:**
  - Move `CORE_SYSTEM_RULES`, `DEFAULT_SYSTEM_PROMPT`, `render_system_prompt`, and `compose_personalized_system_prompt` out of `src/storage/conversations.rs` into `src/core/prompt.rs`.
  - Extract `src/storage/folders.rs` for `conversation_folders` queries so `storage/conversations.rs` drops well below 350 lines.

### 2.4 Fix `duplicate_conversation` Data Loss & Async DB Access
- [ ] Update `Database::duplicate_conversation` (`src/storage/conversations.rs`) to:
  - Copy `model_name`, `execution_device`, and `generation_ms` on duplicated messages (currently dropped).
  - Copy `attachments` and `tool_calls` mapped to each newly generated message ID inside the same transaction.
- [ ] Audit and offload remaining synchronous SQLite calls on the GTK main thread (e.g., `LoadConversation` metadata load in `chat_view/mod.rs`, `insert_message_with_metrics` / `insert_tool_call` in `agent_runner.rs`) onto `core::runtime()` via `spawn_blocking`.

### 2.5 Tests & Verification
- [ ] Add migration unit tests: fresh DB → v16, upgrading from v15 with legacy `pending_downloads` JSON → v16, and rollback verification on a simulated failed migration.
- [ ] Add unit tests for `duplicate_conversation` asserting message metrics, attachments, tool calls, and folders are all preserved.
- [ ] Add unit tests for `storage/downloads.rs` concurrent upserts/updates/deletes.
- [ ] **Verification:** `src/storage/` has zero imports from `crate::agent` or `crate::models`, zero `.expect()` calls, and every file in `src/storage/` is `<= 350` lines. `cargo test` passes.

---

## Refactor Sprint 3: Unified `Tool` / `ToolRegistry` & Security Hardening

**Goal:** Replace scattered string-matching tool dispatch with a composable, extensible `Tool` trait + `ToolRegistry` (also preparing cleanly for Sprint 6 MCP tools in `SPRINT.md`), while fixing the SSRF redirect gap and folder-permission escalation bug.

### 3.1 Introduce `Tool` Trait & `ToolRegistry` (`src/agent/registry.rs`)
Currently, adding or modifying a tool requires editing 6 different `match tool_name` blocks across `permissions.rs`, `tools/mod.rs`, `tools/path_guard.rs`, `chat_view/agent_runner.rs`, `chat_view/tool_runner.rs`, and `preferences/tools_group.rs`.

- [ ] Define core tool abstractions in `src/agent/tool.rs`:
  ```rust
  pub enum ToolRisk {
      ReadOnly,          // e.g. list_files, read_file, search_files
      WriteWorkspace,    // e.g. write_file, create_directory, move_path
      Destructive,       // e.g. delete_file, delete_directory (always ask one-by-one)
      Network,           // e.g. web_search, fetch_url (always review + privacy check)
  }

  pub struct ToolContext<'a> {
      pub granted_folders: &'a [GrantedFolder],
      pub web_search_enabled: bool,
      pub search_engine: SearchEngine,
      pub searxng_url: Option<&'a str>,
      pub local_texts: &'a [String],
  }

  pub enum ApprovalRequest {
      ToolAction {
          tool_name: String,
          preview: String,
          risk: ToolRisk,
          ungranted_path: Option<PathBuf>,
      },
      WebSearch {
          engine_label: String,
          proposed_query: String,
          warnings: Vec<String>,
      },
      FetchUrl {
          proposed_url: String,
          warnings: Vec<String>,
      },
  }
  ```
- [ ] Define `Tool` trait (or clean enum-backed `ToolSpec` + `ToolRegistry`) where each tool owns:
  - `name(&self) -> &'static str`
  - `risk(&self) -> ToolRisk`
  - `parse_args(&self, raw: &Value) -> Result<TypedArgs>` — **single source of truth** for argument aliases (`path` / `file_path` / `filepath` / `file`, `directory`, `subdir`), fixing the drift between `describe_tool_action`, `execute_tool_with_grants`, `requested_ungranted_path`, and `is_write_already_granted`.
  - `describe(&self, ctx: &ToolContext, args: &Value) -> String`
  - `target_path(&self, args: &Value) -> Option<TargetPath>`
  - `execute(&self, ctx: &ToolContext, args: &Value) -> BoxFuture<'_, Result<String>>`
- [ ] Implement `ToolRegistry` containing all built-in file tools + `web_search` + `fetch_url`, so `AgentRunner` dispatches every tool uniformly (and future MCP tools simply register as `Tool` implementations).
- [ ] Update `PermissionEngine` (`src/agent/permissions.rs`) to consult `ToolRisk` instead of hardcoding tool name strings (`"list_files" | "read_file" | "search_files"` and `"delete_file" | "delete_directory"`).

### 3.2 Fix Security & Correctness Bugs
- [ ] **Fix over-broad folder write escalation** (currently in `ui/chat_view/tool_runner.rs:81-85` and `ui/chat_view/agent_runner.rs:291-303`):
  - Resolve the **exact** target folder/file that a write tool (`write_file`, `create_directory`, `move_path`, `delete_file`, `delete_directory`) operates on.
  - On `AllowOnce`, grant temporary write access **only** to that resolved target folder/file — never mutate `g.allow_write = true` for every folder in `granted_folders`.
  - On `AllowChat`, persist `FolderPermission::ReadWrite` **only** for the specific target folder, leaving all other read-only granted folders untouched.
- [ ] **Fix SSRF redirect bypass in `src/agent/web_search/mod.rs` (`execute_fetch_url`):**
  - Currently `shared_search_client()` uses `Policy::limited(4)` and validates `resp.url()` *after* redirects have already been requested.
  - Build a dedicated `shared_fetch_client()` with `reqwest::redirect::Policy::none()`.
  - Implement a manual redirect loop (max 4 hops): on each hop, call `validate_fetch_url` AND resolve DNS + verify `is_public_ip` for every address *before* issuing the next request (and pin/resolve cleanly).
- [ ] **Add a shared `path_guard` target-resolution helper** so `describe`, `requested_ungranted_path`, `is_write_already_granted`, and `execute` all use the exact same resolution logic.

### 3.3 Tests & Verification
- [ ] Add unit tests verifying argument-alias consistency across `describe`, `requested_ungranted_path`, and `execute` for every built-in tool.
- [ ] Add unit tests verifying `AllowOnce` and `AllowChat` on folder A **never** escalate read-only folder B to `ReadWrite`.
- [ ] Add unit tests for `execute_fetch_url` redirect validation (rejecting 302 → `http://127.0.0.1`, `http://[::1]`, `http://169.254.169.254`, `file://`, and > 4 hops).
- [ ] **Verification:** `src/agent/tools/mod.rs` is `<= 300` lines; no function in `src/agent/` exceeds 50 lines; `cargo test` passes.

---

## Refactor Sprint 4: Extract the Agent Engine Out of `src/ui/chat_view/`

**Goal:** Move the entire multi-step agent orchestration, streaming coalescer, and prompt assembly out of GTK/Relm4 into a pure, reusable, unit-testable `src/agent/runner.rs`, turning `src/ui/chat_view/` into a thin presentation layer.

### 4.1 Build Pure `AgentRunner` (`src/agent/runner.rs` + `src/agent/prompt.rs`)
- [ ] **Extract pure prompt assembly (`src/agent/prompt.rs`):**
  - Move `build_prompt_turns` out of `src/ui/chat_view/prompt_builder.rs` and `path_mentions.rs` into `src/agent/prompt.rs` and `src/agent/path_mentions.rs`.
  - Signature takes pure domain data:
    ```rust
    pub struct PromptContext<'a> {
        pub chat_system_prompt: &'a str,
        pub personalization: &'a PersonalizationSettings,
        pub granted_folders: &'a [GrantedFolder],
        pub web_search_active: bool,
        pub history: &'a [MessageWithAttachments],
    }
    pub fn build_prompt_turns(ctx: &PromptContext<'_>) -> Vec<ChatTurn>
    ```
  - Zero GTK / `gio::ListStore` / `MessageObject` dependencies.
- [ ] **Define `ApprovalHandler` trait (`src/agent/approval.rs`):**
  ```rust
  #[async_trait] // or manual BoxFuture to avoid extra proc-macro dep
  pub trait ApprovalHandler: Send + Sync {
      async fn request_approval(&self, req: ApprovalRequest) -> ApprovalResponse;
  }
  ```
- [ ] **Build `AgentRunner` (`src/agent/runner.rs`):**
  - Owns the `for step in 0..MAX_AGENT_STEPS` loop currently living in `ui/chat_view/agent_runner.rs`.
  - Runs entirely on `core::runtime()` (never `glib::MainContext::spawn_local`).
  - Accepts `Arc<dyn ModelProvider>`, `ToolRegistry`, `PermissionEngine`, `Arc<dyn ApprovalHandler>`, `Database` (or a `ChatRepository` trait), and `CancellationFlag`.
  - Emits typed `AgentEvent`s over an `mpsc::UnboundedSender<AgentEvent>`:
    - `Status(String)`
    - `StreamSnapshot { combined_answer, blocks, reasoning }`
    - `ToolCallRecorded(ToolCallRecord)`
    - `FolderGrantsUpdated`
    - `ContextUsage { total_tokens }`
    - `Notice(String)`
    - `Error(String)`
    - `Finished { saved_message_id, total_tokens, elapsed_ms, empty }`
    - `TitleUpdated(String)`
  - Performs DB persistence (`insert_message`, `insert_tool_call`, `record_model_speed`, `maybe_generate_title`) on Tokio's blocking pool — never touching the GTK main thread.
- [ ] **Move `spawn_stream_coalescer` out of `ui/chat_view/stream.rs` into `src/agent/coalescer.rs`:**
  - Already has zero GTK dependencies (once `ActiveChatRun` stays in UI); make its flush interval injectable for deterministic fast tests.

### 4.2 Slim Down `src/ui/chat_view/`
- [ ] Implement `GtkApprovalHandler` in `src/ui/chat_view/approval.rs` bridging `ApprovalRequest` → `glib::MainContext::default().spawn_local` → `AdwAlertDialog` (`permission_dialog.rs`) → `oneshot::Sender<ApprovalResponse>`.
- [ ] Reduce `ui/chat_view/agent_runner.rs` from **403 lines** to a thin ~80-line adapter that launches `AgentRunner` and applies incoming `AgentEvent`s to `MessageObject` / `ListStore` / `ComposerMsg`.
- [ ] Delete `ui/chat_view/tool_runner.rs` (283 lines) and `ui/chat_view/prompt_builder.rs` (147 lines) — replaced by `src/agent/`.
- [ ] Split `ChatViewModel::update` (`src/ui/chat_view/mod.rs`, currently **619 lines** total / **245-line** `update` method) into focused private handler methods (`on_load_conversation`, `on_page_loaded`, `on_refresh_models`, etc.) so `chat_view/mod.rs` is `<= 350` lines.
- [ ] Replace duplicated `std::thread::scope` chunking in `src/ui/chat_view/page_loader.rs:89-118` with `crate::core::parallel_chunks`.

### 4.3 Tests & Verification
- [ ] Implement `MockModelProvider` (scripted multi-step `ChatEvent` sequences) and `MockApprovalHandler` (scripted allow/deny/edited-query responses) in `src/agent/runner.rs` `#[cfg(test)]`.
- [ ] Add end-to-end headless unit tests for `AgentRunner`:
  - Single-turn streaming response + reasoning + usage persistence.
  - Multi-step tool-call loop (model emits `<tool_call>` → tool executes → `<tool_result>` fed back → final answer saved).
  - User denies tool call → `"User denied permission."` fed back cleanly, tool not executed.
  - Cancellation mid-stream removes/skips empty output and stops cleanly without persisting partial junk.
  - `MAX_AGENT_STEPS` cap stops infinite tool loops gracefully.
  - Web search privacy warnings forwarded to `ApprovalHandler`, and user-edited query is what executes.
- [ ] Add unit tests for `build_prompt_turns` (granted folders, empty folders, web search toggle, image vs. text attachments, mentioned OS paths).
- [ ] **Verification:** All files in `src/ui/chat_view/` are `<= 350` lines; `AgentRunner` has full headless test coverage; `cargo test` passes.

---

## Refactor Sprint 5: Extract `DownloadManager` & Eliminate UI Closure/Parameter Soup

**Goal:** Move download lifecycle management out of `ui/hf_browser/` into `src/models/`, remove global mutable download state, eliminate all 12 `#[allow(clippy::too_many_arguments)]` suppressions, and bring every UI module under the 400-line / 50-line guidelines.

### 5.1 Extract `DownloadManager` into `src/models/download_manager.rs`
Currently download state, workers, and cancellation live inside `src/ui/hf_browser/` (`download_state.rs`, `download_worker.rs`, `download_tracker.rs`) and rely on a global `static ACTIVE_DOWNLOADS: LazyLock<Mutex<...>>`.

- [ ] Create `src/models/download_manager.rs` defining an injectable, cloneable `DownloadManager`:
  - Owns `active: Arc<Mutex<HashMap<DownloadKey, Arc<Mutex<ActiveDownloadEntry>>>>>`, `db: Database`, `models_dir: PathBuf`, and a pluggable `Fetcher` trait (defaulting to Hugging Face `download_gguf_file`, stubbable in tests).
  - Clean public API:
    - `start_download(&self, req: DownloadRequest) -> DownloadHandle`
    - `pause(&self, key: &DownloadKey)`
    - `resume(&self, key: &DownloadKey)`
    - `cancel(&self, key: &DownloadKey) -> Result<()>`
    - `restore_pending(&self) -> Vec<DownloadHandle>`
    - `subscribe(&self, key: &DownloadKey) -> Option<broadcast::Receiver<DownloadSnapshot>>`
    - `active_downloads(&self) -> Vec<DownloadSummary>`
- [ ] Unify `mmproj` vision downloads (`ui/model_manager/vision_download.rs`, **312 lines**) to use `DownloadManager` instead of duplicating progress/pause/cancel/part-file wiring!
- [ ] Pass `DownloadManager` from `MainWindow` into `present_settings_dialog` → `model_manager` / `hf_browser`.
- [ ] Delete global `ACTIVE_DOWNLOADS` static and `ui/hf_browser/download_worker.rs`.

### 5.2 Refactor Imperative UI Builders into Cohesive Structs / Relm4 Components
Eliminate `Rc<RefCell<Option<Rc<dyn Fn()>>>>` callback slots and 8–12 parameter functions:
- [ ] **`src/ui/model_manager/`:**
  - Replace `build_models_page` (294 lines) and `build_refresh_installed_callback` (136 lines) in `model_manager/mod.rs` (537 lines) with a `ModelManagerView` struct (or Relm4 component) holding its widgets, `db`, `download_manager`, `hw`, and state fields — removing 6 `#[allow(clippy::too_many_arguments)]` annotations across `mod.rs`, `active_details.rs`, `vision_setup.rs`, and `vision_download.rs`.
- [ ] **`src/ui/hf_browser/`:**
  - Replace `attach_entry_to_tracker` (182 lines, 9 args) and `repo_rows.rs` (2 `too_many_arguments` functions) with a `HfBrowserContext` struct + small helper methods.
  - Break `build_starter_model_row` (`starter_models.rs`, 225 lines) into focused sub-builders (`build_starter_header`, `wire_starter_download_button`, `apply_starter_state`).
- [ ] **Oversized dialogs (`<= 400` lines each, `<= 50` lines per builder function):**
  - Split `src/ui/message_dialogs.rs` (525 lines) into `src/ui/message_dialogs/` (`mod.rs`, `edit_dialog.rs`, `info_dialog.rs`).
  - Refactor `src/ui/folder_permissions_dialog.rs` (416 lines, `populate_grant_group` 115 lines + `too_many_arguments`) into a `FolderPermissionsDialog` struct holding `db`, `conv_id`, `toast_overlay`, and `on_changed` (passing `&self` instead of 9 arguments).
  - Refactor `src/ui/chat_controls_dialog.rs` (302 lines, 285-line function + `too_many_arguments`) into a `ChatControlsConfig` struct + small section builders (`build_model_section`, `build_prompt_section`, `build_thinking_section`).
  - Refactor `src/ui/model_config_dialog.rs` (261 lines, 248-line function) into `ModelConfigDialog` struct + pure `ModelConfigFormState` (dirty/default calculation extracted and unit-tested).
- [ ] **Trim remaining large Relm4 components:**
  - `src/ui/sidebar.rs` (473 lines, 136-line `update`): extract handler methods & `sidebar/search.rs` or `sidebar/actions.rs`.
  - `src/ui/composer/mod.rs` (470 lines, 145-line `update`, 94-line `init`): extract `composer/actions.rs` or per-message handler methods.

### 5.3 Tests & Verification
- [ ] Add unit tests for `DownloadManager` with a mock `Fetcher`: start → progress → pause → resume → complete (verifying DB registration + pending removal), and cancel while active vs. cancel while paused (verifying `.part` cleanup).
- [ ] Add unit tests for `ModelConfigFormState` dirty/reset/save logic.
- [ ] **Verification:** `grep -rn 'too_many_arguments' src/` returns **0 matches**; every file in `src/ui/` is `<= 400` lines; `cargo test` passes and Settings → Models / Explore / Vision / Dialogs work smoothly in `cargo run`.

---

## Refactor Sprint 6: Decompose Parsers & Inference, Workspace Split & Quality Gates

**Goal:** Refactor the remaining oversized pure-logic functions (`markdown.rs`, `llama_cpp/runner.rs`, `llama_cpp/prompt.rs`), fix latent edge-case bugs, lock in module boundaries via a Cargo workspace, and enforce quality automatically in CI.

### 6.1 Decompose `src/core/markdown.rs` (588 lines → folder module)
- [ ] Split into `src/core/markdown/`:
  - `mod.rs` — public API (`MarkdownBlock`, `CodeHighlightCache`, `parse_markdown_blocks*`).
  - `parser.rs` — `MarkdownBuilder` struct replacing the 327-line `parse_markdown_blocks_impl` and its 15 local mutable variables, with clean methods (`push_event`, `handle_code_block`, `handle_table`, `handle_inline`, `finish`).
  - `highlight.rs` — `syntect` syntax resolution, Pango span emitter, and `format_code_language`.
- [ ] **Fix latent bugs in `markdown.rs`:**
  - Respect `skip_unclosed_highlight` in the EOF unclosed-code-block fallback (currently lines 359–371 always run `highlight_code_pango`, ignoring the streaming flag).
  - Replace panicking `&THEME_SET.themes[theme_name]` with `.get(theme_name)` + safe fallback to plain escaped text.

### 6.2 Decompose `src/inference/llama_cpp/` & Improve Testability
- [ ] **Extract `StopSequenceFilter` (`src/inference/stop_filter.rs`):**
  - Move the stop-sequence holdback buffer out of `runner.rs:210-238` into a pure stateful struct `StopSequenceFilter` (mirroring `ThinkStreamParser`) with `push(&mut self, piece: &str) -> StopOutput` and `finish(&mut self) -> String`.
- [ ] **Decompose `run_inference_blocking` (`src/inference/llama_cpp/runner.rs`, 268 lines):**
  - Extract `compute_thread_config`, `eval_text_prompt`, `eval_multimodal_prompt`, `build_sampler`, and `run_decode_loop` so each step is <= 50 lines and readable top-to-bottom.
- [ ] **Decouple prompt formatting from `LlamaModel` (`src/inference/llama_cpp/prompt.rs`, 173 lines):**
  - Extract pure `enum ChatTemplateFamily { Gemma4, QwenThinking, Generic }` + `prepare_turns(...)` + `format_gemma4_prompt(...)` + `format_chatml_fallback(...)` that take `&str` template source instead of `&LlamaModel`.
  - Keep `LlamaModel::apply_chat_template` as a thin 15-line wrapper, unlocking direct unit tests for Gemma 4, Qwen thinking/no-think, vision markers, and ChatML formatting without loading a GGUF.
- [ ] **Small fixes in `inference/llama_cpp/`:**
  - Wrap `"Loading model…"` (`llama_cpp/mod.rs:82`) and `"Generating…"` (`runner.rs:30`) in `crate::i18n::gettext(...)`.
  - Protect `LlamaBackend::init()` in `get_backend()` (`llama_cpp/mod.rs:23-31`) with a `Mutex` or `OnceLock` init guard so concurrent callers can never double-init `llama_backend_init`.
  - Replace the two `.expect("loaded model mutex poisoned")` calls with `Result` propagation.

### 6.3 Cargo Workspace Split (Compiler-Enforced Boundaries)
- [ ] Convert root `Cargo.toml` into a Cargo workspace with focused crates matching the DAG:
  - `crates/companion-core` (no GTK, no SQLite, no `llama-cpp-2`)
  - `crates/companion-storage` (depends on `companion-core`, `rusqlite`)
  - `crates/companion-models` (depends on `companion-core`, `companion-storage`, `reqwest`)
  - `crates/companion-agent` (depends on `companion-core`, `companion-storage`, `reqwest`)
  - `crates/companion-inference` (depends on `companion-core`, `llama-cpp-2`)
  - `crates/companion-app` (GTK4 / Relm4 / libadwaita UI binary wiring everything together)
- [ ] Update `meson.build`, `build-aux/`, and `po/POTFILES.in` paths accordingly.
- [ ] Verify headless test suite runs in seconds without GTK4 or C++/Vulkan headers:
  ```bash
  cargo test -p companion-core -p companion-storage -p companion-models -p companion-agent
  ```

### 6.4 Automated Quality Gates
- [ ] Add workspace-level `[workspace.lints.clippy]` in root `Cargo.toml` (enforcing `unwrap_used = "deny"` in non-test code, `too_many_arguments = "warn"`, `cognitive_complexity`).
- [ ] Add a lightweight CI check script (or GitHub Actions workflow) verifying:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace`
  - Optional line-count check alerting when any `.rs` file crosses 400 lines.

### 6.5 Tests & Verification
- [ ] Add unit tests for `StopSequenceFilter` (exact match, split across 2–3 UTF-8 token chunks, multibyte UTF-8 boundary safety, partial match flushed at EOF).
- [ ] Add unit tests for `markdown::parser` covering streaming EOF unclosed code blocks and unknown theme fallback.
- [ ] Add unit tests for Gemma 4, Qwen (`/think` & `/no_think`), ChatML fallback, and vision-marker prompt formatting.
- [ ] **Verification:** Full `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`, and manual end-to-end smoke test of chat streaming, tool calling, permissions, web search, model switching, and downloads via `cargo run`.

---

## Sprint Progress Tracker

| Sprint | Focus Area | Status | Key Outcome |
|---|---|---|---|
| **Sprint 1** | Pure Domain Types & Zero-GTK Core | `[x]` Done | Zero `glib` in domain layers; typed `ModelParams` & enums |
| **Sprint 2** | Storage Hardening & Responsibility Split | `[ ]` Planned | Atomic migrations, zero DB `.expect()`, pure SQL repos |
| **Sprint 3** | `ToolRegistry` & Security Fixes | `[ ]` Planned | Extensible tool system; SSRF & write-escalation bugs fixed |
| **Sprint 4** | Extract `AgentRunner` from `chat_view` | `[ ]` Planned | Full headless agent loop tests; thin Relm4 chat renderer |
| **Sprint 5** | `DownloadManager` & UI Cleanup | `[ ]` Planned | Zero `too_many_arguments`; injectable downloads; all UI files `<= 400` LOC |
| **Sprint 6** | Parsers/Inference, Workspace & CI Gates | `[ ]` Planned | Compiler-enforced crate DAG; headless `cargo test`; lint gates |
