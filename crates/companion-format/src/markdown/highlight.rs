//! Syntax highlighting (`syntect`) and language label helpers for Markdown code blocks.

use std::fmt::Write as _;
use std::sync::LazyLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{Style, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

use super::escape_pango_markup;

static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);

/// Pre-initialize `syntect` syntax and theme sets off the GTK main thread.
pub fn warm_syntax_highlighting() {
    let _ = &*SYNTAX_SET;
    let _ = &*THEME_SET;
}

fn resolve_syntax(lang: &str) -> &'static SyntaxReference {
    if let Some(syn) = SYNTAX_SET.find_syntax_by_token(lang) {
        return syn;
    }
    let fallback = match lang.trim().to_ascii_lowercase().as_str() {
        "typescript" | "ts" | "tsx" | "mts" | "cts" | "jsx" | "mjs" | "cjs" => "js",
        "jsonc" | "json5" => "json",
        "toml" | "ini" | "conf" => "yaml",
        "zsh" | "fish" | "ksh" | "dockerfile" | "containerfile" => "sh",
        "scss" | "sass" | "less" => "css",
        "kotlin" | "kt" | "kts" | "swift" | "dart" => "java",
        _ => "",
    };
    if !fallback.is_empty()
        && let Some(syn) = SYNTAX_SET.find_syntax_by_token(fallback)
    {
        return syn;
    }
    SYNTAX_SET.find_syntax_plain_text()
}

/// Syntax-highlight `code` into Pango-compatible `<span>` markup using the named `syntect` theme.
/// Falls back to escaped plain text if `theme_name` is not found in `THEME_SET`.
pub fn highlight_code_pango(code: &str, language: Option<&str>, theme_name: &str) -> String {
    let syntax = language
        .map(resolve_syntax)
        .unwrap_or_else(|| SYNTAX_SET.find_syntax_plain_text());

    let Some(theme) = THEME_SET.themes.get(theme_name) else {
        return escape_pango_markup(code);
    };
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut out = String::with_capacity(code.len() * 2);

    for line in LinesWithEndings::from(code) {
        match highlighter.highlight_line(line, &SYNTAX_SET) {
            Ok(ranges) => {
                for (style, text) in ranges {
                    append_styled_span(&mut out, style, text);
                }
            }
            Err(_) => {
                out.push_str(&escape_pango_markup(line));
            }
        }
    }

    out
}

fn append_styled_span(out: &mut String, style: Style, text: &str) {
    let fg = style.foreground;
    let _ = write!(
        out,
        "<span foreground=\"#{:02x}{:02x}{:02x}\">",
        fg.r, fg.g, fg.b
    );
    super::push_escaped_pango(out, text);
    out.push_str("</span>");
}

/// Formats a fenced code block's language tag for display by capitalizing its
/// first character (e.g. `"typescript"` -> `"Typescript"`, `"rust"` -> `"Rust"`).
pub fn format_code_language(language: Option<&str>) -> String {
    let raw = language
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("text");
    let mut chars = raw.chars();
    match chars.next() {
        Some(first) => {
            let mut out = String::with_capacity(raw.len());
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
            out
        }
        None => "Text".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_typescript_and_respects_light_and_dark_themes() {
        let ts_dark = highlight_code_pango(
            "const email: string = 'a@b.com';",
            Some("typescript"),
            "base16-ocean.dark",
        );
        let ts_light = highlight_code_pango(
            "const email: string = 'a@b.com';",
            Some("typescript"),
            "InspiredGitHub",
        );
        assert_ne!(ts_dark, ts_light);
        assert!(ts_dark.matches("<span foreground=").count() > 2);
    }

    #[test]
    fn unknown_theme_falls_back_to_escaped_plain_text() {
        let out = highlight_code_pango("a < b & c", Some("rust"), "nonexistent-theme");
        assert_eq!(out, "a &lt; b &amp; c");
    }

    #[test]
    fn formats_code_language_header() {
        assert_eq!(format_code_language(Some("typescript")), "Typescript");
        assert_eq!(format_code_language(Some("rust")), "Rust");
        assert_eq!(format_code_language(Some("  python ")), "Python");
        assert_eq!(format_code_language(None), "Text");
        assert_eq!(format_code_language(Some("")), "Text");
    }
}
