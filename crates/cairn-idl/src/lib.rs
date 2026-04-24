//! Cairn IDL source and codegen driver.
//!
//! P0 scaffold. IDL content lands here (issue #34); generators land in issue #35.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

/// Absolute path to the `schema/` directory that holds every IDL source file
/// for the `cairn.mcp.v1` contract. Downstream crates (codegen, CLI, MCP) read
/// this to locate the schema root without duplicating the path.
pub const SCHEMA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/schema");
