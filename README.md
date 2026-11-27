# Companion

**Companion** is a native GNOME AI agent application written in Rust with **GTK4**, **libadwaita**, **Relm4**, embedded **llama.cpp** (CPU + Vulkan), and **SQLite**.

It is designed to run local GGUF models directly on your Linux desktop with a clean, responsive GNOME interface—no external model server required.

---

## Vision

Companion aims to make local AI on Linux **effortless, private, and genuinely native**—a trustworthy, living part of the GNOME desktop rather than another cloud wrapper:

- **Zero-Friction Local AI:** No Docker containers, background daemons, or terminal setup—open the app, explore AI models tailored to your hardware, download with one click, and start chatting.
- **Privacy First & Explicit Consent:** Inference runs locally via `llama.cpp` (CPU + Vulkan). Files, folders, and network features (Web Search) are strictly opt-in, with explicit confirmation dialogs for sensitive or external actions.
- **Typed, Narrow Tools Over Arbitrary Shell:** File and folder operations are strictly path-jailed to user-granted directories rather than exposing unrestricted shell execution.
- **A Personalized Companion, Not Just a Tool:** Customize your companion's name, character preset (or custom behavior), and personal context in **Settings → Personalization**, while keeping per-chat instructions in **Chat Notes**.

### Roadmap: Two-Tier Capability Model

- **Tier 1 — Core Companion (Works Everywhere, Including Flatpak):** Local GGUF inference, vision/PDF/Markdown/text attachments, path-jailed Files & Folders access (`Read-Only` / `Read & Write`), opt-in private Web Search (DuckDuckGo or self-hosted SearXNG) + HTTPS-only `fetch_url`, and future XDG Portal actions (notifications, timers, portal screenshots).
- **Tier 2 — Native GNOME & System Awareness (Opt-In, Native Host / RPM):** Read-only system awareness (disk, RAM, battery, OS status), curated GNOME Desktop preferences (dark/light theme, night light, scaling, power profile with confirmation + undo), application management, and deeper desktop integration (optional background service, quick-ask launcher, GNOME Shell Search Provider)—all gated by Settings toggles and explicit approval.

---

## Why Companion Is Different

- **Privacy & Explicit Consent by Design:** Everything runs locally by default with zero telemetry. Web Search is **disabled by default**—and when enabled, every search query and `https://` page fetch is inspectable and editable before leaving your computer, with automatic warnings if a query overlaps with your local files or contains paths/secrets.
- **Safe Agency Without Unrestricted Shell Access:** Instead of exposing a risky general-purpose shell, Companion uses narrow, path-jailed tools (`Read-Only` or `Read & Write` per chat) and always asks before deleting files/folders or accessing ungranted paths.
- **Embedded `llama.cpp`, No External Server:** Unlike GNOME frontends that require installing and running Ollama separately, Companion embeds `llama.cpp` (CPU + Vulkan) directly in a single native Rust binary.
- **True Native GNOME Performance:** Unlike Electron or web-based local AI UIs, Companion uses `Relm4`, `GTK4`, and `libadwaita` with virtualized scrolling (`gtk::ListView`), `SQLite` + `FTS5`, and zero browser overhead.
- **Personal & Transparent Character:** Give your companion a name, inspect the exact behavior instructions behind every personality preset (or write your own `Custom` behavior), and keep per-conversation **Chat Notes** and thinking levels.
- **Hardware-Aware Model & Vision Explorer:** Browse Hugging Face GGUFs with RAM/VRAM fit guidance, pausable/resumable downloads, automatic `mmproj` vision projector pairing, and automatic recovery of local models.

---

## Key Features

- **Embedded Local Inference:** Run `.gguf` models directly via `llama-cpp-2` with CPU and Vulkan GPU acceleration, streaming Markdown + syntax-highlighted code blocks, and collapsible `<think>` reasoning blocks.
- **Multimodal & Attachments:** Attach images (with `mmproj` vision projector support via `mtmd`) as well as Markdown, text/code files, and PDFs (`pdf-extract`) via file picker, drag-and-drop, or clipboard paste.
- **AI Model Management & Hugging Face Explorer:**
  - Import local `.gguf` models or browse and download models directly from Hugging Face.
  - Hardware-aware RAM fit detection, smart quantization recommendations, and automatic concurrent `mmproj` vision projector downloads.
  - Per-model configuration for context window, GPU layers, and vision projector paths.
- **Per-Chat Controls & Personalization:** Configure AI model, thinking effort level, context window size (with live context usage tracking), and **Chat Notes** per conversation, plus global **Personalization** (Companion Name, Character presets or `Custom`, and About You).
- **Privacy-First Web Search (Opt-In):** Global switch in **Settings → Agent & MCP** (off by default) with per-chat globe toggle, DuckDuckGo or custom SearXNG support, always-ask editable query/URL dialogs, local-file leak warnings, and HTTPS-only `fetch_url` with DNS-pinned SSRF protection.
- **GNOME Integration:** Follows GNOME HIG with `AdwPreferencesDialog`, system light/dark color scheme support, localized UI strings (`gettext`), SQLite FTS5 search, and native file dialogs.

