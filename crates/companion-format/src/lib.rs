//! Markdown-to-Pango rendering, syntax highlighting, and attachment text/PDF extraction.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod attachments;
pub mod markdown;

pub use attachments::AttachmentError;
