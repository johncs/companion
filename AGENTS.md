# Companion — Engineering & Agent Guidelines

**Companion** is a native GNOME AI agent application written in Rust (`Relm4` + `GTK4` + `libadwaita` + embedded `llama.cpp` + `SQLite`) organized as a 6-crate Cargo workspace.

All human contributors and AI coding agents **must** uphold these invariants. Prefer small, algebraic, composable, headless-testable building blocks over quick patches.

---

## 1. Build, Test & Mandatory Verification Gate
Requires Rust (stable) and native dev libraries (`GTK4`, `libadwaita`, `SQLite`, `pkg-config`, `cmake`, C/C++ compiler). Inside Fedora `toolbox`, prepend `export PATH=/usr/bin:$PATH`.

Before concluding **any** task, run and pass the full CI gate:
```bash
./scripts/ci-check.sh
```
This **fails hard** on:
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings` (workspace denies `unwrap_used` and `expect_used`)
- `cargo test --workspace`
- Any `.rs` file **> 400 lines**
- Any **`unsafe`** block anywhere in `crates/`
- Any **`po/POTFILES.in` drift** (missing entries or UI files using `gettext`/`tr_f` not listed in `po/POTFILES.in`)
- Any **`Rc<RefCell<Option<Rc<dyn Fn...` callback web** in `crates/`
- Any **`thread::sleep(` / `time::sleep(`** inside test files or `#[cfg(test)]` modules

For fast headless iteration on domain/logic crates (no GTK4 or `llama.cpp` build required):
```bash
cargo test -p companion-core -p companion-storage -p companion-models -p companion-agent
```
*(Use `cargo run -p companion-app` to smoke-test UI changes.)*

---

## 2. Strict Workspace Architecture (Acyclic DAG)

Crate dependencies are compiler-enforced in `Cargo.toml` and **must only flow downward**:

```text
companion-app        -> core, storage, models, agent, inference   (GTK4 / Relm4 / libadwaita UI binary ONLY)
companion-agent      -> core, storage                             (AgentRunner, Tool/ToolRegistry, PermissionEngine, web_search, coalescer, prompt, title_gen)
companion-models     -> core, storage                             (DownloadManager, ModelRegistryService, GGUF metadata, HfClient, hardware probe)
companion-inference  -> core                                      (LlamaCppProvider, ThinkStreamParser, StopSequenceFilter, prompt templates/budget)
companion-storage    -> core                                      (Pure SQLite CRUD, row mapping & atomic schema migrations ONLY)
companion-core       -> (no internal deps)                        (Domain enums, ModelProvider trait, ModelParams, AppPaths, Markdown, prompt rules, i18n)
```

### Non-Negotiable Boundary Laws
1. **Zero `gtk` / `glib` / `adw` / `relm4` outside `crates/companion-app`:**
   - Inject OS paths (`AppPaths`) and platform defaults (e.g. OS real name) from `crates/companion-app/src/main.rs`.
   - Use pure-Rust helpers (e.g. `companion_core::markdown::escape_pango_markup`) in non-UI crates.
2. **Zero Business Orchestration in `crates/companion-app/src/ui/`:**
   - Agent loops, tool authorization/dispatch, prompt assembly, folder grant matching, download lifecycles, and disk sync live in `companion-agent` or `companion-models` — **never** in Relm4 components or dialog callbacks.
   - `companion-app` is a thin presentation shell: render state, forward user intent, and implement UI adapters (`GtkApprovalHandler`).
3. **Zero Business Logic in `crates/companion-storage/`:**
   - Only SQL queries, row mapping, and versioned schema migrations — never prompt templates, filesystem scanning, or domain heuristics.
4. **No Process-Global Mutable State or Implicit Singletons in Library Crates:**
   - Never introduce `static LazyLock<Mutex<...>>` or `OnceLock<reqwest::Client>` registries inside library crates.
   - Inject `tokio::runtime::Handle`, `HttpTransport`, `HfClient`, `GgufMetadataCache`, `DownloadManager`, `AgentRunner`, and `PermissionEngine` so tests instantiate isolated copies with mocks.

---

## 3. Code Structure, Modularity & Rust Standards

### Hard Budgets & Lints
- **Files:** **<= 400 lines** per `.rs` file (CI-enforced). Split into a folder module (`mod.rs` + focused submodules) **in the same change** — never defer.
- **Functions:** **<= 50 lines** per non-test function. Decompose multi-step algorithms into structs with small methods (`InferenceRun`, `MarkdownBuilder`, `StreamCoalescerState`, `ThinkStreamParser`, `StopSequenceFilter`).
- **Zero `unsafe`:** Never use `unsafe` (CI-enforced). For GTK widget state/lifetimes, use safe `#[glib::object_subclass]` types (see `message_row/widget.rs`, `message_object.rs`) — never `set_data`/`data`.
- **Zero `.unwrap()` / `.expect()` in non-test code:** Both are workspace-denied by clippy. Propagate errors via `anyhow::Result` and surface them in the UI.
- **Zero `#[allow(...)]` suppressions in non-test code:** Do not suppress `too_many_arguments`, `type_complexity`, `dead_code`, or `expect_used`. Group arguments into typed config structs (`SettingsDialogConfig`, `InferenceRun`) or methods.

### Required Design Patterns
- **Algebraic State Machines Over Boolean Bags:** Represent mutually exclusive lifecycles with Rust enums, never multi-boolean structs.
  - *Exemplars:* `DownloadStatus` (`download_manager/state.rs`), `BlockState` (`markdown/parser.rs`), `ToolAuthorization` (`agent/tool.rs`).
- **Strongly-Typed Domain Models End-to-End:** Keep roles, statuses, permissions, devices, and settings as Rust enums (`MessageRole`, `ThinkingLevel`, `ExecutionDevice`, `FolderPermission`) all the way into UI models (`MessageEntry`) — never stringify and re-parse. Deserialize JSON blobs (`ModelParams`) once at the storage boundary.
- **True Polymorphic Tools (`Tool` + `ToolRegistry`):** Every tool (file, web, or MCP) must be its own struct in `crates/companion-agent/src/tools/` with typed `#[derive(Deserialize)]` args (using `#[serde(alias = "...")]`), implementing `Tool::prepare()` → `ToolAuthorization` and `Tool::execute_approved()`. Never add `match tool_name` switches or special-case tool names in `AgentRunner`.
- **Sequence / Generation Tokens on Async State:** Every async UI request or background run that writes back to shared state must carry a monotonic guard (`run_id` in `ActiveChatRun`, `request_seq` / `search_seq` in `ChatViewModel`/`SidebarModel`/`HfBrowser`, `attempt_id` in `DownloadManager`) so cancelled or slow operations can never clobber newer state.
- **Never Swallow Persistence or Registration Errors:** Never write `let _ = db.insert_...` or `.ok()?` to hide DB/model-registration failures. Log with `tracing::error!` and propagate via `AgentEvent::Error`, `DownloadStatus::Failed`, or `AdwToast`. Keep live UI records synced with persisted DB IDs and timestamps.
- **Pure View-Models & Form State in `companion-app`:** Extract non-trivial dialog/page state, dirty tracking, and formatting into pure, GTK-free unit-testable structs.
  - *Exemplars:* `ModelConfigFormState` (`model_config_dialog.rs`), `ChatControlsFormState` (`chat_controls_dialog/form_state.rs`), `ActiveModelViewModel` (`model_manager/view_model.rs`).

### Red Flags — Stop & Refactor Immediately
| If you catch yourself writing… | Do this instead |
| --- | --- |
| `Rc<RefCell<Option<Rc<dyn Fn()>>>>` | Use a Relm4 component, typed channel, or controller method (`DownloadControls::bind_download`) |
| `match tool_name { ... }` or `if call.name == "..."` | Implement a dedicated `Tool` struct with typed args and `Tool::prepare()` |
| `is_paused: bool, finished: bool, cancelled: bool` | Collapse into an explicit algebraic `enum` |
| `unsafe { obj.set_data(...) }` | Define a safe `#[glib::object_subclass]` widget (`MessageRowWidget`) |
| `companion_core::runtime().spawn(...)` inside a library crate | Make it `async fn` or accept an injected `tokio::runtime::Handle` |
| `std::thread::sleep` / `tokio::time::sleep` in a test | Use `tokio::time::pause()`, `Notify`/oneshot channels, or explicit timestamps |

---

## 4. Testability & Deterministic Testing
- Keep every workflow headless-testable without GTK or a live GGUF model via small trait seams (`ModelProvider`, `Tool`, `ApprovalHandler`, `ModelFetcher`, `HttpTransport`).
- Unit tests in `core`, `storage`, `models`, `agent`, `inference`, and UI form/view-model tests must run with zero display or live network access.
- **Zero `sleep` in tests:** Never rely on wall-clock sleeps to coordinate async tasks or generate distinct timestamps.
- Every bug fix must add a deterministic regression test locking in the invariant.

---

## 5. Security, Privacy & Data Integrity Invariants
Never weaken these local-first agent guarantees:

1. **Least-Privilege Path & Folder-Scoped Permissions:**
   - Every file tool resolves canonical paths through `path_guard` and rejects `..` traversal or symlink escapes (resolving non-existent write targets via nearest existing ancestor).
   - `PermissionEngine` write grants are strictly scoped to `(conv_id, canonical_folder_path, tool_name)`. Approving `AllowOnce` or `AllowChat` must **never** escalate unrelated read-only folders.
   - Destructive tools (`delete_file`, `delete_directory`) always require explicit per-action confirmation.
2. **DNS-Pinned SSRF & Privacy Guards on All Network Tools:**
   - `fetch_url` validates HTTPS scheme + public IP on **every** redirect hop and pins the validated `SocketAddr` via `HttpTransport::fetch_pinned_hop` (`web_search/transport.rs`) to prevent DNS rebinding.
   - Always run `privacy_guard` pre-flight leak checks before presenting web search or URL fetch approvals.
3. **Atomic, Race-Free SQLite Storage:**
   - Centralize locking via `Database::with_conn` / `Database::with_tx`.
   - Wrap each schema migration `N` together with `PRAGMA user_version = N` in its own transaction (`crates/companion-storage/src/schema/{mod,migrations}.rs`).
   - Use normalized tables (never read-modify-write JSON arrays in `app_settings`) and preserve all columns/child records in operations like `duplicate_conversation`.
4. **Privacy & Portability:** Zero hardcoded personal paths (`/home/<user>`), secrets, or machine-specific assumptions in code, tests, or docs.

---

## 6. GNOME HIG, Async Responsiveness & Localization
1. **Modern `libadwaita` & Virtual Scrolling:**
   - Use `AdwNavigationSplitView`, `AdwOverlaySplitView`, `AdwPreferencesDialog`, `AdwAlertDialog`, `AdwStatusPage`, `AdwBanner`, `AdwToastOverlay`.
   - Chat messages must live in `gtk::ListView` (`gio::ListStore` + `SignalListItemFactory`), never `gtk::ListBox` or a `gtk::Box`.
   - Escape **all** dynamic strings (paths, filenames, repo IDs, model names, errors) with `escape_pango_markup` / `glib::markup_escape_text` before passing them to Pango-parsed `AdwActionRow`, `AdwExpanderRow`, `AdwComboRow`, or `AdwPreferencesGroup` titles/subtitles.
   - Never put trailing ellipses (`…` or `...`) in button labels; icon-only buttons must set `tooltip_text`; dialogs must support keyboard navigation (`Tab`, `Esc`, `Enter`).
2. **Never Block the GTK Main Thread:**
   - Offload SQLite queries/writes, filesystem `exists()`/scans/deletions, GGUF header parsing, prompt path scanning, hardware probing, PDF/image decoding, inference, and HTTP requests via `spawn_blocking_ui` or injected `tokio::runtime::Handle`.
   - Use bounded LRU caches for decoded textures (keyed by path + file metadata) and cache completed `syntect` highlights; throttle streaming UI updates (`~80 ms`) and debounce search (`~150 ms`).
   - Show non-modal feedback (`Spinner`/`ProgressBar`/`Banner`) for operations > 100 ms; clean up partial/empty placeholders on failure or cancellation; surface errors via `AdwToast` or inline **Retry** rows.
3. **Icons & Localization:**
   - Centralize custom symbolic icon names in `crates/companion-app/src/ui/icons.rs` and `data/meson.build`.
   - Wrap all user-facing strings (including status/error messages) in `companion_core::i18n::gettext` / `tr_f` and keep `po/POTFILES.in` synchronized (CI-enforced).

---

## 7. Code Hygiene & Commit Format
- Remove unused code, obsolete CSS, and stale `po/POTFILES.in` entries before finishing. Write concise *why* comments for non-obvious invariants or concurrency rules — avoid narrating obvious code.
- Use **Conventional Commits**:
  ```text
  <type>(<optional-scope>): <short imperative summary>
  ```
  - **Types:** `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `chore`
  - **Scopes:** `ui`, `chat_view`, `composer`, `agent`, `inference`, `models`, `storage`, `core`, `i18n`
