//! Vault management: bootstrap (§3.1) and registry (§3.3).

pub mod bootstrap;
pub mod registry;

pub use bootstrap::{BootstrapOpts, BootstrapReceipt, bootstrap, render_human};
