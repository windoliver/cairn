//! IDL loader — reads `index.json` and every file it references, runs
//! structural validation, returns raw `serde_json::Value` per file.

#![allow(clippy::module_name_repetitions)]
