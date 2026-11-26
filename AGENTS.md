# Companion — Engineering & Agent Guidelines

**Companion** is a native GNOME AI agent application written in Rust (`Relm4` + `GTK4` + `libadwaita` + embedded `llama.cpp` + `SQLite`) organized as a multi-crate Cargo workspace.

All human contributors and AI coding agents **must** follow these invariants. Prefer small, composable, testable building blocks over quick patches.

---

## 1. Build, Test & Mandatory Verification Gate
Requires Rust (stable) and native dev libraries (`GTK4`, `libadwaita`, `SQLite`, `pkg-config`, `cmake`, and a C/C++ compiler). Inside Fedora `toolbox`, prepend `export PATH=/usr/bin:$PATH`.

Before concluding any task, run and pass the full verification script:
```bash
./scripts/ci-check.sh
```
This enforces `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and the `<= 400` lines/file budget.

For fast headless iteration on domain/logic crates (zero GTK4 or `llama.cpp` build required):
```bash
cargo test -p companion-core -p companion-storage -p companion-models -p companion-agent
```
*(Run `cargo run -p companion-app` to smoke-test UI changes.)*

---

## 2. Strict Workspace Architecture (Acyclic DAG)

Crate dependencies are compiler-enforced in `Cargo.toml` and **must only flow downward**. Never introduce upward or cyclic dependencies:

```text
companion-app        -> core, storage, models, agent, inference   (GTK4 / Relm4 / libadwaita UI binary ONLY)
companion-agent      -> core, storage                             (AgentRunner, ToolRegistry, permissions, web search, coalescer, title gen)
companion-models     -> core, storage                             (DownloadManager, ModelRegistry, GGUF metadata, HF client, hardware probe)
companion-inference  -> core                                      (LlamaCppProvider, ThinkStreamParser, StopSequenceFilter)
companion-storage    -> core                                      (Pure SQLite CRUD, row mapping & atomic schema migrations ONLY)
companion-core       -> (no internal deps)                        (Domain types/enums, ModelProvider trait, ModelParams, AppPaths, Markdown, prompt composition, i18n, runtime)
```

### Non-Negotiable Boundary Laws
1. **Zero `gtk` / `glib` / `adw` / `relm4` outside `crates/companion-app`:**
   - Never add GTK/GLib dependencies to `companion-core`, `companion-storage`, `companion-models`, `companion-agent`, or `companion-inference`.
   - Inject OS paths (`AppPaths`) and platform defaults (like OS real name) from `crates/companion-app/src/main.rs`.
   - Use pure-Rust helpers (e.g., `companion_core::markdown::escape_pango_markup`) in non-UI crates.
2. **Zero Business Orchestration in `crates/companion-app/src/ui/`:**
   - Multi-step agent loops, tool dispatch, prompt assembly, download lifecycle management, and model disk sync live in `companion-agent` or `companion-models`, **never** in Relm4 components or dialog callbacks.
   - `companion-app` is a thin presentation shell: render state, forward user intent, and implement UI adapter traits (e.g., `ApprovalHandler` via `AdwAlertDialog`).
3. **Zero Business Logic in `crates/companion-storage/`:**
   - `companion-storage` contains only SQL queries, row mapping, and schema migrations.
   - Never place prompt templates, filesystem scanning, or domain heuristics inside `companion-storage`.
4. **No Process-Global Mutable State for Workflows:**
   - Never introduce `static LazyLock<Mutex<HashMap<...>>>` registries for active jobs, downloads, or runs. Encapsulate state inside injectable service structs (`DownloadManager`, `AgentRunner`, `PermissionEngine`) so tests can instantiate isolated copies.

---

## 3. Code Structure, Modularity & Rust Standards

### Hard Size & Complexity Budgets
- **Files:** **<= 400 lines** per `.rs` file. If your edit pushes a file over 400 lines, split it into a folder module (`mod.rs` + focused submodules) **in the same change** — never defer.
- **Functions:** **<= 50 lines** per function (pure logic and handlers). Decompose multi-step algorithms into a stateful struct with small methods (e.g., `MarkdownBuilder`, `ThinkStreamParser`, `StopSequenceFilter`).
- **No `#[allow(clippy::too_many_arguments)]`:** Never suppress this lint. Group related inputs into a typed struct or convert the free function into a method on a stateful controller/view struct.
- **No `Rc<RefCell<Option<Rc<dyn Fn()>>>>` callback webs:** Model stateful pages and dialogs as Relm4 components or dedicated structs with clear methods/channels.
- **No `.unwrap()` or `.expect()` in non-test code:** Workspace clippy denies `unwrap_used`. Propagate all errors (including mutex lock failures) via `anyhow::Result` and surface them cleanly in the UI.
- **No Stringly-Typed Domain Data or Ad-Hoc JSON:**
  - Represent roles, statuses, permissions, devices, and settings as Rust enums in `companion-core` — never pass magic strings (`"read_write"`, `"allow_chat"`, `"assistant"`) across modules.
  - Deserialize persisted JSON blobs (e.g., `ModelParams`) into a single typed `serde` struct at the storage boundary; never scatter `serde_json::Value::get(...)` across callers.
- **Single Source of Truth for Tools:** Every agent tool (local file, web, or MCP) must register through `Tool` / `ToolRegistry` (`crates/companion-agent/src/tool.rs`, `registry.rs`) so its name, argument aliases, risk level, preview description, and execution live in one place.

---

