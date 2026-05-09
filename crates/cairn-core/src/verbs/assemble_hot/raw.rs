//! `AssembleHotDataRaw` ↔ `AssembleHotData` validation bridge.
//!
//! `AssembleHotData` is generated with
//! `#[serde(try_from = "AssembleHotDataRaw", into = "AssembleHotDataRaw")]`,
//! so every deserialize path runs `TryFrom<Raw> for AssembleHotData`,
//! which calls [`validate_base`] and [`validate_segments`]. Bypass is
//! impossible — see §5/§8 of the design spec.

use super::segments::{AssembleHotValidationError, validate_base, validate_segments};
use crate::generated::verbs::assemble_hot::{AssembleHotData, AssembleHotDataRaw};

impl TryFrom<AssembleHotDataRaw> for AssembleHotData {
    type Error = AssembleHotValidationError;

    fn try_from(raw: AssembleHotDataRaw) -> Result<Self, Self::Error> {
        let data = AssembleHotData {
            bytes: raw.bytes,
            prefix: raw.prefix,
            segments: raw.segments,
            debug: raw.debug,
        };
        validate_base(&data)?;
        validate_segments(&data)?;
        Ok(data)
    }
}

impl From<AssembleHotData> for AssembleHotDataRaw {
    fn from(data: AssembleHotData) -> Self {
        AssembleHotDataRaw {
            bytes: data.bytes,
            prefix: data.prefix,
            segments: data.segments,
            debug: data.debug,
        }
    }
}
