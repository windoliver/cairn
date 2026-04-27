//! Vault management: bootstrap (§3.1) and registry (§3.3).

pub mod bootstrap;
pub mod registry;

pub use bootstrap::{BootstrapOpts, BootstrapReceipt, bootstrap, render_human};
pub use registry::{ResolveOpts, VaultError, VaultRegistryStore, add_vault, resolve_vault, walk_up_to_vault};
