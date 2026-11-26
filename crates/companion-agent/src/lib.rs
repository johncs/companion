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
pub mod session;
mod session_page;
pub mod title_gen;
pub mod tool;
pub mod tools;
pub mod web_search;
