# Companion — Engineering & Agent Guidelines

**Companion** is a native GNOME AI agent application written in Rust (`Relm4` + `GTK4` + `libadwaita` + embedded `llama.cpp` + `SQLite`) organized as a **7-crate Cargo workspace**.

All human contributors and AI coding agents **must** uphold these invariants. Prefer small, algebraic, composable, headless-testable building blocks over quick patches.

---

## 1. Build, Test & Mandatory Verification Gate
Requires Rust (stable) and native dev libraries (`GTK4`, `libadwaita`, `SQLite`, `pkg-config`, `cmake`, C/C++ compiler). Inside Fedora `toolbox`, prepend `export PATH=/usr/bin:$PATH`.

Before concluding **any** task, run and pass the full CI gate (also enforced by `.github/workflows/ci.yml`):
```bash
./scripts/ci-check.sh
```
This **fails hard** on:
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings` (workspace denies `unwrap_used` and `expect_used`)
- `cargo test --workspace`
- Any `.rs` file **> 400 lines**
- Any non-test function **> 50 lines**
- Any **`unsafe`** block anywhere in `crates/`
- Any non-test **`#[allow(...)]`** suppression anywhere in `crates/`
- Any **`po/POTFILES.in` drift** across the entire workspace (missing files using `gettext`/`ngettext`/`tr_f`, missing paths, or stale entries with no translation calls)
- Any **`Rc<RefCell<Option<Rc<dyn Fn...` callback web** in `crates/`
- Any **`thread::sleep(` / `time::sleep(`** inside test files or `#[cfg(test)]` modules
- Any **`std::fs`** call outside `#[cfg(test)]` inside `crates/companion-app/src/ui/`
- Invalid `.desktop` or `.metainfo.xml` files (`desktop-file-validate`, `appstreamcli validate --no-net --explain` when installed)

For fast headless iteration on domain/logic crates (no GTK4 or `llama.cpp` build required):
```bash
cargo test -p companion-core -p companion-format -p companion-storage -p companion-models -p companion-agent
```
*(Use `cargo run -p companion-app` to smoke-test UI changes.)*

---

## 2. Strict Workspace Architecture (Acyclic DAG)

Crate dependencies are compiler-enforced in `Cargo.toml` and **must only flow downward**:

```text
companion-app        -> core, format, storage, models, agent, inference   (GTK4 / Relm4 / libadwaita UI binary + AppServices)
companion-agent      -> core, format, storage                             (AgentRunner, Tool/ToolRegistry, PermissionEngine, session prep/paging, web_search, coalescer, prompt, title_gen)
companion-models     -> core, storage                                     (DownloadManager, ModelRegistryService, GGUF metadata, HfClient, inventory, discovery, naming, hardware probe)
companion-inference  -> core                                              (LlamaCppProvider, TokenStreamPipeline, ThinkStreamParser, StopSequenceFilter, prompt templates/budget)
companion-storage    -> core                                              (Pure SQLite CRUD, StorageError, row mapping & atomic schema migrations ONLY)
companion-format     -> core                                              (Pango Markdown/syntect rendering, bounded file/PDF attachment extraction)
companion-core       -> (no internal deps)                                (Pure domain types, newtype IDs, ModelProvider/ChatRequest/ProviderError, ModelParams, AppPaths, prompt rules, i18n)
```

### Non-Negotiable Boundary Laws
1. **Zero `gtk` / `glib` / `adw` / `relm4` outside `crates/companion-app`:**
   - Inject OS paths (`AppPaths`) and platform defaults (e.g. OS real name) from `crates/companion-app/src/main.rs`.
   - Use pure-Rust helpers (e.g. `companion_format::markdown::escape_pango_markup`) in non-UI crates.
2. **Keep `companion-core` Minimal and Pure:**
   - No Tokio runtime singletons, no Markdown/HTML/PDF parsers, no SQLite, and no filesystem/network I/O. Only pure domain types, traits, IDs, prompt rules, and `i18n`.
