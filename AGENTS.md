# Companion — Engineering & Agent Guidelines

This repository contains **Companion**, a native GNOME AI agent application written in Rust (`Relm4` + `GTK4` + `libadwaita` + embedded `llama.cpp` + `SQLite`).

All contributors and AI agents working in this repo must follow these rules.

---

## 1. Build & Test Commands
Requires Rust (stable toolchain) and native development libraries (`GTK4`, `libadwaita`, `SQLite`, `pkg-config`, `cmake`, and a C/C++ compiler for embedded `llama.cpp`).

```bash
cargo test && cargo run
```
*(If building inside a Fedora `toolbox` or container with non-standard `PATH` ordering, prepend `export PATH=/usr/bin:$PATH`.)*

---

## 2. Architecture & Module Boundaries
- `src/agent/` — Tool-call parser, `PermissionEngine`, path-jailed file tools (`tools/`), and privacy-first web search/fetch (`web_search/`).
- `src/core/` — Domain types (`ThinkingLevel`), cached Markdown/syntect parser, and the shared Tokio runtime (`core::runtime()`).
- `src/inference/` — `ModelProvider` trait, `ChatEvent`, `<think>` stream parser, and `LlamaCppProvider` (CPU + Vulkan).
- `src/models/` — Hardware RAM/GPU probe, Hugging Face GGUF client & resumable downloader, and friendly model naming.
- `src/storage/` — WAL-mode SQLite (`PRAGMA user_version` migrations in `schema.rs`, FTS5 search, paginated messages, models, projects).
- `src/ui/` — Relm4 components, `gtk::ListView` row factories, `AdwPreferencesDialog` settings, and `ui/icons.rs` for centralized icon names.

### Coding Paradigm & Modularity Standards
- **Small, Single-Purpose Files & Functions:** Target **<= 400 lines per `.rs` file** and **<= 50 lines per function**. When a feature or Relm4 component grows beyond that, split it into a folder module (`mod.rs` + focused submodules) rather than mixing layout, async orchestration, and business rules in one file.
- **Separate Pure Logic from GTK/Relm4:** State machines, parsers, path/privacy guards, formatting, and validation must live in pure Rust functions and structs with **zero GTK/Relm4 dependencies** so they can be unit-tested directly with `cargo test`. Keep Relm4/GTK modules thin: wire widgets, forward events, and render state.
- **Reuse Shared Plumbing:** Keep `core::runtime()` → channel → `glib::MainContext` async bridges (`ui::widgets::spawn_blocking_ui`), inline retry rows, progress indicators, icon paths (`ui/icons.rs`), and destructive confirmation dialogs (`ui::widgets::present_destructive_confirm`) in centralized helpers rather than duplicating boilerplate across modules.

---

## 3. GNOME HIG & UI Standards
- Use modern `libadwaita` widgets (`AdwNavigationSplitView`, `AdwOverlaySplitView`, `AdwPreferencesDialog`, `AdwAlertDialog`, `AdwStatusPage`, `AdwBanner`, `AdwToastOverlay`).
- **Virtual Scrolling:** Chat messages must live in `gtk::ListView` (`gio::ListStore` + `SignalListItemFactory`), never `gtk::ListBox` or a `gtk::Box` with all children instantiated.
- **Pango Markup Titles:** `AdwActionRow` and `AdwPreferencesGroup` parse `title` as Pango markup by default—always escape `&` as `&amp;`.
- **Icons:** Register all custom symbolic icons in `src/ui/icons.rs` and `data/meson.build` so composer, sidebar, and Settings stay in sync.
- **Localization:** Wrap all user-facing strings in `crate::i18n::gettext` / `tr_f` (and add any new source files with UI strings to `po/POTFILES.in`).
- **Button Labels:** Never use trailing ellipses (`…` or `...`) in button labels.

---

## 4. Mandatory Loading & Error State Checklist
Every new feature (attachments, vision, agent tools, MCP, Quick Launcher, cloud providers) **must** handle loading and error states in both code and UI:

1. **Never Block the GTK Main Thread:**
   - Run inference, network requests, heavy file parsing (PDF/images), and MCP I/O on `core::runtime()` (`spawn` / `spawn_blocking`).
2. **Show Non-Blocking Loading Feedback:**
   - Any operation taking >100 ms must show a clear non-modal indicator: inline status + `gtk::Spinner` (e.g., `"Loading model…"`), `gtk::ProgressBar`, or `AdwBanner`. Never use modal dialogs that freeze scrolling or prevent cancellation.
3. **Clean Up Partial / Failed UI State:**
   - If an async operation (like streaming or file attachment) fails or is cancelled before producing output, remove any empty placeholder row/chip rather than leaving blank UI behind.
