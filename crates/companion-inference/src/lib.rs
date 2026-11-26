//! Inference provider abstraction and embedded `llama.cpp` backend.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub use companion_core::provider;

pub mod llama_cpp;
pub mod stop_filter;
pub mod think_parser;