3. **Zero Business Orchestration or `std::fs` in `crates/companion-app/src/ui/`:**
   - Agent loops, prompt/session preparation (`companion-agent/src/session.rs`), paginated message assembly (`session_page.rs`), tool authorization/dispatch, folder grant matching, model inventory/deletion (`companion-models/src/inventory.rs`), storage sizing, and download lifecycles live in library crates — **never** in Relm4 components or dialog callbacks.
   - `companion-app` is a thin presentation shell: render state, forward user intent, and implement UI adapters (`GtkApprovalHandler`).
4. **Zero Business Logic in `crates/companion-storage/`:**
   - Only SQL queries, row mapping, `StorageError`, and versioned schema migrations — never prompt templates, English UI strings (like `"(Copy)"`), filesystem scanning, or domain heuristics.
   - Keep `with_conn` and `with_tx` `pub(crate)` — never leak `rusqlite` types through the public API.
5. **Single Shared Service Graph (`AppServices`) — Zero Implicit Singletons or Ad-Hoc Clients:**
   - Never introduce `static LazyLock<Mutex<...>>`, `OnceLock<reqwest::Client>`, or global runtimes inside library crates.
   - Never add convenience free functions that allocate a fresh `reqwest::Client` (`HfClient::new()` / `WebSearchService::new()`) or discard caches (`GgufMetadataCache::new()`).
   - Construct the single Tokio runtime and shared `AppServices` (`db`, `rt_handle`, `paths`, `provider`, `download_manager`, `hf_client`, `web_search`, `gguf_cache`) once in `crates/companion-app/src/services.rs` and inject handles downward so tests instantiate isolated copies with mocks.

---

## 3. Code Structure, Modularity & Rust Standards

### Hard Budgets & Lints (All CI-Enforced)
- **Files:** **<= 400 lines** per `.rs` file. Split into a folder module (`mod.rs` + focused submodules) **in the same change** — never defer.
- **Functions:** **<= 50 lines** per non-test function. Decompose multi-step algorithms into structs with small methods (`InferenceRun`, `TokenStreamPipeline`, `MarkdownBuilder`, `StreamCoalescerState`, `ThinkStreamParser`, `StopSequenceFilter`).
- **Zero `unsafe`:** Never use `unsafe`. For GTK widget state/lifetimes, use safe `#[glib::object_subclass]` types (`message_row/widget.rs`, `message_object.rs`) — never `set_data`/`data`.
- **Zero `.unwrap()` / `.expect()` in non-test code:** Workspace-denied by clippy.
- **Zero `#[allow(...)]` suppressions in non-test code:** Never suppress `too_many_arguments`, `type_complexity`, `dead_code`, or `expect_used`. Group arguments into typed config/context structs (`ChatRequest`, `PromptPrepInput`, `InferenceRun`) or methods.

### Required Design Patterns
- **Typed Domain Errors (`thiserror`) at Crate Boundaries:**
  - Define and return structured `thiserror` error enums at crate boundaries (`ProviderError`, `StorageError`, `AttachmentError`, `DownloadError`, `GgufError`) so callers and UI can branch cleanly and present localized messages. Reserve `anyhow::Result` for top-level orchestration/glue where rich context suffices.
- **Algebraic State Machines Over Boolean Bags:**
  - Represent mutually exclusive lifecycles with Rust enums, never multi-boolean structs.
  - *Exemplars:* `DownloadStatus` (`download_manager/state.rs`), `BlockState` (`companion-format/src/markdown/parser.rs`), `ToolAuthorization` (`companion-agent/src/tool.rs`).
- **Strongly-Typed Domain Models & Newtype IDs End-to-End:**
  - Keep roles, statuses, permissions, devices, and settings as Rust enums (`MessageRole`, `ThinkingLevel`, `ExecutionDevice`, `FolderPermission`, `SearchEngine`) all the way into UI models (`MessageEntry`) — never stringify and re-parse.
  - Use newtype IDs (`ConversationId`, `MessageId`, `ModelId` in `companion-core/src/ids.rs`), `InferenceParams` composed around `ModelParams`, and `ChatRequest`. Deserialize JSON blobs (`ModelParams`) once at the storage boundary.
