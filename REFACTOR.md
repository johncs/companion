# Companion — Quality & Architecture Refactor Plan

> **Goal:** Elevate Companion from a structurally solid indie GNOME app into an **exemplary, GNOME Circle–grade Rust + Relm4 + GTK4 + libadwaita codebase** — leak-free, race-free, strictly layered, cleanly decomposed into small testable building blocks, and verified by strict automated guardrails.
>
> **Engineering Standards:** [AGENTS.md](./AGENTS.md)
> **Rule of Every Sprint:** Every sprint is a **vertical, shippable slice** that ends with `./scripts/ci-check.sh` green, zero regressions, and deterministic tests locking in every fix.

---

## Baseline Metrics (Audit Snapshot)

| Metric | Baseline | Target |
| --- | --- | --- |
| Workspace crates / LOC | 6 crates / `26,736` LOC (`122` `.rs` files) | 6–7 clean crates, minimal leaf `companion-core` |
| Files > 400 lines | `0` (CI-enforced) | `0` (with headroom, not packed to 390–400L) |
| Non-test functions > 50 lines | **`81`** (max: `185` lines) | **`0`** (CI-enforced) |
| Non-test `#[allow(...)]` suppressions | **`7`** (violates `AGENTS.md` §3) | **`0`** (CI-enforced) |
| `unsafe` blocks | `0` (CI-enforced) | `0` |
| Unit / integration tests | `137` (`9` in core UI shell) | `260+` (full security, concurrency, inference & ViewModel suites) |
| Known P0 security / leak / corruption bugs | `20+` across all 6 crates | `0` |

---

## Sprint Overview

| Sprint | Title | Primary Crates | Outcome |
| :---: | --- | --- | --- |
| **R1** | Security, Privacy & Sandbox Hardening | `agent`, `core`, `app` | Closes every filesystem escape, SSRF bypass, privilege escalation, and OOM vector |
| **R2** | Storage & Core Data Integrity | `core`, `storage` | Fixes pagination loss, over-deletion, FTS5/LIKE search, serde mismatch, and markdown bugs |
| **R3** | Inference Fidelity & Token Pipeline | `inference` | Fixes UTF-8 drops, multimodal trim crash, 5 ms GPU throttle, and template/parser bugs |
| **R4** | Downloads, GGUF Parser & Model Registry | `models` | Eliminates `.part` corruption races, HTTP integrity bugs, `Qwen3` quant bug, and GGUF DoS |
| **R5** | UI Memory Leaks, Concurrency Races & Row Bugs | `app` | Eliminates infinite dialog leaks, `RecvError::Lagged` freeze, cross-chat races, and row glitches |
| **R6** | Shared `AppServices` & Zero Main-Thread Blocking | `app`, `models`, `storage`, `core` | Removes all sync DB/FS/GGUF/CPU work from GTK main thread; wires shared caches & clients |
| **R7** | Crate Purity, Typed Errors & MCP-Ready Tools | `core`, `storage`, `agent`, `app` | Slims `core`, makes `storage` pure CRUD, extracts UI orchestration, adds `thiserror` + dynamic `Tool` |
| **R8** | Idiomatic Relm4, Pure ViewModels & Decomposition | `app`, `agent`, `inference`, `models` | Moves to Relm4 `Component`/`FactoryVecDeque`, pure ViewModels, `<= 50`-line functions, zero `#[allow]` |
| **R9** | GNOME Circle Polish, i18n, KV Reuse & Strict CI | `data/`, `po/`, `inference`, `scripts/` | Fixes `xgettext`/`tr_f`/Meson i18n, a11y, shortcuts, CSS tokens, KV-cache reuse, and CI gates |

---

## Sprint R1: Security, Privacy & Sandbox Hardening

**Goal:** Close every verified filesystem-jail escape, SSRF bypass, unintended permission escalation, and unbounded memory read, and lock each invariant behind deterministic security tests.

### 1.1 Filesystem Sandbox & Symlink Safety (`companion-agent`)
- [x] **Fix dangling-symlink jail escape** in `crates/companion-agent/src/tools/path_guard.rs` (`resolve_granted_path`, lines 103–123):
  - Replace `candidate.exists()` + `p.exists()` with component-by-component inspection using `std::fs::symlink_metadata`.
  - Reject any symlink (existing or dangling) whose resolved target escapes `root_canon` before returning a write/create target.
  - Fix nested non-existent parent resolution in `resolve_granted_root_inner` (`path_guard.rs:289–294`) by walking `ancestors()` consistently instead of checking only `req.parent()`.
- [x] **Fix symlink traversal and target deletion** in `crates/companion-agent/src/tools/fs_ops.rs`:
  - In `collect_files` (`fs_ops.rs:205`), inspect `entry.file_type()` and **never** recurse into symlinks (prevents jail escape to `/etc` and infinite symlink cycles).
  - In `delete_granted_file` / `delete_granted_directory` (`fs_ops.rs:110–163`), verify the link itself via `symlink_metadata` so deleting a symlink unlinks the symlink instead of deleting its target.
- [x] **Fix `MovePathTool` path-guard bugs** in `crates/companion-agent/src/tools/write_tools.rs:184–231`:
  - Inspect `from`, `to`, and `root` in `prepare()` so ungranted absolute `from`/`to` paths are detected and surfaced properly.
  - Handle absolute `to` paths inside `root` cleanly in `execute_approved()` without passing an absolute path into `resolve_granted_path` as a relative path.

### 1.2 Permission Scoping & Approval Safety (`companion-agent`, `companion-app`)
- [x] **Stop `Destructive + AllowChat` folder escalation** in `crates/companion-agent/src/runner/dispatch.rs:94, 163–181`:
  - Ensure `ToolRisk::Destructive` never upgrades a `ReadOnly` folder to `ReadWrite` in `cfg.granted_folders` or SQLite.
  - Ensure `PermissionEngine` per-tool scoping `(conv_id, canonical_folder, tool_name)` is not bypassed by blanket `ReadWrite` upgrades, and canonicalize paths consistently before comparing (`path_guard.rs:31`, `dispatch.rs:157, 175`).
  - Move `canonicalize_scope(folder)` *outside* `self.session_allows.lock()` in `crates/companion-agent/src/permissions.rs:65–72`.
- [x] **Stop silent ancestor-walk grant escalation** in `crates/companion-agent/src/tools/path_guard.rs:157–179` & `dispatch.rs:166–172`:
  - When a non-existent absolute path falls back to an existing ancestor (e.g. `/home/user` or `/`), surface the exact resolved ancestor in the approval prompt and disallow implicit `AllowChat` persistence for broad/root ancestors unless explicitly confirmed.
- [x] **Fix destructive confirmation dialog HIG default** in `crates/companion-app/src/ui/permission_dialog.rs:50–60`:
  - Drive `allow_chat` and `is_destructive` from `ToolAuthorization` metadata (remove hardcoded `matches!(tool_name, "delete_file" | "delete_directory")`).
  - For destructive tools, set `alert.set_default_response(Some("deny"))`, `alert.set_close_response("deny")`, style `Deny` neutrally, and style the destructive action as `adw::ResponseAppearance::Destructive`.
  - Make `preview_label` (`permission_dialog.rs:68`) selectable so users can inspect long paths/URLs.

