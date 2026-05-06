//! Stub-body `HotMemoryAssembler`. Filled in by Task 11.

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AssembleHotError {}

#[allow(dead_code)]
pub fn assemble_hot(
    _config: &crate::config::HotMemoryConfig,
) -> Result<crate::generated::verbs::assemble_hot::AssembleHotData, AssembleHotError> {
    unreachable!("filled in by Task 11")
}
