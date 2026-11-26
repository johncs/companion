//! Parser for `<tool_call>{...}</tool_call>` and Gemma 4 `<|tool_call>call:name{...}<tool_call|>` blocks.

use serde_json::Value;

const TOOL_TAG_PAIRS: [(&str, &str); 2] = [
    ("<tool_call>", "</tool_call>"),
    ("<|tool_call>", "<tool_call|>"),
];

#[derive(Debug, Clone)]
pub struct ParsedToolCall {
    pub name: String,
    pub arguments: Value,
}

fn find_earliest_open_tag(text: &str) -> Option<(usize, &'static str, &'static str)> {
    TOOL_TAG_PAIRS
        .iter()
        .filter_map(|&(open, close)| text.find(open).map(|idx| (idx, open, close)))
        .min_by_key(|(idx, _, _)| *idx)
}

fn normalize_arguments_value(val: Value) -> Value {
    match val {
        Value::String(s) => serde_json::from_str::<Value>(s.trim()).unwrap_or(Value::String(s)),
        other => other,
    }
}

fn parse_json_tool_object(obj: &serde_json::Map<String, Value>) -> Option<ParsedToolCall> {
    let name = obj
        .get("name")
        .or_else(|| obj.get("tool"))
        .and_then(|v| v.as_str())?
        .trim();
    if name.is_empty() {
        return None;
    }
    let arguments = obj
        .get("arguments")
        .or_else(|| obj.get("args"))
        .or_else(|| obj.get("parameters"))
        .cloned()
        .map(normalize_arguments_value)
        .filter(|v| !v.is_null())
        .unwrap_or_else(|| {
            let mut flat = serde_json::Map::new();
            for (k, v) in obj {
                if !matches!(
                    k.as_str(),
                    "name" | "tool" | "arguments" | "args" | "parameters"
                ) {
                    flat.insert(k.clone(), v.clone());
                }
            }
            Value::Object(flat)
        });
    Some(ParsedToolCall {
        name: name.to_string(),
        arguments,
    })
}

fn parse_tool_payload(raw: &str) -> Option<ParsedToolCall> {
    let trimmed = raw.trim();
    if let Some(after_call) = trimmed.strip_prefix("call:") {
        let brace_idx = after_call.find('{')?;
        let name = after_call[..brace_idx].trim();
        if name.is_empty() {
            return None;
        }
        let body = after_call[brace_idx..].trim();
        let arguments: Value = serde_json::from_str(body)
            .or_else(|_| {
                let normalized = body.replace("<|\"|>", "\"");
                serde_json::from_str(&normalized)
            })
            .ok()?;
        return Some(ParsedToolCall {
            name: name.to_string(),
            arguments: normalize_arguments_value(arguments),
        });
    }
    let val: Value = serde_json::from_str(trimmed).ok()?;
    let obj = val.as_object()?;
    parse_json_tool_object(obj)
}

/// Extract all valid `<tool_call>...</tool_call>` and `<|tool_call>...<tool_call|>` blocks from `text`.
pub fn parse_tool_calls(text: &str) -> Vec<ParsedToolCall> {
    let mut calls = Vec::new();
    let mut rest = text;

    while let Some((start, open, close)) = find_earliest_open_tag(rest) {
        let after_start = &rest[start + open.len()..];
        let Some(end) = after_start.find(close) else {
            // Recover a trailing tool call if the model omitted the closing tag at end-of-turn.
            if let Some(parsed) = parse_tool_payload(after_start) {
                calls.push(parsed);
            }
            break;
        };
        if let Some(parsed) = parse_tool_payload(&after_start[..end]) {
            calls.push(parsed);
        }
        rest = &after_start[end + close.len()..];
    }

    calls
}

/// Remove completed tool-call blocks and any trailing unclosed opening tag (while streaming)
/// so raw tool JSON is not shown in chat prose.
pub fn strip_tool_calls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some((start, open, close)) = find_earliest_open_tag(rest) {
        out.push_str(&rest[..start]);
        let after_start = &rest[start + open.len()..];
        if let Some(end) = after_start.find(close) {
            rest = &after_start[end + close.len()..];
        } else {
            // Unclosed tag while streaming: hide the partial tool-call tail.
            rest = "";
            break;
        }
    }
    out.push_str(rest);

    let mut collapsed = String::with_capacity(out.len());
    let mut empty_run = 0usize;
    for line in out.lines() {
        if line.trim().is_empty() {
            empty_run += 1;
            if empty_run <= 1 && !collapsed.is_empty() {
                collapsed.push('\n');
            }
        } else {
            empty_run = 0;
            if !collapsed.is_empty() {
                collapsed.push('\n');
            }
            collapsed.push_str(line.trim_end());
        }
    }

    collapsed.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_call_blocks() {
        let text = "Let me check that file.\n<tool_call>{\"name\": \"read_file\", \"arguments\": {\"path\": \"Cargo.toml\"}}</tool_call>";
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments["path"], "Cargo.toml");
    }

    #[test]
    fn strips_complete_and_partial_tool_call_blocks() {
        let text = "Checking files.\n\n<tool_call>{\"name\": \"list_files\", \"arguments\": {\"subdir\": \".\"}}</tool_call>\n\nDone!";
        assert_eq!(strip_tool_calls(text), "Checking files.\n\nDone!");

        let streaming = "Checking files.\n<tool_call>{\"name\": \"list_";
        assert_eq!(strip_tool_calls(streaming), "Checking files.");
    }

    #[test]
    fn parses_flat_and_stringified_tool_arguments() {
        let flat = "<tool_call>{\"name\": \"read_file\", \"path\": \"/tmp/a b/c.txt\"}</tool_call>";
        let calls = parse_tool_calls(flat);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments["path"], "/tmp/a b/c.txt");

        let stringified = "<tool_call>{\"name\": \"read_file\", \"arguments\": \"{\\\"path\\\": \\\"/tmp/a.txt\\\"}\"}</tool_call>";
        let calls2 = parse_tool_calls(stringified);
        assert_eq!(calls2.len(), 1);
        assert_eq!(calls2[0].arguments["path"], "/tmp/a.txt");

        let unclosed =
            "<tool_call>{\"name\": \"delete_file\", \"arguments\": {\"path\": \"COMMENT.md\"}}";
        let calls3 = parse_tool_calls(unclosed);
        assert_eq!(calls3.len(), 1);
        assert_eq!(calls3[0].name, "delete_file");
        assert_eq!(calls3[0].arguments["path"], "COMMENT.md");
    }

    #[test]
    fn ignores_malformed_and_nested_tool_call_blocks() {
        let text = "Before <tool_call>{not json</tool_call> middle <tool_call>{\"name\":\"read_file\",\"arguments\":{\"path\":\"a.txt\"}}</tool_call> after";
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(strip_tool_calls(text), "Before  middle  after");
    }
}
