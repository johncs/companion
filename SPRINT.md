# Companion — Implementation Sprint Plan

> **Architecture Reference:** [COMPANION.md](./COMPANION.md)  
> **Approach:** Vertical slices — every sprint ends with a runnable, testable build.

---

## Sprint 0: Project Scaffolding & Build Pipeline
**Goal:** Establish a clean Rust + Relm4 + Meson + Flatpak skeleton that builds and launches instantaneously.

- [x] Initialize Cargo project (`relm4`, `gtk4`, `libadwaita`, `tokio`, `tracing`).
- [x] Add Meson build files, Flatpak manifest (`io.github.companion.Companion.json` with `org.gnome.Platform` / `org.gnome.Sdk`), `.desktop` file, and AppStream `.metainfo.xml`.
- [x] Create minimal `AdwApplication` + `AdwApplicationWindow` shell with `AdwToolbarView` and `AdwHeaderBar`.
- [x] Validate AppStream metadata (`appstreamcli validate --no-net` passes).
- [x] **Verification:** Compiled and launched the Companion window via `cargo run` in `toolbox` (`flatpak-builder` check deferred until Flatpak packaging).

---

## Sprint 1: Persistence Layer & Adaptive GNOME Shell
**Goal:** Working sidebar with **Projects** (folder-scoped workspaces), **General Chats**, and instant FTS5 search backed by SQLite.

- [x] Implement `storage/` SQLite layer (WAL mode, migrations, `projects`, `conversations`, `messages`, `messages_fts`, `models`, `personas`).
- [x] Build `AdwNavigationSplitView` + `AdwBreakpoint` adaptive layout.
- [x] Build Sidebar:
  - [x] Create/delete **Projects** (assign folder via `gtk::FileDialog` / XDG FileChooser portal).
  - [x] Expandable project conversation lists + **General Chats** section + "New Chat" action.
  - [x] Live search bar backed by `messages_fts`.
- [x] Build `AdwStatusPage` empty state and initial `AdwPreferencesDialog`.
- [x] **Verification:** Ran `cargo test` and `cargo run` in `toolbox`; verified database migrations, FTS5 search, and adaptive GNOME shell launch cleanly.

---

## Sprint 2: Virtualized Chat Stream & Markdown Rendering
**Goal:** Unlimited smooth scrolling (`gtk::ListView`) with cached Markdown, syntax-highlighted code blocks, and a responsive composer.

- [x] Implement paginated message loading from SQLite into a `gio::ListModel` (`gio::ListStore`).
- [x] Build `gtk::ListView` + `SignalListItemFactory` message rows with widget recycling:
  - [x] Distinct User vs. Assistant styling.
  - [x] Cached Markdown AST (`pulldown-cmark`) + `syntect` syntax-highlighted code blocks with Copy button.
  - [x] Collapsible **Thinking / Reasoning** expander block.
- [x] Build Composer Bar:
  - [x] Auto-growing `gtk::TextView`, Send / Stop Generation button.
  - [x] **Thinking Level** selector (`Off`, `Low`, `Medium`, `High`, `Max`).
- [x] **Verification:** Ran `cargo test` and `cargo run` in `toolbox`; verified virtualized `gtk::ListView`, Thinking blocks, Markdown/code copy, auto-growing composer, and custom GNOME HIG styling.

---

## Sprint 3: Embedded `llama.cpp` Engine & Model Manager
**Goal:** Run local GGUF models out of the box and manage the full model lifecycle (download, import, switch, delete).

- [x] Define `ModelProvider` trait and `ChatEvent` stream (`Token`, `ReasoningToken`, `Done`, `Error`).
- [x] Implement `LlamaCppProvider` (`llama-cpp-2` with Vulkan + CPU fallback, in-memory model caching, `<think>` stream parser, and ~33 FPS throttled UI flushes).
- [x] Build unified **Models** page in Settings (`AdwPreferencesDialog`):
  - [x] Installed models list showing disk usage, active status, capability badges, switch, and **delete** (with `AdwAlertDialog` confirmation).
  - [x] Local `.gguf` import via `gtk::FileDialog`.
  - [x] Hugging Face GGUF browser + resumable `.part` downloader with RAM-fit indicator and progress bar.
