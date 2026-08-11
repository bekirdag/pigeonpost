//! # Pigeonpost MCP server
//!
//! Agent messaging as MCP tools — the primary integration path in `docs/integration.md`. This is
//! how Docdex and any other agent framework will use Pigeonpost, and it is a thin shell over
//! `pigeonpost-client` so there is exactly one implementation of the hard parts.
//!
//! Speaks JSON-RPC 2.0 over stdio: one request per line, one response per line.

#![forbid(unsafe_code)]

pub mod server;
pub mod tools;

pub use server::{handle_request, serve_stdio, serve_stdio_with_config, McpServerConfig};
