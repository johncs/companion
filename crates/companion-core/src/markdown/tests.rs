use super::parser::{BlockState, CodeBlockBuilder, MarkdownBuilder};
use super::*;

#[test]
fn parses_text_and_fenced_code_blocks() {
    let md = "Hello **world**!\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n";
    let blocks = parse_markdown_blocks(md);
    assert_eq!(blocks.len(), 2);
    match &blocks[0] {
        MarkdownBlock::Text(t) => assert!(t.contains("<b>world</b>")),
        _ => panic!("expected text block"),
    }
    match &blocks[1] {
        MarkdownBlock::Code { language, code, .. } => {
            assert_eq!(language.as_deref(), Some("rust"));
            assert!(code.contains("fn main()"));
        }
        _ => panic!("expected code block"),
    }
}

#[test]
fn handles_unclosed_streaming_code_block() {
    let md = "Working on it:\n\n```python\nprint(1 < 2 & 3)";
    let blocks = parse_markdown_blocks(md);
    assert_eq!(blocks.len(), 2);
    assert!(matches!(blocks[1], MarkdownBlock::Code { .. }));

    let mut cache = CodeHighlightCache::new();
    let streaming_blocks = parse_markdown_blocks_streaming(md, &mut cache);
    assert!(cache.is_empty());
    match &streaming_blocks[1] {
        MarkdownBlock::Code {
            light_pango_markup,
            dark_pango_markup,
            ..
        } => {
            assert_eq!(light_pango_markup, "print(1 &lt; 2 &amp; 3)");
            assert_eq!(dark_pango_markup, "print(1 &lt; 2 &amp; 3)");
        }
        _ => panic!("expected code block"),
    }
}

#[test]
fn finish_respects_skip_unclosed_highlight_when_code_block_still_open() {
    let mut cache = CodeHighlightCache::new();
    let mut builder = MarkdownBuilder::new("```rust\nlet x = a < b;", &mut cache, true);
    builder.state = BlockState::Code(CodeBlockBuilder {
        language: Some("rust".to_string()),
        buf: "let x = a < b;\n".to_string(),
    });

    let blocks = builder.finish();
    assert!(cache.is_empty());
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
        MarkdownBlock::Code {
            light_pango_markup,
            dark_pango_markup,
            ..
        } => {
            assert_eq!(light_pango_markup, "let x = a &lt; b;");
            assert_eq!(dark_pango_markup, "let x = a &lt; b;");
        }
        _ => panic!("expected code block"),
    }
}

#[test]
fn parses_tables_quotes_rules_and_task_lists() {
    let md = "> A **wise** note\n\n---\n\n| Name | Role |\n| --- | --- |\n| Ada | `admin` |\n\n- [x] Done\n- [ ] Todo\n";
    let blocks = parse_markdown_blocks(md);
    assert!(matches!(&blocks[0], MarkdownBlock::Quote(q) if q.contains("<b>wise</b>")));
    assert!(matches!(&blocks[1], MarkdownBlock::Rule));
    match &blocks[2] {
        MarkdownBlock::Table { headers, rows } => {
            assert_eq!(headers, &vec!["Name".to_string(), "Role".to_string()]);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0][0], "Ada");
            assert!(rows[0][1].contains("<tt>") && rows[0][1].contains("admin"));
        }
        _ => panic!("expected table block"),
    }
    match &blocks[3] {
        MarkdownBlock::Text(t) => {
            assert!(t.contains("☑") && t.contains("Done"));
            assert!(t.contains("☐") && t.contains("Todo"));
        }
        _ => panic!("expected task list text block"),
    }
}

#[test]
fn reuses_prepopulated_code_highlight_cache() {
    let mut cache = CodeHighlightCache::new();
    cache.insert(
        (Some("rust".to_string()), "let x = 1;".to_string()),
        ("cached-light".to_string(), "cached-dark".to_string()),
    );
    let blocks = parse_markdown_blocks_cached("```rust\nlet x = 1;\n```", &mut cache);
    assert_eq!(blocks.len(), 1);
    match &blocks[0] {
        MarkdownBlock::Code {
            light_pango_markup,
            dark_pango_markup,
            ..
        } => {
            assert_eq!(light_pango_markup, "cached-light");
            assert_eq!(dark_pango_markup, "cached-dark");
        }
        _ => panic!("expected code block"),
    }
}

#[test]
fn closes_unclosed_inline_tags_and_handles_tilde_fences() {
    let blocks = parse_markdown_blocks("**bold\n\n~~~sh\necho hi\n~~~");
    assert_eq!(blocks.len(), 2);
    match &blocks[0] {
        MarkdownBlock::Text(t) => assert!(t.contains("<b>bold</b>") || t.contains("**bold")),
        _ => panic!("expected text block"),
    }
    assert!(matches!(
        &blocks[1],
        MarkdownBlock::Code {
            language: Some(lang),
            code,
            ..
        } if lang == "sh" && code == "echo hi"
    ));
}

#[test]
fn escapes_pango_markup_special_chars_and_unicode() {
    assert_eq!(escape_pango_markup(""), "");
    assert_eq!(
        escape_pango_markup(r#"a & b <tag> "q" 's' 🚀"#),
        "a &amp; b &lt;tag&gt; &quot;q&quot; &apos;s&apos; 🚀"
    );
}

#[test]
fn parses_nested_lists_in_blockquotes_and_unclosed_table_inline() {
    let md = "> - item 1\n> - item 2\n\n| Col1 | Col2 |\n| --- | --- |\n| **bold | `code |\n";
    let blocks = parse_markdown_blocks(md);
    assert!(
        matches!(&blocks[0], MarkdownBlock::Quote(q) if q.contains("item 1") && q.contains("item 2"))
    );
    assert!(
        matches!(&blocks[1], MarkdownBlock::Table { headers, rows } if headers.len() == 2 && rows.len() == 1)
    );
}