- [x] **Verification:** Installed C++/Vulkan build deps in `toolbox`, passed all 7 unit tests, downloaded/imported `.gguf` models, and verified live local `llama.cpp` streaming.

---

## Sprint 4: File Attachments, Vision & Personas
**Goal:** Attach images, text/code, and PDFs in chat, plus reusable Personas and per-model settings.

- [x] Add Composer **Attach File** button (`gtk::FileDialog::open_multiple`) + drag-and-drop (`gtk::DropTarget`) + removable attachment chips with non-blocking loading spinners.
- [x] Implement local attachment processors ([src/core/attachments.rs](src/core/attachments.rs)):
  - [x] Text/code files injected into prompt context (with NUL-byte binary guard and length cap).
  - [x] Local PDF text extraction (`pdf-extract`).
  - [x] Image attachment chips + pre-flight `mmproj` check & `mmproj` picker in per-model settings.
- [x] Add SQLite schema v2 `attachments` table persistence and attachment badges in user message rows.
- [x] Implement **Personas / Prompt Templates** selector in composer and per-model inference settings (`n_ctx`, `n_gpu_layers`, `temperature`, `supports_thinking`, `mmproj_path`).
- [x] **Verification:** Ran `cargo test` and `cargo run` in `toolbox`; verified file attachments (text, PDF, images with thumbnails & lightbox), drag-and-drop, Personas, and per-model settings.

---

## Sprint 5: Project Agent Core, File Tools & Permissions
**Goal:** Turn Companion into a folder-aware AI agent (OpenWork / Claude Cowork style) with safe permission prompts.

- [x] Implement structured tool-calling loop in `src/agent/` and `chat_view.rs`.
- [x] Build Permission Engine (`Deny`, `AllowOnce`, `AllowProject`) backed by `project_permissions` table and `AdwAlertDialog` confirmations (`src/ui/permission_dialog.rs`).
- [x] Implement built-in Project tools (`src/agent/tools.rs`): bounded list/search files (respecting `.gitignore` / common build dirs), read files, create/write files, and confirmed shell execution (`run_command`).
- [x] Add inline collapsible **Tool-Call Cards** in `gtk::ListView` (`src/ui/message_row.rs`, `src/ui/message_object.rs`).
- [x] Add optional right-hand **Project Inspector** (`AdwOverlaySplitView`) in `src/ui/window.rs` showing linked folder and bounded project file list.
- [x] Handle missing/expired Project folder access gracefully with a one-click **"Re-grant Access…"** `AdwBanner` in `src/ui/chat_view.rs`.
- [x] **Verification:** Tested Project tool calling (`list_files`, `read_file`), `AdwAlertDialog` permissions, Project Inspector, `@`-file mentions, and Message Info dialog in `toolbox`.

---

## Sprint 6: Agent Policies & MCP (Model Context Protocol) Support
**Goal:** Give users control over built-in Project Agent permissions and allow extending Companion with external MCP tool servers.

- [ ] Add **Agent Policies & Workspace Permissions** in **Settings → Agent & MCP**:
  - Configurable tool-calling approval modes (e.g., Always Ask, Auto-approve read-only `list_files`/`read_file` inside Project folders, Ask before writes and `run_command`).
  - Workspace folder boundary management and active Project permission status.
- [ ] Implement MCP client (`stdio` local command and `HTTP+SSE` transports) with non-blocking connection/discovery and inline Retry on errors.
- [ ] Add MCP server management UI in **Settings → Agent & MCP** (add, enable/disable, test connection, inspect discovered tools, remove).
- [ ] Route external MCP tools (knowledge bases, databases, GitHub/GitLab, live web/docs) through the unified Tool-Call Card UI and `AdwAlertDialog` permission engine.
- [ ] **Verification:** Configure agent approval policy, connect a sample MCP server, and execute an MCP tool from a chat session.