- **True Polymorphic, MCP-Ready Tools (`Tool` + `ToolRegistry`):**
  - Every tool (file, web, or external MCP) must implement `Tool` with `Cow<'static, str>` names (`HashMap<String, Arc<dyn Tool>>`), declarative `ToolMetadata` (single source of truth for prompt generation and Settings UI), and single-pass validation: `Tool::prepare()` parses and validates args once into a `PreparedToolCall` (returning `Rejected` on malformed args before prompting) and `Tool::execute_approved()` consumes it.
  - Never add `match tool_name` switches or special-case tool names in `AgentRunner` or UI dialogs; inject dependencies (e.g. `Arc<WebSearchService>`) into tool structs.
- **Sequence / Generation Tokens on Async State:**
  - Every async UI request or background run that writes back to shared state must carry a monotonic guard (`run_id` in `ActiveChatRun`, `request_seq` / `search_seq` / `load_seq` in `ChatViewModel`/`SidebarModel`/`HfBrowser`, `attempt_id` in `DownloadManager`, active model ID in vision setup) so cancelled or slow operations never clobber newer state.
- **Never Swallow Persistence, Registration, or Worker Errors:**
  - Never write `let _ = db.insert_...`, `let _ = db.upsert_...`, or `.ok()?` to hide DB/model/worker failures. Log with `tracing::error!`/`tracing::warn!`, propagate via typed errors / `AgentEvent::Error` / `DownloadStatus::Failed` / `AdwToast`, and roll back optimistic UI controls on failure.
- **Pure View-Models & Form State in `companion-app`:**
  - Extract non-trivial component/dialog state, dirty tracking, and formatting into pure, GTK-free unit-testable structs.
  - *Exemplars:* `ChatViewState` (`chat_view/state.rs`), `ComposerViewModel` (`composer/view_model.rs`), `SidebarViewModel` (`sidebar/view_model.rs`), `MessageInfoViewModel` (`message_dialogs/view_model.rs`), `ActiveModelViewModel` (`model_manager/view_model.rs`), `ModelConfigFormState` (`model_config_form_state.rs`), `ChatControlsFormState` (`chat_controls_dialog/form_state.rs`), `folder_permissions_dialog/view_model.rs`.

### Red Flags — Stop & Refactor Immediately
| If you catch yourself writing… | Do this instead |
| --- | --- |
| `Rc<RefCell<Option<Rc<dyn Fn()>>>>` | Use a Relm4 component, typed channel, or controller method (`DownloadControls::bind_download`) |
| `match tool_name { ... }` or `if call.name == "..."` | Implement a `Tool` struct with `ToolMetadata`, `PreparedToolCall`, and `Tool::prepare()` |
| `is_paused: bool, finished: bool, cancelled: bool` | Collapse into an explicit algebraic `enum` |
| `unsafe { obj.set_data(...) }` | Define a safe `#[glib::object_subclass]` widget (`MessageRowWidget`) |
| Strong `dialog.clone()` inside a child button/controller closure | Use `dialog.downgrade()` or `build_dialog_scaffold` (`ui/dialog_scaffold.rs`) |
| Moving a channel `Sender` into its own `spawn_local` receiver loop | Keep `Sender` owned externally and close/cancel on `dialog.connect_closed` |
| `while let Ok(v) = broadcast_rx.recv().await` | Handle `RecvError::Lagged(_)` by resyncing latest state; break only on `Closed` |
| `HfClient::new()` / `WebSearchService::new()` / `GgufMetadataCache::new()` in UI or tools | Inject the shared instance from `AppServices` |
| `std::fs::read(...)` or `resp.bytes().await` without size limits | Cap reads via `Read::take(MAX_BYTES)` / streaming chunk byte counters |
| `std::thread::sleep` / `tokio::time::sleep` in tests or inference decode loops | Use `tokio::time::pause()`, `Notify`/oneshot channels, or explicit timestamps |

---

