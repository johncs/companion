//! Background attachment parsing for text/code files, PDFs, and images.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

use thiserror::Error;

use companion_core::types::StagedAttachment;

/// Typed errors produced while validating or extracting a local file attachment.
#[derive(Debug, Error)]
pub enum AttachmentError {
    #[error("File does not exist: {0}")]
    NotFound(String),
    #[error("Attachment path is not a regular file: {0}")]
    NotAFile(String),
    #[error("'{file_name}' exceeds the maximum attachment size ({max_mib} MiB)")]
    TooLarge { file_name: String, max_mib: u64 },
    #[error("'{0}' appears to be a binary file and cannot be attached as text")]
    BinaryFile(String),
    #[error("PDF '{0}' contains no extractable text (it may be scanned)")]
    EmptyPdf(String),
    #[error("Failed to read '{path}': {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Failed to extract text from PDF '{file_name}': {message}")]
    PdfExtract { file_name: String, message: String },
}

const MAX_EXTRACTED_CHARS: usize = 120_000;
const MAX_ATTACHMENT_BYTES: u64 = 16 * 1024 * 1024;

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
pub fn process_attachment_file(path: &Path) -> Result<StagedAttachment, AttachmentError> {
    if !path.exists() {
        return Err(AttachmentError::NotFound(path.display().to_string()));
    }

    let meta = fs::metadata(path).map_err(|source| AttachmentError::Io {
        path: path.display().to_string(),
        source,
    })?;
    if !meta.is_file() {
        return Err(AttachmentError::NotAFile(path.display().to_string()));
    }
    let byte_size = meta.len() as i64;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("attachment")
        .to_string();
    let mime_type = mime_for_path(path).to_string();
    let max_mib = MAX_ATTACHMENT_BYTES / (1024 * 1024);

    if meta.len() > MAX_ATTACHMENT_BYTES {
        return Err(AttachmentError::TooLarge { file_name, max_mib });
    }

    let extracted_text = match classify_path(path) {
        AttachmentKind::Image => None,
        AttachmentKind::Pdf => Some(extract_pdf_text(path, &file_name)?),
        AttachmentKind::Text => Some(extract_text_file(path, &file_name, max_mib)?),
    };

    Ok(StagedAttachment {
        file_name,
        mime_type,
        file_path: path.to_string_lossy().to_string(),
        extracted_text,
        byte_size,
    })
}

fn extract_pdf_text(path: &Path, file_name: &str) -> Result<String, AttachmentError> {
    let text = pdf_extract::extract_text(path).map_err(|err| AttachmentError::PdfExtract {
        file_name: file_name.to_string(),
        message: err.to_string(),
    })?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(AttachmentError::EmptyPdf(file_name.to_string()));
    }
    Ok(truncate_chars(trimmed, MAX_EXTRACTED_CHARS))
}

fn extract_text_file(
    path: &Path,
    file_name: &str,
    max_mib: u64,
) -> Result<String, AttachmentError> {
    let mut file = File::open(path).map_err(|source| AttachmentError::Io {
        path: file_name.to_string(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_ATTACHMENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| AttachmentError::Io {
            path: file_name.to_string(),
            source,
        })?;
    if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
        return Err(AttachmentError::TooLarge {
            file_name: file_name.to_string(),
            max_mib,
        });
    }
    if bytes.iter().take(4096).any(|&b| b == 0) {
        return Err(AttachmentError::BinaryFile(file_name.to_string()));
    }
    let text = String::from_utf8_lossy(&bytes);
    Ok(truncate_chars(&text, MAX_EXTRACTED_CHARS))
}

/// Format extracted text/PDF attachments into a prompt prefix for the user's turn.
pub fn format_attachments_for_prompt(attachments: &[StagedAttachment]) -> String {
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
    let Some((idx, _)) = s.char_indices().nth(max_chars) else {
        return s.to_string();
    };
    let mut truncated = String::with_capacity(idx + 32);
    truncated.push_str(&s[..idx]);
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
        assert!(matches!(err, AttachmentError::BinaryFile(_)));
        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn rejects_missing_directory_and_oversized_text_file_with_typed_errors() {
        let missing =
            std::env::temp_dir().join(format!("companion_missing_{}.txt", uuid::Uuid::new_v4()));
        assert!(matches!(
            process_attachment_file(&missing),
            Err(AttachmentError::NotFound(_))
        ));

        let dir = std::env::temp_dir().join(format!("companion_dir_{}.png", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let err = process_attachment_file(&dir).unwrap_err();
        assert!(matches!(err, AttachmentError::NotAFile(_)));
        let _ = fs::remove_dir_all(&dir);

        let big = std::env::temp_dir().join(format!("companion_big_{}.txt", uuid::Uuid::new_v4()));
        let f = File::create(&big).unwrap();
        f.set_len(MAX_ATTACHMENT_BYTES + 1).unwrap();
        let err = process_attachment_file(&big).unwrap_err();
        assert!(matches!(err, AttachmentError::TooLarge { .. }));
        let _ = fs::remove_file(&big);
    }
}