4. **Inline Retry & Actionable Errors:**
   - Transient errors: show an `AdwToast` with clear wording.
   - Failed lists/network fetches (e.g., Hugging Face, MCP discovery): render an inline error row (`network-offline-symbolic` / `dialog-warning-symbolic`) with a **Retry** (`view-refresh-symbolic`) button.
   - Missing local resources (e.g., moved `.gguf` file or expired XDG Portal folder grant): validate **before** running inference/tools and guide the user with a one-click action (`AdwBanner` or `AdwToast`).
5. **No Runtime `panic!` / `.expect()` on User or I/O Data:**
   - Propagate errors with `anyhow::Result` and surface them cleanly in the UI.

---

## 5. Mandatory Performance & Responsiveness Rules
Companion must **never freeze, stutter, or lag**:

1. **Paginated & Batched Storage:**
   - Never load unbounded message histories into memory; paginate with SQLite (`LIMIT` + keyset cursor) and batch related lookups (`WHERE id IN (...)`) to avoid N+1 queries.
2. **Cache Expensive UI & Parsing Work:**
   - Cache decoded `gdk::Texture`s (bounded in-memory cache) so `gtk::ListView` row recycling never re-decodes images on scroll.
   - Cache completed `syntect` code-block highlights during token streaming to avoid `O(N²)` re-highlighting.
3. **Throttle & Debounce High-Frequency Events:**
   - Throttle streaming UI flushes (`~80 ms` via `STREAM_UI_FLUSH_INTERVAL`) rather than re-laying out GTK widgets on every token.
   - Debounce live search inputs (~150 ms) before querying FTS5 or rebuilding lists.
4. **Reuse Shared Clients & Runtimes:**
   - Reuse `core::runtime()` for async tasks and shared `reqwest::Client` instances for connection/TLS pooling.
5. **Respect `.gitignore` & Bounded Directory Scans:**
   - When indexing or searching Project folders (especially over XDG Document Portal FUSE mounts), skip `target/`, `node_modules/`, `.git/`, and `.gitignore`d paths.

---

## 6. Pre-Completion Change Checklist
Before finishing any code edit or feature, verify every applicable item:

1. **Loading & Error Handling:** Are all async or fallible paths covered with non-blocking loading indicators, clean error recovery (toasts, inline retry, or cleanup of partial UI), and no `.expect()`/`panic!` on user or I/O data?
2. **Performance & Responsiveness:** Does the change keep the GTK main thread free (offloading heavy I/O, parsing, or network work to `core::runtime()` and using multicore processing like `core::parallel_chunks` where beneficial)?
3. **Clean Architecture & Reusability:** Are files (`<= 400` lines) and functions (`<= 50` lines) small and single-purpose, with pure logic separated from GTK/Relm4 and no duplicated plumbing?
4. **No Leftovers:** Were all unused variables, dead code, obsolete styling, and unused translation strings or `POTFILES.in` entries cleaned up?
5. **GNOME HIG & Localization:** Does the UI follow GNOME HIG (`libadwaita` patterns, virtual scrolling, escaped Pango markup), and are all user-facing strings wrapped in `gettext` / `tr_f`?
6. **Tests:** Have unit tests been added or updated for any new non-UI logic, state transitions, parsing, or edge cases (kept free of GTK dependencies)?
7. **Centralized Constants & Shared State:** Are shared constants, icons (`ui/icons.rs`), and global utilities (`core::runtime()`) placed in their centralized modules rather than hardcoded or duplicated?
8. **Privacy & Portability:** Are there zero hardcoded personal paths (e.g., `/home/<user>`), secrets, or machine-specific assumptions in code, tests, or documentation?
9. **Accessibility & Keyboard Navigation:** Do icon-only buttons have clear `tooltip_text` / accessible labels, and do dialogs and views work cleanly with keyboard navigation (`Tab`, `Esc`, `Enter`)?
10. **Safe Storage Migrations:** Do any SQLite schema changes use a versioned migration (`PRAGMA user_version` in `src/storage/schema.rs`) so existing user databases upgrade without data loss?
11. **Meaningful, Concise Comments:** Do non-obvious invariants, concurrency/locking rules, or GNOME/GTK conventions have brief comments explaining *why* (without bloating self-explanatory code)?

---

## 7. Commit Format
Use **Conventional Commits** for all commits:
```text
<type>(<optional-scope>): <short imperative summary>
```
- **Types:** `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `build`, `chore`
- **Scopes (optional):** `ui`, `chat_view`, `composer`, `agent`, `inference`, `models`, `storage`, `core`, `i18n`
- **Examples:** `feat(ui): add model context length slider`, `fix(agent): handle query params in URL leak check`, `refactor(chat_view): split streaming coalescer into submodule`


