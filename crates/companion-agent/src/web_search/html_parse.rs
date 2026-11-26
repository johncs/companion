//! HTML text extraction, entity decoding, and DuckDuckGo result parsing.

use super::{MAX_RESULTS, SearchResult};

pub(super) fn extract_readable_text(html: &str) -> String {
    let mut cleaned = html.to_string();
    for tag in ["script", "style", "noscript", "svg"] {
        cleaned = strip_tag_block(&cleaned, tag);
    }
    strip_html_tags(&cleaned)
}

fn strip_tag_block(input: &str, tag: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let open_pat = format!("<{tag}");
    let close_pat = format!("</{tag}>");
    let mut out = String::with_capacity(input.len());
    let mut idx = 0;
    while idx < input.len() {
        let Some(rel_start) = lower[idx..].find(&open_pat) else {
            out.push_str(&input[idx..]);
            break;
        };
        let start = idx + rel_start;
        out.push_str(&input[idx..start]);
        if let Some(rel_end) = lower[start..].find(&close_pat) {
            idx = start + rel_end + close_pat.len();
        } else {
            break;
        }
    }
    out
}

pub(super) fn parse_duckduckgo_html(html: &str) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let mut rest = html;

    while results.len() < MAX_RESULTS {
        let Some(pos) = rest.find("class=\"result__a\"") else {
            break;
        };
        let chunk = &rest[pos..];
        let href = extract_attr_before_gt(chunk, "href").unwrap_or_default();
        let url = unwrap_ddg_redirect(&decode_html_entities(&href));

        let after_gt = chunk.find('>').map(|i| &chunk[i + 1..]).unwrap_or("");
        let title_raw = after_gt
            .find("</a>")
            .map(|end| &after_gt[..end])
            .unwrap_or("");
        let title = strip_html_tags(title_raw);

        let snippet = if let Some(spos) = chunk.find("class=\"result__snippet\"") {
            let schunk = &chunk[spos..];
            let safter = schunk.find('>').map(|i| &schunk[i + 1..]).unwrap_or("");
            let sraw = safter
                .find("</a>")
                .or_else(|| safter.find("</div>"))
                .map(|end| &safter[..end])
                .unwrap_or("");
            strip_html_tags(sraw)
        } else {
            String::new()
        };

        if !title.is_empty() && (url.starts_with("http://") || url.starts_with("https://")) {
            results.push(SearchResult {
                title,
                url,
                snippet,
            });
        }

        rest = &rest[pos + 17..];
    }

    results
}

fn extract_attr_before_gt(tag_start: &str, attr: &str) -> Option<String> {
    let gt = tag_start.find('>')?;
    let head = &tag_start[..gt];
    let needle = format!("{attr}=\"");
    let idx = head.find(&needle)?;
    let after = &head[idx + needle.len()..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

fn unwrap_ddg_redirect(raw: &str) -> String {
    if let Some(idx) = raw.find("uddg=") {
        let encoded = &raw[idx + 5..];
        let end = encoded.find('&').unwrap_or(encoded.len());
        return percent_decode(&encoded[..end]);
    }
    if raw.starts_with("//") {
        return format!("https:{raw}");
    }
    raw.to_string()
}

pub(super) fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn strip_html_tags(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_tag = false;
    for ch in raw.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    decode_html_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn decode_html_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duckduckgo_html_results() {
        let html = r#"
        <div class="result">
            <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fgtk-rs.org%2F&amp;rut=1">GTK Rust <b>Bindings</b></a>
            <a class="result__snippet">Official Rust bindings for GTK 4.</a>
        </div>
        "#;
        let parsed = parse_duckduckgo_html(html);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].title, "GTK Rust Bindings");
        assert_eq!(parsed[0].url, "https://gtk-rs.org/");
        assert_eq!(parsed[0].snippet, "Official Rust bindings for GTK 4.");
    }

    #[test]
    fn extract_readable_text_strips_scripts_and_tags() {
        let html = "<html><head><style>body{color:red}</style></head><body><script>alert(1)</script><h1>Inter Miami 2 - 1</h1><p>Final score.</p></body></html>";
        let text = extract_readable_text(html);
        assert_eq!(text, "Inter Miami 2 - 1 Final score.");
    }

    #[test]
    fn percent_decode_and_ddg_redirect_handle_edge_cases() {
        assert_eq!(percent_decode("hello+world%21%ZZ"), "hello world!%ZZ");
        assert_eq!(
            unwrap_ddg_redirect("//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Fa+b&rut=1"),
            "https://example.org/a b"
        );
        assert_eq!(
            unwrap_ddg_redirect("//example.org/direct"),
            "https://example.org/direct"
        );
    }
}