## 4. Testability & Deterministic Testing
- Keep every workflow headless-testable without GTK or a live GGUF model via small trait seams (`ModelProvider`, `Tool`, `ApprovalHandler`, `ModelFetcher`, `HttpTransport`, `TokenStreamPipeline`).
- Unit tests in `core`, `format`, `storage`, `models`, `agent`, `inference`, and UI form/view-model modules must run with zero display or live network access.
- **Zero `sleep` in tests (CI-enforced):** Never use wall-clock sleeps or busy `yield_now()` spin loops to coordinate async tasks or generate distinct timestamps. Use `tokio::time::pause()`, `Notify`/oneshot barriers, and RAII temp-dir cleanup guards.
- Every bug fix or security invariant must add a deterministic regression test locking in the behavior.

---

## 5. Security, Privacy & Data Integrity Invariants
Never weaken these local-first agent guarantees:

1. **Symlink-Safe Path Jail & Folder-Scoped Permissions:**
   - `path_guard` must inspect path components with `fs::symlink_metadata` and reject `..` traversal, existing symlink escapes, and **dangling symlink** escapes.
   - Directory walks (`fs_ops`) must never recurse into symlinks (`file_type().is_symlink()`), and delete tools must unlink symlinks rather than deleting their targets.
   - `PermissionEngine` write grants are strictly scoped to `(conv_id, canonical_folder_path, tool_name)`. Approving `AllowOnce` or `AllowChat` (including on `Destructive` tools or ancestor fallbacks) must **never** silently escalate unrelated or read-only folders.
   - Destructive tools (`delete_file`, `delete_directory`) always require explicit per-action confirmation and default to **Deny** in the UI.
2. **DNS-Pinned SSRF, Privacy Guards & Bounded I/O:**
   - `fetch_url` validates HTTPS scheme, rejects embedded URL credentials, blocks loopback/private/CGNAT/`0.0.0.0/8`/NAT64/site-local and **IPv4-mapped IPv6** (`to_ipv4_mapped()`) addresses on **every** redirect hop, and pins the validated `SocketAddr` via `HttpTransport::fetch_pinned_hop`.
   - `privacy_guard` scans the full URL (`path`, `query`, and `fragment`) and search queries for secrets, emails, paths, and local-file overlap before presenting approval dialogs.
   - All file, attachment, GGUF, and HTTP reads must enforce explicit byte/depth caps (e.g., `Read::take`, `Seek`-based GGUF skipping with recursion depth limit, HTTP connect/read timeouts, and `downloaded == total_bytes` / `ETag` verification before `.part` → `.gguf` rename).
3. **Atomic, Race-Free SQLite Storage:**
   - Centralize locking via `pub(crate)` `Database::with_conn` / `Database::with_tx`; use `prepare_cached` on hot queries and prepare statements outside loops.
   - Wrap each schema migration `N` together with `PRAGMA user_version = N` in its own transaction (`crates/companion-storage/src/schema/{mod,migrations}.rs`) and reject databases with `user_version > CURRENT_SCHEMA_VERSION`.
   - Use composite `(created_at, rowid)` keyset pagination (never `created_at < cursor` alone), verify target row existence inside `with_tx` before truncation/deletion (never `COALESCE(..., 0)`), persist assistant messages + tool calls atomically, and keep normalized tables with preserved child records on `duplicate_conversation`.
4. **Race-Free Downloads & Faithful Streaming Inference:**
   - `DownloadManager` enforces a single active worker per `DownloadKey` with an exit barrier before resuming, checks `attempt_id` on every progress/completion/error path, and sanitizes `repo_id`/`filename` against path traversal.
   - `companion-inference` decodes tokens through `TokenStreamPipeline` (`max_utf8_buffer_length` + EOF flush), trims multimodal `image_paths` in sync with `trimmed_messages`, avoids orphaned leading `Assistant` turns after budget trimming, reuses KV cache / `MtmdContext` across turns, and never sleeps per token on GPU.
