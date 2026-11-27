# Companion — Roadmap

Planned future milestones for Companion. For current architecture and engineering invariants, see [AGENTS.md](./AGENTS.md).

---

## 1. External MCP Server Support
- [ ] Implement MCP client (`stdio` and `HTTP+SSE` transports) with non-blocking connection/discovery and inline Retry on errors.
- [ ] Build MCP server management UI in **Settings → Agent & MCP** (add, enable/disable, test connection, inspect discovered tools, remove) and enable the `experimental-mcp` feature flag.
- [ ] Route external MCP tools through `ToolRegistry`, `ToolMetadata`, `PermissionEngine`, and inline Tool-Call Cards.

## 2. Background Service, Quick Launcher & Audio Input
- [ ] Implement `--gapplication-service` background mode with an **Auto-Unload Model on Idle** timer to free RAM/VRAM.
- [ ] Build Spotlight-style `adw::Window` **Quick Launcher** with *"Continue in Main Window"* handoff, `org.freedesktop.portal.GlobalShortcuts`, and `companion --quick-ask`.
- [ ] Add GNOME Shell Search Provider, `gio::Notification` alerts, and Markdown/JSON conversation export.
- [ ] Add audio input support (`mtmd` audio projector + `.wav` / `.mp3` / `.ogg` attachments) and enable the `experimental-audio` feature flag.

## 3. Localization, Flathub & GNOME Circle (`apps.gnome.org`)
- [ ] Generate `po/companion.pot` from `po/POTFILES.in` via Meson and add initial `.po` translation catalogs.
- [ ] Add reproducible Flatpak manifest (bundling `llama.cpp` + Vulkan + offline `cargo-sources.json`) and verify XDG Portal sandboxing without `--filesystem=host`.
- [ ] Add light/dark `<screenshots>` to `data/io.github.companion.Companion.metainfo.xml.in` and verify `appstreamcli validate --pedantic`.
- [ ] Add `CODE_OF_CONDUCT.md` (GNOME Code of Conduct) and `CONTRIBUTING.md`, tag a release, publish on Flathub, and submit to GNOME Circle (`Teams/Circle`).