---

## Sprint 7: GNOME Ecosystem & Quick Launcher
**Goal:** Deep GNOME desktop integration for a 100% local, privacy-first open-weight AI companion.

- [ ] Implement `--gapplication-service` background mode with **Auto-Unload Model on Idle** timer to keep laptop RAM/VRAM free.
- [ ] Build Spotlight-style `adw::Window` **Quick Launcher** (<15 ms toggle, focused prompt, "Continue in Main Window" handoff).
- [ ] Wire `org.freedesktop.portal.GlobalShortcuts` + CLI/D-Bus trigger (`companion --quick-ask`).
- [ ] Add `gio::Notification` alerts, GNOME Shell Search Provider, and Markdown/JSON conversation export.
- [ ] Add **Audio Input Support** (`mtmd` audio projector + `.wav`/`.mp3`/`.ogg` attachments and optional voice input) for audio-capable GGUF models.
- [ ] **Verification:** Toggle Quick Launcher via D-Bus/shortcut, promote a chat to the main window, and verify Auto-Unload on Idle frees model memory.

---

## Sprint 8: Release Polish, Localization & CI
**Goal:** Bring Companion from `0.1.0` to a reproducible, release-ready GNOME application.

- [ ] Generate `po/companion.pot` from `po/POTFILES.in` and add initial `.po` translation catalogs.
- [ ] Update `data/io.github.companion.Companion.metainfo.xml.in` (repository homepage URL, screenshots, and release notes) and `Cargo.toml` version for release.
- [ ] Add GitHub Actions CI workflow running `cargo fmt --check`, `cargo clippy`, and `cargo test`.
- [ ] Verify end-to-end Flatpak build and packaging (`flatpak-builder` + `build-aux/`).
- [ ] **Verification:** Clean CI pass and installable `.flatpak` bundle tested on GNOME.

---

## Sprint 9: Flathub & GNOME Circle (`apps.gnome.org`) Readiness
**Goal:** Meet GNOME Circle design, sandboxing, metadata, and community standards so Companion can be published on Flathub and submitted for `apps.gnome.org`.

- [ ] **Flathub & Sandboxing:**
  - [ ] Finalize reproducible Flatpak manifest (bundling `llama.cpp` + Vulkan support) and publish on Flathub.
  - [ ] Audit Flatpak permissions to rely on XDG Portals (`GtkFileDialog`, folder grants) without broad `--filesystem=host` access.
- [ ] **GNOME Design & HIG Polish:**
  - [ ] Design a custom SVG app icon following GNOME icon guidelines (scalable + symbolic).
  - [ ] Add `AdwAboutDialog` (version, credits, issue tracker, GPL-3.0-or-later license) and a Keyboard Shortcuts dialog (`Ctrl+?`) in the primary menu.
  - [ ] Ensure no placeholder tabs/pages ship in Settings (complete **Settings → Agent & MCP** or hide unfinished sections before release).
  - [ ] Verify adaptive layout across desktop and narrow/compact window widths, light/dark/high-contrast themes, and keyboard-only navigation (`Tab`, `Esc`, `Ctrl+,`).
- [ ] **AppStream, i18n & Community Hygiene:**
  - [ ] Add light/dark screenshots and pass `appstreamcli validate --pedantic` on `data/io.github.companion.Companion.metainfo.xml.in`.
  - [ ] Verify Meson `po/` i18n build (`companion.pot` generation and `.po` compilation).
  - [ ] Add `CODE_OF_CONDUCT.md` (GNOME Code of Conduct) and `CONTRIBUTING.md`.
  - [ ] Switch repository to **public** with Issues enabled (personal GitHub, GitHub org, or GNOME GitLab) and ensure `metainfo.xml.in` + `AdwAboutDialog` URLs match it.
- [ ] **Submission:** Open a GNOME Circle application issue on GNOME GitLab (`Teams/Circle`) after Flathub publication and stability verification.
