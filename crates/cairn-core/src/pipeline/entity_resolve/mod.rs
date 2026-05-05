//! Three-tier entity resolution for the bitemporal knowledge graph
//! (issue #187, brief §5.2 + §4 LLMProvider contract).
//!
//! Pipeline stage between Extract and Store: pure (Tier 1+2) and
//! async (Tier 3) functions that map a candidate entity name plus
//! a caller-provided slice of in-scope existing nodes to a
//! [`Resolution`]. No I/O; no store calls; no harness assumptions.

mod llm;
mod minhash;
mod normalize;

#[cfg(test)]
mod proptests;
