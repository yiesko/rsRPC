//! Unit tests for the library, one module per area.
//!
//! Kept in their own folder (instead of inline `#[cfg(test)]` modules) so
//! the implementation files stay focused on implementation.
//! They run with `cargo test --lib`.

mod client_connector;
mod cmd;
mod commands;
mod ipc_utils;
mod logger;
mod process;
mod rpc_server;
mod state;
mod user;
