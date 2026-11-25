//! Engine of the debugging assistant: transports, data pipeline and analysis.
//!
//! This crate must not depend on any GUI toolkit so that it can run headless
//! (MCP, CLI, tests) and produce identical results with and without a window.

pub mod analysis;
pub mod api;
pub mod clock;
pub mod engine;
pub mod mcp;
pub mod pipeline;
pub mod platform;
pub mod transport;
