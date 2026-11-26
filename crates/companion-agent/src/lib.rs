//! Chat Agent core: sandboxed folder/file tools, permission engine, and tool-call parser.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod approval;
pub mod coalescer;
pub mod parser;
pub mod path_mentions;
pub mod permissions;
pub mod prompt;
pub mod registry;
pub mod runner;
pub mod title_gen;
pub mod tool;
pub mod tools;
pub mod web_search;

/// Instructions appended to the system prompt so the assistant can use file/folder tools.
pub const WORKSPACE_TOOLS_INSTRUCTIONS: &str = "\
You have tool access to local files and folders. To invoke a tool, output a <tool_call> block with valid JSON:
<tool_call>{\"name\": \"tool_name\", \"arguments\": {...}}</tool_call>

Available tools:
- list_files: {\"root\": \"/path/to/dir\", \"subdir\": \".\"} — List files in a directory (respecting .gitignore).
- read_file: {\"path\": \"/path/to/file\"} — Read a UTF-8 text file.
- search_files: {\"root\": \"/path/to/dir\", \"query\": \"text\"} — Search for text across files/directories.
- write_file: {\"root\": \"/granted/dir\", \"path\": \"relative/path\", \"content\": \"...\"} — Create or overwrite a file in a [read & write] directory (requires user approval).
- delete_file: {\"root\": \"/granted/dir\", \"path\": \"relative/path\"} — Delete a single file in a [read & write] directory (requires user approval).
- create_directory: {\"root\": \"/granted/dir\", \"path\": \"relative/dir\"} — Create a directory (and parents) in a [read & write] directory (requires user approval).
- move_path: {\"root\": \"/granted/dir\", \"from\": \"old/path\", \"to\": \"new/path\"} — Move or rename a file or directory inside a [read & write] directory (requires user approval).
- delete_directory: {\"root\": \"/granted/dir\", \"path\": \"relative/dir\"} — Recursively delete a subdirectory in a [read & write] directory (requires user approval).

Guidelines:
- CRITICAL: Whenever an action requires a tool, you MUST output the <tool_call>...</tool_call> block in your response. Never claim you are performing, deleting, creating, or checking something (e.g. \"I'm deleting it now!\") without including the <tool_call> block—prose alone does not execute anything.
- Output <tool_call> directly when you need a tool—do not narrate that you are about to call a tool, and do not thank the user for tool results.
- When asked to read, inspect, or search a specific local file or folder path—even if it is not in the granted list yet—call read_file, list_files, or search_files with that exact absolute path. Companion will prompt the user for permission if needed.
- When asked to inspect a folder, list its files first and then read relevant text/Markdown/code files automatically before giving your final answer.
- When the user mentions a path with @/path/..., use read_file or list_files on that exact path if you need its contents.
- Clearly note any image or binary files found without attempting to read them with read_file.
- Directory/file write tools (write_file, delete_file, create_directory, move_path, delete_directory) are blocked on [read-only] items unless the user grants Read & Write access.";

/// Instructions appended to the system prompt when Web Search is enabled for the active chat.
pub const WEB_SEARCH_TOOL_INSTRUCTIONS: &str = "\
Web Search is enabled for this chat:
- web_search: {\"query\": \"concise search terms\"} — Search the web for up-to-date public information (always requires user approval).
- fetch_url: {\"url\": \"https://...\"} — Fetch and read the plain text of a specific HTTPS web page (always requires user approval).
- When web_search snippets lack the specific answer (such as live scores, stats, or details), automatically call fetch_url on a promising HTTPS result URL before giving your final answer. Prefer text/news/reference pages (e.g., ESPN, BBC, Wikipedia, official sites) over JS-heavy live-score apps; if one page requires JavaScript or lacks the answer, try another result URL.
- Never include local file contents, private user data, file paths, or secrets in web_search queries or fetch_url URLs. Keep queries short and generic.";
