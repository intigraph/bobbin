//! MCP (Model Context Protocol) server implementation for Bobbin.
//!
//! This module exposes Bobbin's code search and analysis capabilities via the
//! Model Context Protocol, allowing AI agents (Claude, Cursor) to use Bobbin as a tool.

mod bead_response;
mod knowledge_tools;
mod local_graph_tools;
mod remote;
mod server;
mod tool_annotations;
mod tools;

pub use server::{run_http_server, run_server};
