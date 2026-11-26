//! Pure OS path mention extraction for system-prompt hints (never auto-grants access).

use std::path::{Path, PathBuf};

pub(super) fn extract_mentioned_os_paths(text: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for token in text.split_whitespace() {
        let Some(raw) = token.strip_prefix('@') else {
            continue;
        };
        let cleaned = raw.trim_end_matches([',', '.', ';', ':', ')', ']', '"', '\'']);
        if !cleaned.starts_with('/') {
            continue;
        }
        let path = Path::new(cleaned);
        if (path.is_dir() || path.is_file())
            && let Ok(canon) = std::fs::canonicalize(path)
            && !out.contains(&canon)
        {
            out.push(canon);
        }
    }
    out
}

/// Detect existing absolute OS paths (including ones containing spaces) in free-form user text.
/// Used only as a system-prompt hint so the model passes the exact path; never auto-grants access.
pub(super) fn detect_existing_os_paths_in_text(text: &str) -> Vec<PathBuf> {
    const MAX_STAT_CHECKS: usize = 32;
    let mut out = Vec::new();
    let mut stat_checks = 0usize;
    if text.len() > 8192 {
        return out;
    }
    for line in text.lines() {
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() && stat_checks < MAX_STAT_CHECKS {
            if bytes[i] == b'/'
                && (i == 0
                    || matches!(
                        bytes[i - 1],
                        b' ' | b'\t' | b'"' | b'\'' | b'(' | b'[' | b'@'
                    ))
            {
                let rest = &line[i..];
                let mut matched_len = 0usize;
                let mut end_indices: Vec<usize> = rest
                    .char_indices()
                    .filter_map(|(idx, ch)| {
                        if ch.is_whitespace() || matches!(ch, '"' | '\'' | '?' | ',' | ')' | ']') {
                            Some(idx)
                        } else {
                            None
                        }
                    })
                    .take(12)
                    .collect();
                end_indices.push(rest.len());

                for &end in end_indices.iter().rev() {
                    if stat_checks >= MAX_STAT_CHECKS {
                        break;
                    }
                    let candidate = rest[..end]
                        .trim()
                        .trim_end_matches(['?', ',', '.', ';', ':', ')', ']', '"', '\'']);
                    if candidate.len() > 1 {
                        stat_checks += 1;
                        let p = Path::new(candidate);
                        if (p.is_file() || p.is_dir())
                            && let Ok(canon) = std::fs::canonicalize(p)
                        {
                            if !out.contains(&canon) {
                                out.push(canon);
                            }
                            matched_len = end;
                            break;
                        }
                    }
                }
                if matched_len > 0 {
                    i += matched_len;
                    continue;
                }
            }
            i += line[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_at_mentions_and_paths_with_spaces() {
        let dir = std::env::temp_dir().join(format!("companion_pm_{}", uuid::Uuid::new_v4()));
        let spaced = dir.join("My Folder");
        std::fs::create_dir_all(&spaced).unwrap();
        let plain_file = dir.join("plain.txt");
        let spaced_file = spaced.join("notes.txt");
        std::fs::write(&plain_file, "hi").unwrap();
        std::fs::write(&spaced_file, "hi").unwrap();

        let msg1 = format!("Check @{}.", plain_file.display());
        assert_eq!(
            extract_mentioned_os_paths(&msg1),
            vec![std::fs::canonicalize(&plain_file).unwrap()]
        );

        let msg2 = format!("Please inspect {} when you can?", spaced_file.display());
        assert_eq!(
            detect_existing_os_paths_in_text(&msg2),
            vec![std::fs::canonicalize(&spaced_file).unwrap()]
        );

        assert!(extract_mentioned_os_paths("hello @/definitely/missing/file").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