5. **Privacy & Portability:** Zero hardcoded personal paths (`/home/<user>`), secrets, build-host `CARGO_MANIFEST_DIR` paths in release binaries, or unsolicited outbound network requests (e.g., don't fetch Hugging Face metadata when starter UI is hidden).

---

## 6. GNOME HIG, Async Responsiveness & Localization
1. **Modern `libadwaita`, Virtual Scrolling & Leak-Free Lifecycles:**
   - Use `AdwNavigationSplitView`, `AdwOverlaySplitView`, `AdwPreferencesDialog`, `AdwAlertDialog`, `AdwStatusPage`, `AdwBanner`, `AdwToastOverlay`, and shared `DialogScaffold` (`ui/dialog_scaffold.rs`).
   - Chat messages must live in `gtk::ListView` (`gio::ListStore` + `SignalListItemFactory`), never `gtk::ListBox` or a `gtk::Box`. Always clear transient child widgets and stop spinners in `connect_unbind`.
   - Use `dialog.downgrade()` in dialog child closures, detach broadcast/signal subscriptions in `dialog.connect_closed`, call `popover.unparent()` on owned popovers, and avoid nested vertical `ScrolledWindow`s.
   - Destructive confirmations must default to `Deny`/`Cancel` (`set_default_response(Some("deny"))`) and style the destructive action with `adw::ResponseAppearance::Destructive`.
   - Escape **all** dynamic strings (paths, filenames, repo IDs, model names, assistant names, errors) before passing them to Pango-parsed `AdwActionRow`, `AdwExpanderRow`, `AdwComboRow`, or `AdwPreferencesGroup` titles/subtitles/descriptions.
   - Use modern Libadwaita CSS custom properties (`var(--window-fg-color)`, `var(--border-color)`, `color-mix(...)`) rather than deprecated `@define-color` variables, and verify Light, Dark, and High Contrast themes.
   - Support full keyboard navigation (`Tab`, `Esc`, `Enter`, standard shortcuts `Ctrl+N`, `Ctrl+F`, `Ctrl+,`, `Ctrl+?`), preserve visible focus rings, set `tooltip_text` on icon-only buttons, never put trailing ellipses (`…` or `...`) in button labels, and set GTK4 `Accessible` labels/relations on non-obvious inputs/tiles.
2. **Never Block the GTK Main Thread:**
   - Offload SQLite queries/writes (debouncing text inputs in Preferences ~300 ms), filesystem operations (zero `std::fs` in `ui/`, CI-enforced), GGUF header parsing (via `AppServices.gguf_cache`), prompt/session prep, hardware probing, PDF/image decoding, `provider.unload()`, inference, Markdown/syntect finalization, and HTTP requests to background tasks.
   - Key decoded texture caches by `(path, mtime, file_len)` (`media_widgets.rs`), cache completed `syntect` highlights, throttle streaming UI updates (`~80 ms`), and debounce search (`~150 ms`).
   - Show non-modal feedback (`Spinner`/`ProgressBar`/`Banner`) for operations > 100 ms; clean up partial/empty placeholders on failure or cancellation; surface errors via `AdwToast` or inline **Retry** rows.
3. **Icons, Packaging & Localization:**
   - Centralize custom symbolic icon names in `crates/companion-app/src/ui/icons.rs` and `data/meson.build`; resolve them via `gtk::IconTheme` rather than direct file paths.
   - Wrap all user-facing strings (including status/error messages and tool metadata) in **string-literal** `companion_core::i18n::gettext` / `tr_f` calls (`po/meson.build` extracts `tr_f:1`; never pass variables to `gettext`), keep `po/POTFILES.in` synchronized across all crates (CI-enforced), and use `i18n.merge_file` in `data/meson.build` for `.desktop` and `.metainfo.xml`.

---

## 7. Code Hygiene & Commit Format
- Remove unused code, unused `Cargo.toml` dependencies, obsolete CSS, and stale `po/POTFILES.in` entries before finishing. Write concise *why* comments for non-obvious invariants or concurrency rules — avoid narrating obvious code.
- Use **Conventional Commits**:
  ```text
  <type>(<optional-scope>): <short imperative summary>
  ```
  - **Types:** `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `chore`
  - **Scopes:** `ui`, `chat_view`, `composer`, `agent`, `inference`, `models`, `storage`, `format`, `core`, `i18n`
