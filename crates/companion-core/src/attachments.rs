//! Background attachment parsing for text/code files, PDFs, and images.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use uuid::Uuid;

use crate::types::AttachmentRecord;

const MAX_EXTRACTED_CHARS: usize = 120_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Text,
    Pdf,
    Image,
}

/// Classify a file by extension.
pub fn classify_path(path: &Path) -> AttachmentKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" => AttachmentKind::Image,
        "pdf" => AttachmentKind::Pdf,
        _ => AttachmentKind::Text,
    }
}

pub fn mime_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "md" => "text/markdown",
        "json" => "application/json",
        _ => "text/plain",
    }
}

/// Read and extract text (for text/code/PDF) or validate an image file on a blocking worker thread.
pub fn process_attachment_file(path: &Path) -> Result<AttachmentRecord> {
    if !path.exists() {
        bail!("File does not exist: {}", path.display());
    }

    let meta = fs::metadata(path).with_context(|| format!("Cannot read {}", path.display()))?;
    let byte_size = meta.len() as i64;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("attachment")
        .to_string();
    let mime_type = mime_for_path(path).to_string();

    let extracted_text = match classify_path(path) {
        AttachmentKind::Image => None,
        AttachmentKind::Pdf => {
            let text = pdf_extract::extract_text(path)
                .with_context(|| format!("Failed to extract text from PDF '{file_name}'"))?;
            let trimmed = text.trim();
            if trimmed.is_empty() {
                bail!("PDF '{file_name}' contains no extractable text (it may be scanned)");
            }
            Some(truncate_chars(trimmed, MAX_EXTRACTED_CHARS))
        }
        AttachmentKind::Text => {
            let bytes = fs::read(path)
                .with_context(|| format!("Failed to read file '{file_name}'"))?;
            if bytes.iter().take(4096).any(|&b| b == 0) {
                bail!("'{file_name}' appears to be a binary file and cannot be attached as text");
            }
            let text = String::from_utf8_lossy(&bytes);
            Some(truncate_chars(&text, MAX_EXTRACTED_CHARS))
        }
    };

    Ok(AttachmentRecord {
        id: Uuid::new_v4().to_string(),
        message_id: String::new(),
        file_name,
        mime_type,
        file_path: path.to_string_lossy().to_string(),
        extracted_text,
        byte_size,
    })
}

/// Format extracted text/PDF attachments into a prompt prefix for the user's turn.
pub fn format_attachments_for_prompt(attachments: &[AttachmentRecord]) -> String {
    let mut out = String::new();
    for att in attachments {
        if let Some(text) = &att.extracted_text {
            out.push_str(&format!(
                "<attachment name=\"{}\">\n{}\n</attachment>\n\n",
                att.file_name, text
            ));
        } else if att.mime_type.starts_with("image/") {
            out.push_str(&format!(
                "[Attached image: {} ({})]\n\n",
                att.file_name, att.file_path
            ));
        }
    }
    out
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut truncated: String = s.chars().take(max_chars).collect();
    truncated.push_str("\n\n[…truncated due to length…]");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_extensions_and_extracts_text() {
        assert_eq!(classify_path(Path::new("photo.PNG")), AttachmentKind::Image);
        assert_eq!(classify_path(Path::new("doc.pdf")), AttachmentKind::Pdf);
        assert_eq!(classify_path(Path::new("main.rs")), AttachmentKind::Text);

        let tmp = std::env::temp_dir().join("companion_att_test.txt");
        fs::write(&tmp, "Hello Companion attachment!").unwrap();
        let rec = process_attachment_file(&tmp).unwrap();
        assert_eq!(rec.file_name, "companion_att_test.txt");
        assert_eq!(
            rec.extracted_text.as_deref(),
            Some("Hello Companion attachment!")
        );
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn rejects_binary_files_with_nul_bytes() {
        let tmp = std::env::temp_dir().join("companion_bin_test.bin");
        fs::write(&tmp, [0x7f, 0x45, 0x4c, 0x46, 0x00, 0x01]).unwrap();
        let err = process_attachment_file(&tmp).unwrap_err();
        assert!(err.to_string().contains("binary file"));
        let _ = fs::remove_file(&tmp);
    }
}
