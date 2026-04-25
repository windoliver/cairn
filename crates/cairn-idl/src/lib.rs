//! Cairn IDL source and codegen driver.
//!
//! Hosts the canonical `cairn.mcp.v1` JSON Schema files under [`SCHEMA_DIR`]
//! and the [`codegen`] pipeline that lowers them into Rust SDK types, CLI clap
//! definitions, MCP tool declarations, and the shippable Cairn skill bundle.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod codegen;

/// Absolute path to the `schema/` directory that holds every IDL source file
/// for the `cairn.mcp.v1` contract. Downstream crates (codegen, CLI, MCP) read
/// this to locate the schema root without duplicating the path.
pub const SCHEMA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/schema");