## 4. Testability & Trait Seams
- **Every workflow must be testable headless without GTK or a live GGUF model:**
  - Keep I/O boundaries behind small trait seams (`ModelProvider`, `Tool`, `ApprovalHandler`, `ModelFetcher`) so core workflows (agent loop, permission decisions, download pause/resume/cancel, prompt formatting) have deterministic unit tests with mocks.
  - Keep unit tests in `companion-core`, `companion-storage`, `companion-models`, `companion-agent`, and pure `companion-inference` helpers completely free of GTK/display dependencies.
  - Avoid `std::thread::sleep` in tests whenever channels, explicit timestamps, or mocks can make assertions deterministic.

---

## 5. Security, Privacy & Data Integrity Invariants
Companion is a local-first AI agent with filesystem and optional network access. Never weaken these guarantees:

1. **Least-Privilege Path & Permission Scoping:**
   - Every file tool must resolve canonical paths through `path_guard` and reject `..` traversal or symlink escapes.
   - Granting `AllowOnce` or `AllowChat` for a write operation must apply **only** to the specific resolved target folder/file — never escalate unrelated read-only grants.
   - Destructive tools (`delete_file`, `delete_directory`) always require explicit per-action confirmation.
2. **SSRF & Privacy Guards on All Network Tools:**
   - Never auto-follow HTTP redirects on `fetch_url` before validating destination URLs and resolved IPs. Validate HTTPS scheme + public IP on **every** redirect hop.
   - Always run pre-flight privacy leak checks (`privacy_guard`) before presenting web search or URL fetch approvals.
3. **Atomic, Race-Free SQLite Storage:**
   - Centralize locking via `Database::with_conn` / `Database::with_tx` (zero `.expect()` on mutex locks).
   - Wrap each schema migration `N` together with `PRAGMA user_version = N` in its own transaction (`crates/companion-storage/src/schema.rs`).
   - Never store mutable collections as a read-modify-write JSON blob in `app_settings` — use a normalized SQLite table.
   - Never silently drop columns or child records (metrics, attachments, tool calls) in operations like `duplicate_conversation`.
4. **Privacy & Portability:** Zero hardcoded personal paths (`/home/<user>`), secrets, or machine-specific assumptions in code, tests, or docs.

---

## 6. GNOME HIG, Async Responsiveness & Error Handling
1. **Modern `libadwaita` & Virtual Scrolling:**
   - Use `AdwNavigationSplitView`, `AdwOverlaySplitView`, `AdwPreferencesDialog`, `AdwAlertDialog`, `AdwStatusPage`, `AdwBanner`, `AdwToastOverlay`.
   - Chat messages must live in `gtk::ListView` (`gio::ListStore` + `SignalListItemFactory`), never `gtk::ListBox` or a `gtk::Box` with all children instantiated.
   - Escape `&` as `&amp;` in Pango-parsed `AdwActionRow` / `AdwPreferencesGroup` titles and subtitles.
   - Never use trailing ellipses (`…` or `...`) in button labels.
   - Icon-only buttons must set `tooltip_text` / accessible labels; dialogs must support keyboard navigation (`Tab`, `Esc`, `Enter`).
2. **Icons & Localization:**
   - Centralize custom symbolic icon names in `crates/companion-app/src/ui/icons.rs` and `data/meson.build`.
   - Wrap all user-facing strings (including inference/tool status messages) in `companion_core::i18n::gettext` / `tr_f` and register new UI source files in `po/POTFILES.in`.
3. **Never Block the GTK Main Thread:**
   - Offload SQLite I/O, filesystem scans/`Path::exists()` on portal mounts, PDF/image parsing, inference, and HTTP requests to `companion_core::runtime()` (`spawn` / `spawn_blocking` / `ui::widgets::spawn_blocking_ui`).
   - Reuse shared helpers (`companion_core::parallel_chunks`, `companion_core::runtime()`, shared `reqwest::Client` pools, `ui::widgets::present_destructive_confirm`).
4. **Loading, Cleanup & Actionable Errors:**
   - Operations > 100 ms must show non-modal feedback (`gtk::Spinner`, `gtk::ProgressBar`, or `AdwBanner`).
   - Clean up empty/partial UI placeholders on failure or cancellation.
   - Surface transient errors via `AdwToast`; failed lists/network fetches via inline error rows with **Retry**; missing files/grants via pre-flight validation + one-click recovery.
5. **Performance:**
   - Paginate SQLite histories (`LIMIT` + keyset cursor) and batch related lookups (`WHERE id IN (...)`).
   - Cache decoded `gdk::Texture`s (bounded) and completed `syntect` code-block highlights.
   - Throttle streaming UI updates (`~80 ms`) and debounce search inputs (`~150 ms`).
   - Skip `target/`, `node_modules/`, `.git/`, and `.gitignore`d paths in directory scans.

---

## 7. Code Hygiene & Comments
- Clean up all unused variables, dead code, unused `#[allow(...)]` attributes, obsolete CSS, and unused `po/POTFILES.in` entries before finishing.
- Add concise comments explaining *why* for non-obvious invariants, concurrency/locking rules, or GTK/GObject subtleties — avoid narrating self-explanatory code.

---

## 8. Commit Format
Use **Conventional Commits** for all commits:
```text
<type>(<optional-scope>): <short imperative summary>
```
- **Types:** `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `chore`
- **Scopes (optional):** `ui`, `chat_view`, `composer`, `agent`, `inference`, `models`, `storage`, `core`, `i18n`
- **Examples:** `feat(ui): add model context length slider`, `fix(agent): validate redirect hops in fetch_url`, `refactor(core): split markdown builder into submodule`