### 1.3 Network SSRF, Privacy Guard & Bounded Reads (`companion-agent`, `companion-core`)
- [x] **Close IPv4-mapped IPv6 and non-global IP SSRF bypasses** in `crates/companion-agent/src/web_search/privacy_guard.rs:122–173`:
  - In `is_public_ip`, unwrap `v6.to_ipv4_mapped()` (and IPv4-compatible `::/96`) and delegate to the IPv4 check so `::ffff:127.0.0.1` and `::ffff:169.254.169.254` are blocked.
  - Block IPv4 `100.64.0.0/10` (CGNAT/Tailscale), `0.0.0.0/8`, `192.0.0.0/24`, `198.18.0.0/15`, `240.0.0.0/4`, and IPv6 `64:ff9b::/96` (NAT64), `fec0::/10`, `2001:db8::/32`, `100::/64`.
  - Use `url.host()` (`Host::Ipv4` / `Host::Ipv6` / `Host::Domain`) instead of manual bracket stripping, and reject URLs carrying embedded credentials (`!url.username().is_empty() || url.password().is_some()`).
- [x] **Inspect URL path & fragment in `inspect_fetch_url`** (`privacy_guard.rs:175–187`):
  - Decode and scan `url.path()`, `url.query()`, and `url.fragment()` for secrets, emails, local paths, and local file overlap.
  - Expand `contains_secret_or_email` to catch `AKIA`, `github_pat_`, `gho_`, `ghs_`, `eyJ` (JWT), `xoxp-`, PEM headers, and fix false positives on `pkg@1.2.3` / `https://site/@author`.
- [x] **Eliminate unbounded memory reads (OOM vectors):**
  - `crates/companion-core/src/attachments.rs:54–88`: check `meta.is_file()` for all attachment kinds (including images) and stream at most `MAX_ATTACHMENT_BYTES` via `File::open` + `Read::take` before binary/UTF-8 checks.
  - `crates/companion-agent/src/tools/fs_ops.rs:47, 77`: use `File::open` + `Read::take(MAX_READ_BYTES + 1)` and slice safely on a UTF-8 char boundary instead of `&bytes[..MAX_READ_BYTES]`.
  - `crates/companion-agent/src/web_search/transport.rs:62–66`: cap response size in `ReqwestTransport::get` identically to `fetch_pinned_hop`.