---

## Architecture & Performance

- **Instant Startup & Paginated Storage:** The GTK4/Adwaita window opens immediately while startup sync, GGUF metadata, and syntax sets warm up in the background via a single shared `AppServices` graph. SQLite runs in WAL mode with composite keyset-paginated message loading and FTS5 full-text search.
- **Virtualized Chat Stream (`gtk::ListView`):** Messages render via `gio::ListStore` + `SignalListItemFactory` with `O(viewport)` widget recycling, throttled token-streaming flushes, KV-cache & `mmproj` reuse across turns, and cached Markdown/`syntect` code-block highlighting.
- **`crates/companion-core`** — Minimal pure-domain leaf crate: strongly-typed enums (`MessageRole`, `ThinkingLevel`, `AssistantPersonality`, `FolderPermission`, `SearchEngine`), newtype IDs (`ConversationId`, `MessageId`, `ModelId`), `ModelProvider` / `ChatRequest` / `ProviderError`, `ModelParams`, `AppPaths`, pure prompt composition, and `gettext` helpers (zero GTK, SQLite, Markdown, or `llama.cpp` dependencies).
- **`crates/companion-format`** — Pure-Rust Pango Markdown & `syntect` code-block renderer (`BlockState`, `CodeHighlightCache`) and bounded text/PDF/image attachment extraction (`AttachmentError`).
- **`crates/companion-storage`** — Pure WAL-mode SQLite CRUD (`StorageError`), atomic versioned migrations with downgrade protection, FTS5 search, composite keyset-paginated message loading, conversations, granted folders, pending downloads, and models.
- **`crates/companion-models`** — Hardware RAM/GPU probe, cached seek-based GGUF header reader (`GgufMetadataCache`, `GgufError`), injectable Hugging Face client (`HfClient`), `ModelRegistryService`, disk inventory, algebraic single-worker resumable `DownloadManager` (`DownloadError`), and friendly model naming.
- **`crates/companion-agent`** — `AgentRunner`, session/prompt & page orchestration, MCP-ready polymorphic `Tool` / `ToolRegistry` (`PreparedToolCall`, `ToolMetadata`), tool-call parser, folder-scoped `PermissionEngine`, symlink-safe path-jailed file/directory tools (`list_files`, `read_file`, `search_files`, `write_file`, `create_directory`, `move_path`, `delete_file`, `delete_directory`), DNS-pinned privacy-first web tools (`web_search`, `fetch_url`), streaming coalescer, and title generation.
- **`crates/companion-inference`** — Embedded `LlamaCppProvider` with KV-cache & `MtmdContext` reuse, UTF-8-safe `TokenStreamPipeline`, `ThinkStreamParser`, `StopSequenceFilter`, and context-budget prompt templates.
- **`crates/companion-app`** — Thin Relm4 / GTK4 / `libadwaita` UI shell (100% safe Rust, zero `unsafe`, zero `std::fs` in UI code), shared `AppServices`, virtualized `gtk::ListView` chat history with `MessageRowWidget`, pure headless-testable ViewModels, Chat Controls, Files & Folders manager, keyboard shortcuts, accessibility, and Settings (`AdwPreferencesDialog`).

---

## Building & Running

### Prerequisites
- Rust toolchain (`cargo`, `rustc`)
- `GTK4` (`>= 4.16`) and `libadwaita` (`>= 1.6`) development headers
- `clang` / `LLVM`, `cmake`, and `ninja` (for building embedded `llama.cpp`)
- Optional: a color emoji font (such as `google-noto-color-emoji-fonts` / `fonts-noto-color-emoji`) if running inside a minimal container

### Build & Test

```bash
# Run the full workspace test suite and launch the app
cargo test --workspace && cargo run

# Fast headless tests for domain/logic crates (no GTK4 or llama.cpp build)
cargo test -p companion-core -p companion-format -p companion-storage -p companion-models -p companion-agent

# Full verification gate (fmt, clippy -D warnings, tests, <= 400L/file, <= 50L/fn, zero unsafe, zero non-test #[allow], full-workspace POTFILES, zero callback webs, zero test sleeps, zero UI std::fs, desktop/AppStream validation)
./scripts/ci-check.sh
```

> **Note (Fedora `toolbox` / immutable systems):** Ensure `/usr/bin` precedes custom shims (`export PATH=/usr/bin:$PATH`) when building inside a `toolbox` container.

See [AGENTS.md](AGENTS.md) for engineering, UI, and performance guidelines.

---

## License

Licensed under **GPL-3.0-or-later**. See [Cargo.toml](Cargo.toml).