- [x] **Fix HTML parser edge cases** in `crates/companion-agent/src/web_search/html_parse.rs`:
  - Enforce tag-name boundaries in `strip_tag_block` (don't match `<scripting>` or truncate the page on `</script >`).
  - Bound DuckDuckGo snippet extraction to the current result block and decode `&amp;` *last* in `decode_html_entities`.

### 1.4 Verification & Regression Tests
- [x] Add symlink security tests in `crates/companion-agent/src/tools/tests_path_guard.rs`: dangling symlink write escape, existing symlink escape, symlink cycle in `list_files`/`search_files`, deleting symlink without deleting target, `MovePathTool` absolute `from`/`to`, nested new dirs.
- [x] Add SSRF & privacy tests in `crates/companion-agent/src/web_search/tests.rs`: `::ffff:127.0.0.1`, `::ffff:169.254.169.254`, `100.64.0.1`, `0.0.0.1`, URL credentials, redirect loop > max hops, HTTP downgrade redirect, and URL path/fragment leak detection.
- [x] Add `Destructive + AllowChat` non-escalation test in `crates/companion-agent/src/runner/tests_perms.rs`.
- [x] Add oversized-file & directory-as-image regression tests in `crates/companion-core/src/attachments.rs` (using unique temp dirs with RAII cleanup).
- [x] Pass `./scripts/ci-check.sh`.

---

## Sprint R2: Storage & Core Data Integrity

**Goal:** Eliminate every data-loss, over-deletion, search, serialization, and markdown-parsing bug in `companion-core` and `companion-storage`.

### 2.1 SQLite Query & Schema Fixes (`companion-storage`)
- [x] **Fix keyset pagination dropping same-millisecond messages** in `crates/companion-storage/src/messages.rs:59–92`:
  - Replace `before_created_at: Option<i64>` with a composite cursor `Option<MessageCursor>` (or `before_message_id: Option<&str>` looking up `(created_at, rowid)`).
  - Query with `WHERE conversation_id = ?1 AND (created_at < ?2 OR (created_at = ?2 AND rowid < ?3)) ORDER BY created_at DESC, rowid DESC LIMIT ?4`.
- [x] **Fix `COALESCE(..., 0)` over-deletion** in `crates/companion-storage/src/messages.rs:95–145`:
  - Remove the redundant `created_at` parameter from `edit_message_and_truncate_after` and `delete_message_and_after`.
  - Inside `with_tx`, look up `(created_at, rowid)` via `SELECT created_at, rowid FROM messages WHERE id = ?1 AND conversation_id = ?2`; return `NotFound` / no-op if absent before running the `DELETE`.
- [x] **Fix FTS5 tokenization & `LIKE` escaping** in `crates/companion-storage/src/conversations/mod.rs:216, 267–284`:
  - Escape `\` *before* `%` and `_` in `LIKE` patterns (`ESCAPE '\'`).
  - Split `build_fts_query` tokens on `!c.is_alphanumeric() && c != '_'` so hyphenated terms (`foo-bar`) and file paths (`src/main.rs`) match SQLite's `unicode61` tokenizer.
  - Remove redundant `DISTINCT` on primary-key `conversations` queries.
- [x] **Add missing hot index, schema downgrade guard, and batch safety:**
  - Add migration creating `CREATE INDEX IF NOT EXISTS idx_conversations_pinned_updated ON conversations(pinned DESC, updated_at DESC)` in `crates/companion-storage/src/schema/migrations.rs`.
  - Reject opening databases with `user_version > CURRENT_SCHEMA_VERSION` in `crates/companion-storage/src/schema/mod.rs:45–55` and attach migration version context on errors.
  - Add `ORDER BY rowid ASC` to `list_attachments_for_messages` and chunk `IN (?, ...)` lists against `SQLITE_MAX_VARIABLE_NUMBER` in `agent.rs` and `attachments.rs`.
  - Add an atomic `insert_message_with_tool_calls` method used by `AgentRunner::finalize_run` so assistant replies and tool calls commit or roll back together.
  - Wrap `add_conversation_folder` (`folders.rs`) and `record_model_speed` (`models.rs`) in single transactions / atomic SQL, use `prepare_cached` on hot queries, and prepare `INSERT` statements *outside* loops in `conversations/duplicate.rs`.
  - Replace unchecked `as i64` limit casts with `i64::try_from(...).unwrap_or(i64::MAX)`.
  - Deduplicate repeated `SELECT` column lists and `map_folder` across `conversations/mod.rs`, `messages.rs`, `models.rs`, and `folders.rs`.

### 2.2 Core Domain, Prompt & Markdown Fixes (`companion-core`)
- [x] **Fix `SearchEngine` serde mismatch** in `crates/companion-core/src/types.rs:146–167`:
  - Replace `#[serde(rename_all = "snake_case")]` with explicit `#[serde(rename = "duckduckgo")]` and `#[serde(rename = "searxng")]`.
  - Implement `Display`, `AsRef<str>`, and `TryFrom<&str>` / `FromStr` across all domain enums; add `Serialize`/`Deserialize` to `AssistantPersonality`; stop fail-open `ToolCallStatus::from_str_lossy("corrupt") -> Ok` and fix default `'ask'` mismatch in `migrations.rs:131`.
- [x] **Fix `prompt.rs` & single-pass placeholder substitution:**
  - Pass `clean_user` (not raw untrimmed `user_name`) in `compose_personalized_system_prompt` (`crates/companion-core/src/prompt.rs:60`) and group its 6 positional args into a `PersonalizationProfile` struct.
  - Replace chained `.replace()` calls in `core/src/lib.rs:215–217`, `prompt.rs:19–21`, and `i18n.rs:22–28` with a single-pass template substiter so replacement values containing `{user_name}` or `{1}` are never re-expanded.
  - Log `bind_textdomain_codeset` errors in `i18n.rs:15`.
- [x] **Fix `parallel_chunks` panic swallowing** (`crates/companion-core/src/lib.rs:55`): propagate worker panics (`resume_unwind` or `Result`) instead of `.filter_map(|h| h.join().ok())`.
- [x] **Fix Markdown parser bugs & allocations** in `crates/companion-core/src/markdown/`:
  - Fix `BlockState::Quote` being overwritten without flushing when a code block, table, or horizontal rule starts inside a blockquote (`parser.rs:127–164`).
  - Fix streaming unclosed-fence detection (`parser.rs:101–105`) when only `"```"` has arrived, and flush `TableBuilder::cell_buf` on EOF (`table.rs:76–84`).
  - Avoid cloning entire code blocks for cache lookups (`parser.rs:237`), bound `CodeHighlightCache` capacity, and write syntax-highlighted spans directly via `std::fmt::Write` in `highlight.rs:71–78`.
  - Optimize `truncate_chars` in `attachments.rs:120–127` to a single O(limit) `char_indices().nth(max_chars)` pass.

### 2.3 Verification & Regression Tests
- [x] Add regression tests in `companion-storage`: same-millisecond multi-page pagination (verifying zero dropped messages across all pages), `edit/delete` with missing `message_id` (verifying zero collateral deletion), FTS5 search on `foo-bar`/`main.rs` and `LIKE` with `%`/`_`/`\`, schema future-version rejection, atomic `insert_message_with_tool_calls`, and split the 106-line catch-all test in `conversations_integration.rs`.
- [x] Add unit tests in `companion-core`: serde roundtrips for all enums (including `SearchEngine`), `prompt.rs` whitespace `user_name` + placeholder re-expansion, `i18n::tr_f` with `{0}` inside replacement value, blockquote containing code/table, streaming `"```"` fence, and `parallel_chunks`.
- [x] Pass `./scripts/ci-check.sh`.

---

## Sprint R3: Inference Fidelity & Token Pipeline (`companion-inference`)

**Goal:** Eliminate silent UTF-8 character drops, multimodal context crashes, GPU throughput throttling, and prompt-template fallbacks, while making the decode pipeline headless-testable.

### 3.1 Token Streaming & UTF-8 Decoding
- [x] **Fix silent UTF-8 drops on `OutputFull`** in `crates/companion-inference/src/llama_cpp/runner.rs:261–263, 290–294`:
  - Extract a pure, GTK/llama-free `TokenStreamPipeline` struct combining `encoding_rs::Decoder` + `StopSequenceFilter` + `ThinkStreamParser`.
  - Size the decode buffer using `decoder.max_utf8_buffer_length(output_bytes.len())` (looping until `DecoderResult::InputEmpty`), and flush with `decoder.decode_to_string(&[], &mut tail, true)` on EOF.
- [x] **Fix `StopSequenceFilter` & `ThinkStreamParser` edge cases:**
  - Filter out empty stop sequences in `stop_filter.rs:42–49`, compute exact prefix-overlap holdback instead of holding a fixed 13 bytes unconditionally (eliminates 3–4 words of streaming latency), and normalize `finish()` trailing whitespace behavior.
  - In `think_parser.rs:7–8, 71, 87, 99–115`: avoid allocating a `Vec<&str>` of open tags per token, fix eager match of `"<|channel>thought"` before `"\n"` across token boundaries, and always reset `active_close_tag` in `finish()`.

### 3.2 Multimodal, Budgeting & Template Accuracy
- [x] **Fix multimodal image-list / marker desync** in `crates/companion-inference/src/llama_cpp/runner.rs:66–98`:
  - Collect `image_paths` from `trimmed_messages` (after `trim_turns_to_budget`), never from untrimmed `messages`.
  - Account for per-image vision token budget in `crates/companion-inference/src/llama_cpp/prompt/budget.rs`.
- [x] **Fix orphaned leading `Assistant` turn & template fallbacks** (`prompt/budget.rs`, `prompt/templates.rs`, `prompt/mod.rs`):
  - After trimming old turns, drop any leading `Assistant` turn immediately following `System` so strict Jinja templates (Gemma, Llama, Mistral) do not reject the prompt and silently degrade to ChatML.
  - Log `tracing::warn!` whenever `apply_chat_template` falls back to ChatML (using the currently unused `tracing` dependency).
  - Keep `ChatRole` strongly typed in `prompt/templates.rs` until `LlamaChatMessage::new` (no stringification + string matching).
  - Stop priming `<think>\n` in `format_chatml_fallback` (`templates.rs:158–163`) for non-reasoning `Generic` models; tighten `QwenThinking` template detection; fix Gemma 4 reasoning-effort skip when system prompt already starts with `<|think|>`.

### 3.3 `llama.cpp` Performance, Cancellation & Safety
- [x] **Remove the hardcoded 5 ms/token GPU sleep** (`runner.rs:287, 334–340`):
  - Replace per-token `thread::sleep(5ms)` with OS thread-priority lowering (e.g., idle/nice background worker) or occasional non-sleeping yield, keeping UI fluid without throttling GPU throughput or poisoning `avg_tokens_per_sec`.
- [x] **Fix cancellation & panic propagation:**
  - Make waiting on `provider.inference_lock.lock().await` (`llama_cpp/mod.rs:130–138`) cancellable via `tokio::select!` + `Notify`/interval check.
  - Check `cancel_flag` during `eval_multimodal_prompt` (before `mmproj` load, between images, before `eval_chunks`).
  - Handle `spawn_blocking(...).await` `JoinError` (`llama_cpp/mod.rs:147–160`) by sending `ChatEvent::Error` so the UI never hangs silently on a backend panic.
- [x] **Harden `llama.cpp` context setup:**
  - Implement actual GPU-to-CPU fallback when `LlamaModel::load_from_file` or `model.new_context` fails (`llama_cpp/mod.rs:99–101`, `runner.rs:220–228`).
  - Call `.with_n_batch(self.n_batch as u32)` on `LlamaContextParams`, guard `tokens.is_empty()` before `sampler.sample`, cap auto-expanded `ctx_size` safely, and use checked integer conversions (`i32::try_from`, `saturating_add`).

### 3.4 Verification & Regression Tests
- [x] Add unit tests for `TokenStreamPipeline` covering multi-byte UTF-8 emojis/CJK split across 1-byte tokens, invalid byte `U+FFFD` expansion, EOF flush, empty stop sequences, and `"<|channel>thought"` + `"\n..."` split tokens.
- [x] Add prompt/budget tests verifying no orphaned leading `Assistant` turn after trimming, vision token budget accounting, and `image_paths` matching `<__media__>` count.
- [x] Add `LlamaCppProvider` tests for missing model file, `unload()`, and pre-cancelled requests.
- [x] Pass `./scripts/ci-check.sh`.

---

## Sprint R4: Downloads, GGUF Parser & Model Registry (`companion-models`)

**Goal:** Eliminate every `.part` concurrency race, truncated-file corruption bug, GGUF parser DoS, and model-naming/registration bug in `companion-models`.

### 4.1 Race-Free `DownloadManager` State Machine
- [x] **Enforce single active worker per `DownloadKey`** in `crates/companion-models/src/download_manager/`:
  - Track the active worker's `JoinHandle` (or completion `watch`) and add an explicit completion barrier so `resume()` waits for attempt `N` to exit before opening `.part` for attempt `N + 1`.
  - Prevent `pause()` from overwriting `DownloadStatus::Cancelled`.
  - Check `attempt_id` and non-cancelled state in `handle_fetch_completion`'s `Ok(())` branch (`worker.rs:165–174`), `build_progress_cb` (`worker.rs:94–96`), and atomically inside a single lock in `handle_fetch_error` (`worker.rs:230–248`) — eliminating the double-lock TOCTOU.
  - Serialize `upsert_pending_download` before launching the worker so `update_pending_download_total` / `remove_pending_download` never race ahead of `INSERT`.
  - Make `restore_pending` async (offload SQLite + `fs::metadata` via `spawn_blocking`).
  - Fix `DualProgressAggregator` (`worker.rs:51–68`) so total progress doesn't jump to 100% before both files report `Content-Length`, and use `saturating_add`.
  - Sanitize `repo_id` and `filename` against `..` and leading `/` before joining into `models_dir`.
  - Make `cancel()` return `()` (or propagate actual errors) instead of a dead `Result<()>`.

### 4.2 Resumable HTTP Integrity (`HfClient`)
- [x] **Harden `crates/companion-models/src/huggingface/client.rs`:**
  - Configure explicit `connect_timeout` and `read_timeout` on `reqwest::Client`, and race `stream.next().await` with cancellation so stalled sockets cancel immediately.
  - Verify `downloaded == total_bytes` (when `total_bytes > 0`) before renaming `.part` → `dest_path`.
  - Handle HTTP `416 Range Not Satisfiable` cleanly (check remote size via `HEAD` or restart from 0 instead of deadlocking forever).
  - Persist/send `ETag` (`If-Range`) or validate `Content-Range` so upstream file updates don't concatenate incompatible revisions onto `.part`.
  - In dual-file jobs (`worker.rs:107–144`), skip downloading a sibling whose final `dest_path` already exists and is valid, and extract the duplicated `f_main`/`f_mm` blocks into one helper.
  - Support recursive `/tree/main?recursive=true` (or subdirectory traversal) in `list_repo_gguf_files` so sharded/nested GGUFs in `unsloth`/`bartowski` repos are discovered.
  - Stop swallowing HTTP errors in `huggingface/matcher.rs:164` (`find_matching_mmproj`).

### 4.3 GGUF Parser, Naming & Model Registry
- [x] **Harden `crates/companion-models/src/gguf.rs`:**
  - Add a strict recursion depth cap (e.g., `MAX_ARRAY_DEPTH = 8`) to `skip_gguf_value` (`gguf.rs:321–337`).
  - Use `Seek::seek(SeekFrom::Current(...))` (validating against file length) in `skip_bytes` instead of reading gigabytes into `io::sink()`.
  - Propagate skip failures (`Result`, not ignored `bool`), use `usize::try_from`, read file metadata from the already-opened `File` in `GgufMetadataCache` (removing TOCTOU), and bound cache size + `warm()` concurrency.
  - Fix KV order-dependency (`gguf.rs:226–237`) by recording all `*.context_length` entries and resolving `<arch>.context_length` *after* reading metadata.
  - Expand vision detection (`gguf.rs:208, 255–261`) for `qwen3vl`, `gemma3`, `gemma4`, `llama4`, `mistral3`, `pixtral`, and `<|vision_start|>`/`<|image_pad|>`.
  - Remove dead unreachable code at `gguf.rs:111–113`, fix `format_short_context` rounding, and move UI formatting helpers out of `gguf.rs`.
- [x] **Fix `infer_quantization` & decompose `companion-models/src/lib.rs`:**
  - Fix `is_quant_token` (`lib.rs:32–55`) so `"Qwen3"` / `"Qwen2"` are not misclassified as quantizations (require `Q[0-9]` / `IQ[0-9]` / `F16` / `BF16` / `FP16` / `MXFP4` patterns).
  - Fix `lib.rs:138–146` size-token parser (reject `"b"`, `".b"`, `"eb"`), filter `is_auxiliary_gguf_name` on top-level GGUFs (`lib.rs:279`), and normalize case-insensitive `.gguf` suffix stripping across `lib.rs` and `matcher.rs`.
  - Split `companion-models/src/lib.rs` (375 lines) into `naming.rs`, `discovery.rs`, and `lib.rs`.
- [x] **Fix `ModelRegistryService` (`registry.rs`):**
  - Deduplicate `register_local_gguf` by canonical `file_path` (updating existing records and preserving `avg_tokens_per_sec`/`default_params` instead of creating duplicate rows).
  - Prune stale `mmproj_path` values and auto-pair newly present `mmproj` files during `sync_managed_models`.
  - Pass `&GgufMetadataCache` into discovery & sync so each GGUF is parsed once instead of 3×.
- [x] **Clean `Cargo.toml` & `hardware.rs`:**
  - Remove unused `serde_json` from `crates/companion-models/Cargo.toml`, explicitly enable `tokio` `fs` & `io-util` features, align `hardware.rs` doc (`1.2x` vs `1.25`) + `saturating_mul`, and extract `parse_meminfo(&str)`.

### 4.4 Verification & Regression Tests
- [x] Replace `for _ in 0..1000 { yield_now().await }` loops in `download_manager/tests.rs` with `Notify`/oneshot barriers and add RAII temp-dir cleanup.
- [x] Add tests for: active download cancellation (fix misleadingly named test), `pause` → immediate `resume` race, stale `attempt_id` completion ignored, dual-file resume with one sibling complete, `HfClient::download_gguf_file` (200/206/416/truncated EOF/cancel against local loopback server), `infer_quantization("Qwen3-8B-BF16.gguf")`, `gguf` nested-array depth cap & reversed KV order, `registry` deduplication & stale `mmproj` pruning, and `hardware::parse_meminfo`.
- [x] Pass `./scripts/ci-check.sh`.

---

## Sprint R5: UI Memory Leaks, Concurrency Races & Row Bugs (`companion-app`)

**Goal:** Stop every dialog/task/signal memory leak, fix all UI concurrency races and recycled-row visual bugs, and resolve user-facing behavioral bugs.

### 5.1 Memory Leaks & Lifecycle Safety
- [x] **Fix infinite `spawn_local` channel leaks:**
  - `crates/companion-app/src/ui/model_manager/mod.rs:81–84, 222–277` and `crates/companion-app/src/ui/folder_permissions_dialog.rs:72–118`: never move a strong `tx` clone into its own `rx` loop; use weak widget refs (`downgrade()`) and cancel/close the loop in `dialog.connect_closed`.
- [x] **Break strong `adw::Dialog` reference cycles:**
  - Use `dialog.downgrade()` in child button and controller closures across `chat_controls_dialog/mod.rs`, `model_config_dialog.rs`, `message_dialogs/edit_dialog.rs`, `folder_permissions_dialog.rs`, `preferences/personalization_group.rs`, and `preferences/web_search_group.rs`.
- [x] **Fix `MentionPicker` & window signal leaks:**
  - Break the `list_box.connect_row_activated` cycle in `mention_picker.rs:53–69` via weak refs and ensure `popover.unparent()` runs on parent destroy/unmap.
  - Fix `attach_dynamic_height_scaler` (`composer/input_helpers.rs:270–296`) so it never leaks duplicate `gtk::Window` signal handlers on remap (disconnect on unmap / track once).
  - Remove duplicate `DropTarget` shadowing between `chat_view/handlers.rs:335–382` and `composer/input_helpers.rs:73–94`.
- [x] **Fix download lifecycle subscription bugs** (`hf_browser/download_controls.rs:230–286`, `download_tracker.rs:91–122`):
  - Replace `while let Ok(snap) = rx.recv().await` with an explicit `match rx.recv().await` that continues (resyncing latest snapshot) on `RecvError::Lagged(_)`.
  - Track active downloads by full `DownloadKey` `(repo_id, filename)` instead of only `repo_id`.
  - Detach UI subscriptions on `dialog.connect_closed` (preventing leaked tasks and duplicate completion notifications).
  - Add a model-id/generation guard in `model_manager/vision_setup.rs` and `vision_download.rs`, and register `mmproj` downloads in `DownloadTracker`.

### 5.2 Chat View, Sidebar & Row Correctness
- [x] **Fix cross-conversation & index races in `chat_view`:**
  - Guard `start_llama_stream` UI mutations (`store.append`, `composer.SetGenerating(true)`, `scroll_to_bottom`) on `self.active_conversation_id.as_deref() == Some(conv_id)` (`agent_runner.rs:81, 107`).
  - Guard `handle_edit_prepared` and `handle_delete_completed` (`message_actions.rs:145–308`) by re-locating the item by `message_id` + verifying `load_seq` instead of trusting a pre-async `pos: u32`.
  - Prevent double-send on rapid Enter by setting an in-flight sending guard immediately in `handle_send_message`.
  - Carry `conv_id` through `handle_grant_folder` → `PresentGrantFolderDialog` (`chat_view/handlers.rs:201–245`).
  - Reset `load_seq`, `oldest_loaded_ts`, `has_more_pages`, and `is_loading_older` in `on_clear_conversation` (`handlers.rs:115–124`).
  - Load recent conversation history (up to 48 turns) from SQLite in `prepare_prompt_blocking` instead of slicing the paginated 20-item `ListStore` (`agent_runner.rs:249–267`).
  - Fix `GtkApprovalHandler` (`chat_view/approval.rs:75–76`) to fall back to `default_deny(&req)` instead of always `ApprovalResponse::Tool(Deny)`.
- [x] **Fix recycled `MessageRowWidget` & live-update bugs:**
  - Clear `blocks_box`, `attachments_box`, and stop `streaming_spinner` in `connect_unbind` (`message_row/mod.rs:104–117`).
  - Always refresh `role_label` on `bind_entry_inner` (`message_row/bind.rs:41–45`) so live personalization name changes update visible messages.
  - Stop `finalize_persisted` (`message_object.rs:235–259`) from emitting `"updated"` twice or triggering `BlockUpdatePlan::RebuildAll` on ID swap (preserve rendered blocks & user text selection).
  - Store the streaming code block's latest text in a shared cell read by the "Copy code" button (`message_row/blocks.rs:100–121`) so copying mid-stream copies current code, not the first chunk.
  - Use `msg_obj.with_entry(...)` instead of cloning `MessageEntry` in the theme listener (`message_row/mod.rs:343`), and guard against re-entrant `RefCell` borrows in `message_row/bind.rs:24–31`.
- [x] **Fix Sidebar, Composer & Dialog bugs:**
  - Remove the duplicate `GestureClick` in `sidebar_rows.rs:74–91` (stopping double `SelectConversation` firing per click).
  - Call `refresh_controls_button()` in `ComposerMsg::SetThinkingLevel` (`composer/mod.rs:365`) and add a sequence token to `ComposerModel::refresh_granted_files` (`composer/toolbar.rs:22–50`).
  - Sort *before* `.take(MAX_MENTION_RESULTS)` in `mention_picker.rs:269–287`.
  - Persist Color Scheme in SQLite (`preferences/mod.rs:146–158`) and apply it at startup in `main.rs`.
  - Defer `spawn_starter_metadata_lookup` (`hf_browser/starter_models.rs:42`) until `starter_group` is actually shown (eliminating unsolicited Hugging Face traffic when models are already installed) and fix single-shot `progress.pulse()` / stuck `downloading_label`.
  - Fix `MessageInfoDialog` (`info_dialog.rs:151, 262–281`) live tool-call status updates, thinking-buffer shrinkage, and throwaway widget allocation.
  - Escape Pango markup in `download_controls.rs:221`, `chat_controls_dialog/mod.rs:261–264`, and `model_manager/{mod,active_group}.rs:34–37`; standardize on one escaping helper.
  - Use `AppPaths` for clipboard image storage (`composer/input_helpers.rs:151`) and clean up orphaned clipboard temp files.

### 5.3 Verification
- [x] Add unit tests for `MentionPicker` ranking with > 20 candidates, `BlockUpdatePlan` after unbind onto empty blocks, `MessageObject::finalize_persisted` single-emit behavior, and `DownloadControls` lag recovery.
- [x] Smoke-test opening/closing Settings and Files & Folders repeatedly, switching chats mid-send, editing/deleting after loading older pages, and clicking sidebar rows.
- [x] Pass `./scripts/ci-check.sh`.

---

## Sprint R6: Shared `AppServices` & Zero Main-Thread Blocking

**Goal:** Wire one shared `AppServices` container from `main.rs`, remove the global `TOKIO_RT` singleton from `companion-core`, eliminate all synchronous DB/FS/GGUF/CPU work from the GTK main thread, and stop swallowing persistence errors.

### 6.1 Shared `AppServices` Container & Runtime Ownership
- [x] **Move Tokio runtime ownership to `companion-app::main`** (`crates/companion-core/src/lib.rs:16–27` → `crates/companion-app/src/services.rs`):
  - Delete `static TOKIO_RT` and its `#[allow(clippy::expect_used)]` from `companion-core`.
  - Construct a single `AppServices` struct in `main.rs` holding `Arc` handles to: `Database`, `tokio::runtime::Handle`, `AppPaths`, `Arc<LlamaCppProvider>`, `Arc<DownloadManager>`, `Arc<HfClient>`, `Arc<WebSearchService>`, and `Arc<GgufMetadataCache>`.
- [x] **Fix dead/discarded caches & connection pools:**
  - Retain and warm `services.gguf_cache` at startup (fixing the discarded temporary in `window.rs:196`).
  - Remove free-function wrappers that instantiate fresh `reqwest::Client`s per call (`companion-models/src/huggingface/mod.rs:42–71`, `companion-agent/src/web_search/mod.rs:118–125`) and pass `services.hf_client` / `services.web_search` everywhere.
  - Fix `TEXTURE_CACHE` (`crates/companion-app/src/ui/widgets.rs:137–188`) to key entries by `(path, mtime, file_len)` instead of discarding `_stamp`.

### 6.2 Eliminate GTK Main-Thread Blocking
- [x] **Startup & Window init (`main.rs`, `window.rs`, `chat_view/mod.rs`, `composer/mod.rs`):**
  - Move `ModelRegistryService::sync_managed_models`, initial `list_models()`, `get_active_model_id()`, `get_assistant_name()`, `get_user_name()`, and `get_web_search_global_enabled()` off the GTK main thread (load via async init task and update UI on completion).
- [x] **Dialogs & Preferences:**
  - Query `services.gguf_cache` asynchronously in `chat_controls_dialog` and `model_config_dialog` (zero sync GGUF header reads on dialog open).
  - Debounce text inputs (~300 ms) and offload all SQLite reads/writes in `preferences/{mod,personalization_group,web_search_group}.rs` to background tasks.
  - Offload `folder_permissions_dialog` DB queries, `canonicalize()`, and single-pass `exists()`/`is_file()` checks to `spawn_blocking_ui` (never run sync I/O inside `spawn_local`).
  - Carry `(path, is_dir)` from the background indexer into `MentionPicker` so keystrokes perform zero `stat()` calls (`mention_picker.rs:139`).
  - Offload `starter_models` installed check, `storage_stats` `create_dir_all`, `composer/attachments` `is_dir`/`canonicalize`, `input_helpers` `is_file`/`save_to_png_bytes`, and `widgets::present_image_lightbox` `exists()`.
- [x] **Chat Streaming & Composer:**
  - Pass pre-rendered final `Vec<MarkdownBlock>` from the background coalescer into `finalize_persisted` and cancellation paths (eliminating main-thread `syntect` re-parsing).
  - Offload `provider.unload()` (`chat_view/handlers.rs:177`) to `spawn_blocking` so dropping multi-GB weights never freezes GTK.
  - Cache active model metadata and search engine in `ComposerModel` instead of calling `db.list_models()` on every `SetContextUsed` (`composer/toolbar.rs:119, 163`).
  - Remove `dev_icon.exists()` + scale-1 `IconPaintable::for_file` in `crates/companion-app/src/ui/icons.rs:15–31` and rely on `gtk::IconTheme` (fixing HiDPI blurriness and removing `CARGO_MANIFEST_DIR` from release binaries).
  - Fix nested thread oversubscription in `preferences/storage_stats.rs:110–155` and offload `title_gen.rs` SQLite calls to `spawn_blocking`.

### 6.3 Zero Swallowed Persistence Errors
- [x] Audit and replace silent `let _ = db...` / `.ok()` persistence calls with explicit error logging + user-visible `AdwToast` (or UI state rollback on failure):
  - `window.rs:297`, `chat_view/mod.rs:306, 315, 324`, `chat_view/handlers.rs:271`, `chat_view/message_actions.rs:79`, `composer/actions.rs:80`, `model_manager/active_details.rs:184`, `folder_permissions_dialog.rs:270–284` (revert dropdown on error), `model_config_dialog.rs:191` (close only after save succeeds), `preferences/{personalization_group,web_search_group}.rs`, `runner/dispatch.rs:193–201`, `runner/step.rs:120`, `download_manager/{mod,worker}.rs`.
  - Add SearXNG URL scheme/host validation in `preferences/web_search_group.rs:89–99`.
  - Also fix agent cancellation gaps: check `cancel_flag` between batched tools (`runner/mod.rs:134–144`), abort pending approvals on cancel (`runner/dispatch.rs:70, 114`), bail in `title_gen.rs` on cancel (and strip `<attachment>` tags from fallback title), and stop persisting empty assistant replies on non-cancelled inference errors (`runner/finalize.rs:37–52`).

### 6.4 Verification
- [x] Verify with `grep` that zero non-test `std::fs` or direct sync `db.` calls remain on UI event handlers in `crates/companion-app/src/ui/`.
- [x] Add regression tests in `companion-agent` for empty reply on inference error, mid-batch tool cancellation, and `title_gen` attachment-tag fallback.
- [x] Pass `./scripts/ci-check.sh`.

---

## Sprint R7: Crate Purity, Typed Errors & MCP-Ready Tool Architecture

**Goal:** Make every crate boundary crisp, cohesive, and genuinely reusable, with typed domain errors and an extensible `Tool` system ready for external MCP servers.

### 7.1 Clean Crate Boundaries
- [x] **Slim `companion-core`:**
  - Extract Pango-coupled Markdown rendering (`markdown/`) and PDF/file extraction (`attachments.rs` I/O) into a dedicated crate (e.g. `crates/companion-format`) or clearly separated non-leaf layer so `companion-core` stays a tiny, fast-compiling pure-domain crate (types, IDs, `ModelParams`, `ModelProvider`, `AppPaths`, pure prompt composition, `i18n`).
  - Move `ThinkingLevel` and `AssistantPersonality` out of `companion-core/src/lib.rs` into focused domain modules.
  - Split `StagedAttachment` (unsaved) from `AttachmentRecord` (persisted with `message_id`) — eliminating dummy `message_id: String::new()`.
- [x] **Make `companion-storage` strictly pure CRUD:**
  - Accept explicit `NewConversation` (with caller-resolved `thinking_level` & `system_prompt`) in `create_conversation`, and explicit `new_title: &str` in `duplicate_conversation` (removing hardcoded English `"(Copy)"`).
  - Move speed-averaging heuristic out of `models.rs:81–109` into `companion-models` or `companion-agent`.
  - Seal `with_conn` and `with_tx` as `pub(crate)` (don't leak `rusqlite` in the public API).
  - Centralize default assistant name (`DEFAULT_ASSISTANT_NAME = "Companion"`) and settings key constants in one place; add explicit `ModelProviderType` enum replacing stringly-typed `ModelRecord.provider_type`.
- [x] **Move remaining domain orchestration out of `crates/companion-app/src/ui/`:**
  - Move `prepare_prompt_blocking`, `resolve_active_model_blocking`, and `load_and_parse_page` aggregation (`chat_view/{agent_runner,page_loader}.rs`) into `companion-agent` (e.g., `ConversationSessionService`).
  - Move `model_manager/inventory.rs` into `companion-models` (`ModelRegistryService`).
  - Move `preferences/storage_stats.rs` disk/WAL sizing helpers into `companion-storage` / `companion-models`.
  - Move composer's direct model `n_ctx` mutation (`composer/actions.rs:76–81`) into a clean output/service call so `MainWindow`'s active-model state stays synchronized.

### 7.2 True Polymorphic, MCP-Ready `Tool` System (`companion-agent`)
- [x] **Remove static-lifetime & hardcoded limits on `Tool` / `ToolRegistry`:**
  - Change `Tool::name(&self)` and `Tool::denied_message(&self)` to `&str` / `Cow<'static, str>`, and `ToolRegistry` storage to `HashMap<String, Arc<dyn Tool>>`.
  - Add metadata methods to `Tool` (`display_name`, `description`, `parameter_schema`, `policy_summary`) so `preferences/tools_group.rs`, `permission_dialog.rs`, and `prompt.rs` tool instructions are all generated from `ToolRegistry` (single source of truth).
- [x] **Single-pass parse & validation (`PreparedCall`):**
  - Have `Tool::prepare()` parse and validate JSON arguments *once*, returning `ToolAuthorization::Rejected` immediately on malformed args (fixing broken `"Delete file: ?"` dialogs in `write_tools.rs` / `destructive_tools.rs`) and returning a typed/boxed `PreparedToolCall` to `execute_approved()`.
  - Inject `Arc<WebSearchService>` into `WebSearchTool` and `FetchUrlTool` so they can be mock-tested through `AgentRunner`.
  - Split kitchen-sink `ToolPrepareContext`/`ToolContext` and remove the hardcoded `tool_name == "read_file"` special-case in `runner/dispatch.rs:219` (use a `Tool::produces_sensitive_local_context()` flag or output tag instead).
  - Deduplicate the 8× `spawn_blocking` tool wrappers and `list_files`/`search_files` multi-root fan-out in `crates/companion-agent/src/tools/`.
  - Fix `coalescer.rs` multi-step reasoning separator (`"\n\n"`), reuse `CodeHighlightCache` across steps in a turn, inject `tokio::runtime::Handle`, and use `tokio::time::Instant` so `tokio::time::pause()` tests real interval coalescing.
  - Fix `parser.rs`: preserve fenced code block whitespace in `strip_tool_calls`, handle unclosed `<tool_call>` followed by a valid call, and cap `extract_mentioned_os_paths` work (`path_mentions.rs`) while scanning user text *before* prepending `<attachment>` blocks (`prompt.rs:113, 131`).

### 7.3 Typed Errors & Strong Domain Types
- [x] Add `thiserror` and define focused error enums at crate boundaries:
  - `StorageError` (`NotFound`, `ConstraintViolation`, `SchemaTooNew`, `Database`, `LockPoisoned`)
  - `AttachmentError` (`NotFound`, `NotAFile`, `TooLarge`, `BinaryFile`, `EmptyPdf`, `Io`)
  - `DownloadError` (`Cancelled`, `HttpStatus`, `IncompleteTransfer`, `InvalidPath`, `Io`, `Network`)
  - `GgufError` (`InvalidMagic`, `UnsupportedVersion`, `DepthLimitExceeded`, `Truncated`, `Io`)
  - `ProviderError` (`ModelNotFound`, `ContextOverflow`, `Cancelled`, `Backend`)
- [x] Introduce lightweight newtype IDs (`ConversationId`, `MessageId`, `ModelId`) in `companion-core` and validate `ModelParams` invariants (finite `temperature`, non-zero `n_ctx`/`max_tokens`).
- [x] Compose `InferenceParams` around `ModelParams` (removing duplicated fields and conflicting `thinking_level` defaults) and group `ModelProvider::stream_chat`'s 5 arguments into a `ChatRequest` struct.

### 7.4 Verification & Regression Tests
- [x] Add end-to-end `AgentRunner` tests exercising mocked `WebSearchTool`/`FetchUrlTool`, malformed write/destructive args rejected before approval, Gemma 4 tool-call parsing + `strip_tool_calls` preserving code blocks, and real `tokio::time::pause()` coalescing intervals.
- [x] Verify clean crate dependency graph with `cargo tree -d` / `cargo test --workspace`.
- [x] Pass `./scripts/ci-check.sh`.

---

## Sprint R8: Idiomatic Relm4, Pure ViewModels & Function Decomposition

**Goal:** Eliminate the Relm4/imperative split in `companion-app`, extract pure headless-testable ViewModels for every major view, decompose all 81 functions > 50 lines, and eliminate all 7 non-test `#[allow(...)]` suppressions.

### 8.1 Idiomatic Relm4 Shell & Shared UI Building Blocks (`companion-app`)
- [ ] **Upgrade core components from `SimpleComponent` to `Component`:**
  - Convert `MainWindow`, `ChatViewModel`, `SidebarModel`, and `ComposerModel` to `relm4::Component` using `sender.oneshot_command` / `sender.command` so async tasks receive automatic `ShutdownReceiver` cancellation and flow cleanly through `update_cmd` instead of ad-hoc `spawn_local` mutations.
  - Unify async UI task spawning onto one pattern (retire mixed `runtime().spawn_blocking` vs `spawn_blocking_ui`).
- [ ] **Eliminate imperative list & toolbar thrashing:**
  - Convert `SidebarModel` lists from imperative `clear_list_box` + SQLite re-query on every click/spinner toggle to an in-memory cached list backed by `FactoryVecDeque` or `gtk::ListView`.
  - Convert `ComposerModel`'s 4 imperative toolbar buttons (`composer/toolbar.rs:60–160`) to declarative `#[watch]` bindings driven by a pure `ComposerViewModel`.
- [ ] **Extract reusable GTK/Adw dialog & widget primitives:**
  - [x] Shared `DialogScaffold` builder (standard `adw::Dialog` + `ToolbarView` + `HeaderBar` + `Cancel` / primary action + weak-ref close wiring) reused by `chat_controls_dialog`, `model_config_dialog`, and `edit_dialog`.
  - [x] Shared `open_path_with_launcher` helper replacing 3 copies (`folder_permissions_dialog`, `storage_stats`, `model_manager/active_group`).
  - [x] Shared editable confirmation-dialog builder unifying `present_web_search_confirm_dialog` and `present_fetch_url_confirm_dialog` (`permission_dialog.rs`).
  - [x] Shared `ImageThumbnailButton` builder deduplicating `composer/attachments.rs` and `message_row/bind.rs`.
  - [x] Remove nested vertical `ScrolledWindow`s in `chat_controls_dialog`, `edit_dialog`, and `info_dialog`.
  - [ ] Standardize row action button classes (`["flat"]` vs `["flat", "circular"]`) and `ModelConfigFormState` / `ChatControlsFormState` defaults using `ModelParams::default()` (and disable `Reset to Default` / `Save` when clean).

### 8.2 Pure ViewModels & UI Test Coverage
- [x] Extract pure, GTK-free, unit-tested ViewModels for:
  - `ChatViewState` (conversation switching, `load_seq`, pagination cursor state, active runs map, send/edit/delete state transitions).
  - `ComposerViewModel` (toolbar tooltips/icons/labels, send/stop state, attachment readiness).
  - `SidebarViewModel` (cached conversations, search debounce sequencing, active selection, running indicators).
  - `FolderPermissionsViewModel` (directory/file grouping, covering-folder checks, permission rollback).
  - `MessageInfoViewModel` (duration/speed/timestamp/tool-call formatting).

### 8.3 Zero `#[allow(...)]` & `<= 50`-Line Function Budget
- [x] **Remove all 7 non-test `#[allow(...)]` suppressions:**
  - `companion-app/src/ui/message_object.rs:274, 300`: store `RefCell<MessageEntry>` (`Default`) instead of `RefCell<Option<MessageEntry>>`, eliminating both `.expect()` calls and `#[allow(clippy::expect_used)]`; remove unused `set_metrics`.
  - `companion-agent/src/runner/step.rs:102` & `dispatch.rs:139`: group arguments into typed context structs, removing `#[allow(clippy::too_many_arguments)]`.
  - `companion-agent/src/tool.rs:75, 77`: wire `risk` and `ungranted_path` into `permission_dialog.rs` (removing `#[allow(dead_code)]`).
  - `companion-app/src/ui/icons.rs:8`: remove unused `THINKING` constant (or use it) and `#[allow(dead_code)]`.
  - `companion-core/src/i18n.rs:3`: remove redundant `#[allow(unused_imports)]`.
  - `preferences/personalization_group.rs:281, 308`: remove pointless `RefCell::new(text_buffer)`.
- [x] **Decompose all 81 non-test functions > 50 lines** into focused `<= 50`-line helpers/methods across all crates, prioritizing the largest offenders first:
  - `chat_view/agent_runner.rs::start_llama_stream` (185L)
  - `folder_permissions_dialog.rs::present_folder_permissions_dialog` (165L) & `build_grant_row` (88L)
  - `companion-agent/src/parser.rs::parse_tool_payload` (139L)
  - `model_manager/mod.rs::build_models_page` (124L) & `active_details.rs::build_installed_model_row` (114L) & `active_group.rs::build_active_group` (104L)
  - `chat_view/mod.rs::update` (121L), `sidebar/mod.rs::update` (102L), `window.rs::init` (99L) & `update` (87L)
  - `chat_controls_dialog/mod.rs::build_settings_section` (118L), `model_config_dialog.rs::present_model_config_dialog` (109L), `edit_dialog.rs::present_edit_message_dialog` (106L)
  - `runner/finalize.rs::finalize_run` (107L), `runner/dispatch.rs::dispatch_tool` (107L), `runner/step.rs::run_single_step` (85L), `runner/mod.rs::run` (77L)
  - `path_guard.rs::resolve_granted_root_inner` (91L), `download_controls.rs::new` (91L), `page_loader.rs::load_and_parse_page` (90L), `mention_picker.rs::update_from_buffer` (86L)
  - Remaining functions in `message_row/`, `sidebar_rows.rs`, `composer/`, `permission_dialog.rs`, `preferences/`, `hf_browser/`, `companion-inference`, `companion-models`, and `companion-storage`.

### 8.4 Verification
- [x] Confirm `0` non-test functions > 50 lines and `0` non-test `#[allow(...)]` across `crates/`.
- [x] Pass all new pure ViewModel unit tests + `./scripts/ci-check.sh`.

---

## Sprint R9: GNOME Circle Polish, i18n Pipeline, KV Reuse & Strict CI

**Goal:** Fix broken translation extraction and Meson packaging, add native GNOME accessibility and keyboard navigation, optimize multi-turn `llama.cpp` latency, and lock every quality invariant into CI permanently.

### 9.1 Localization & GNOME Packaging (`po/`, `data/`, `src/meson.build`)
- [ ] **Fix `xgettext` extraction:**
  - Add `args: ['--keyword=tr_f:1']` to `po/meson.build` so every `tr_f(...)` string is extracted.
  - Refactor `preferences/tools_group.rs` (now driven from `ToolRegistry` metadata) to use extractable `gettext("...")` string literals.
  - Localize raw strings in `message_dialogs/info_dialog.rs` (`"s"`, `tc.status.as_str()`, raw `tc.tool_name`), `widgets.rs` (`"FILE"`), `composer/attachments.rs` (`"file"`), `composer/toolbar.rs` bullet layout, and `companion-agent` error/status messages.
- [ ] **Fix `po/POTFILES.in` drift:**
  - Remove the 6 stale entries (`icons.rs`, `agent/src/lib.rs`, `runner/mod.rs`, `web_search/mod.rs`, `core/src/attachments.rs`, `storage/src/attachments.rs`).
  - Add the 4 missing `companion-agent` files (`runner/step.rs`, `title_gen.rs`, `tool.rs`, `tools/web_tools.rs`) plus any newly split files.
- [ ] **Fix Meson, Desktop & AppStream metadata:**
  - Switch `data/meson.build:4–21` from `configure_file` to `i18n.merge_file` so `.desktop` and `.metainfo.xml` are actually translated.
  - Add `desktop-file-validate` and `appstreamcli validate --no-net` Meson tests in `data/meson.build`.
  - Add `SingleMainWindow=true` to `data/io.github.companion.Companion.desktop.in` and replace stale `"Projects"` copy with `"Files & Folders"`.
  - Update `data/io.github.companion.Companion.metainfo.xml.in`: current release description, `<translation type="gettext">companion</translation>`, `<supports>`, `<requires><display_length compare="ge">360</display_length></requires>`, `<branding>`, consistent repository/developer IDs, and `bugtracker`/`vcs-browser` URLs.
  - Fix `src/meson.build` to pass `-p companion-app` and support `--offline` Flatpak builds; normalize `io.github.companion.Companion-symbolic.svg` dimensions (`width="16" height="16"`).

### 9.2 Native GNOME Accessibility, Keyboard Shortcuts & CSS Polish
- [ ] **Keyboard accelerators & HIG polish (`window.rs`, `main.rs`):**
  - Wire standard GNOME `gio::SimpleAction`s and accelerators: `Ctrl+N` (New Chat), `Ctrl+F` (Search Sidebar), `Ctrl+,` (Preferences), `Esc` (Stop Generation), `Ctrl+?` (Keyboard Shortcuts dialog), plus `AdwAboutDialog` in the primary menu.
  - Hide unfinished placeholder sections in Settings (`preferences/mod.rs:240–246` MCP placeholder, `view_model.rs:73` `"Coming soon"` audio) behind feature flags until implemented.
  - Add `adw::Spinner` / `gtk::Spinner` to `hf_browser/mod.rs:140–144`, inline Retry button in `hf_browser/repo_rows.rs:71–74`, and replace pointer-centric `"Click +"` text in `folder_permissions_dialog.rs:105, 113`.
- [ ] **GTK4 Accessibility (a11y):**
  - Add `AccessibleRelation::LabelledBy` / `AccessibleProperty::Label` on the composer `gtk::TextView` and key icon controls.
  - Make document tiles (`widgets.rs:191–226`) keyboard-focusable with accessible names, remove the `grab_focus()` ping-pong hack in `mention_picker.rs:223–224`, replace fixed `width_chars` in `preferences/tools_group.rs:183–184` with `gtk::SizeGroup`, and restore visible focus rings on `listview.chat-list > row`.
- [ ] **Modernize `crates/companion-app/src/ui/style.css`:**
  - Remove the 4 dead CSS rules (`style.css:62–84`: `.thinking-expander`, `dropdown.pill-dropdown > button`, `popover.model-popover contents`, `button.popover-item`).
  - Scope `list.navigation-sidebar` (`style.css:184–195`) to `list.companion-sidebar` so it doesn't bleed into `MentionPicker`; remove misleading `"composer-input"` class from dialog `TextView`s.
  - Migrate deprecated `@define-color` variables (`@window_fg_color`, etc.) to modern Libadwaita CSS custom properties (`var(--window-fg-color)`, `var(--border-color)`, `color-mix(...)`) and verify Light, Dark, and High Contrast themes.

### 9.3 Multi-Turn Inference Caching (`companion-inference`)
- [ ] **KV-cache prefix reuse & `MtmdContext` caching:**
  - Retain the active `LlamaContext` (and its cached prompt token vector) inside `LlamaCppProvider` when model path and `n_ctx` are unchanged, trimming only the diverged KV suffix (`clear_kv_cache_seq`) instead of re-evaluating the entire conversation + tool definitions from `pos = 0` on every agent step.
  - Cache the loaded `MtmdContext` alongside the active `LlamaModel` so `mmproj` weights (500 MB–1.7 GB) aren't reloaded from disk on every multimodal turn.

### 9.4 Permanent CI Guardrails (`scripts/ci-check.sh` & `.github/workflows/ci.yml`)
- [ ] Expand `scripts/ci-check.sh` to enforce:
  1. **Zero non-test `#[allow(...)]`** across `crates/`.
  2. **Full-workspace `po/POTFILES.in` sync** — scan *all* crates for `gettext`/`ngettext`/`tr_f` AND fail on stale entries with zero matches.
  3. **Non-test function length `<= 50` lines** across `crates/`.
  4. **Zero `std::fs` calls in `crates/companion-app/src/ui/`** (outside `#[cfg(test)]`).
  5. **Desktop & AppStream validation** (`desktop-file-validate`, `appstreamcli validate --no-net`) when available.
- [ ] Add `.github/workflows/ci.yml` running `./scripts/ci-check.sh` on every push and pull request.
- [ ] Run full `./scripts/ci-check.sh` and Meson build verification end-to-end.

---

## Definition of Done

Companion reaches the target quality bar when:
1. Every checklist item in **Sprints R1–R9** is completed and verified.
2. `./scripts/ci-check.sh` passes with **all tightened guardrails active** (`fmt`, `clippy -D warnings`, `test --workspace`, `<= 400` lines/file, `<= 50` lines/fn, zero `unsafe`, zero non-test `#[allow(...)]`, zero callback webs, zero test sleeps, zero `std::fs` in UI handlers, full-workspace `POTFILES.in` sync).
3. `meson test` validates `.desktop`, `.metainfo.xml`, and `.pot` extraction (including `tr_f`).
4. Opening/closing Settings & dialogs shows zero leaked tasks/widgets, streaming stays fluid with zero main-thread I/O stalls, and every security/concurrency/storage/inference bug has a permanent deterministic regression test.
