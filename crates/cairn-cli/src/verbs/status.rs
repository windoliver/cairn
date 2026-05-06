//! `cairn status` handler — capability discovery (§8.0.a).
//!
//! Returns the contract version, advertised capabilities, and server info.
//! For P0 (no daemon), a fresh incarnation ULID is minted per invocation.
//! When the store adapter lands, read the incarnation from the daemon table.
//!
//! Capabilities are advertised only when the runtime can honor them
//! end-to-end. The IDL declares `cairn.mcp.v1.policy_trace` (#95) and
//! store-driven search / retrieve / forget mode capabilities; verb
//! runtime emits the keyword/semantic search capabilities once the
//! store is wired and gates the others (#9 / #61 / #62) until each is
//! honored. Advertising a capability the runtime cannot back would
//! mislead clients that negotiate from `status.capabilities`.

use std::path::Path;
use std::process::ExitCode;

use cairn_core::config::{CairnConfig, EmbeddingModelKind};
use cairn_core::domain::identity::keys::VaultId;
use cairn_core::generated::common::Capabilities;
use cairn_core::generated::status::{StatusResponse, StatusResponseServerInfo};
use cairn_core::pipeline::dispatch::{DefaultRegistry, pipeline_dispatch_advertisement};

use super::envelope::{emit_json, new_operation_id};

/// Outcome of probing `<vault>/.cairn/vault.id` for the capability gate.
#[derive(Debug, PartialEq, Eq)]
pub enum VaultBinding {
    /// Sentinel exists and parses as a valid `VaultId`.
    Bound,
    /// Sentinel is absent — directory is not a Cairn vault. Fail-closed
    /// at the empty capability list, matching `Sdk::new`.
    Unbound,
    /// Sentinel exists but is not a valid `VaultId` (empty file, garbage,
    /// directory-typed path, …) or could not be read because of an I/O
    /// error other than `NotFound`. Surface as an operator-visible config
    /// error so a damaged vault is not silently treated as a fresh one.
    Invalid(String),
}

/// Probe the vault-binding sentinel without performing other I/O.
///
/// Mirrors the validation `vault/bootstrap.rs::preflight_vault_id` runs
/// on its file-only path:
/// - `vault.id` parses as a `VaultId` → `Bound`.
/// - `vault.id` is absent and no binding sentinels exist → `Unbound`
///   (directory is not a Cairn vault).
/// - `vault.id` is absent but `.cairn/vault.binding{,.pending}` exists
///   → `Invalid`. The identity layer marked this vault as bound, so the
///   missing id is a corruption that must be recovered, not a fresh
///   directory we can silently treat as unbound.
/// - `vault.id` exists but does not parse, or the read fails for a
///   reason other than `NotFound` → `Invalid` with the underlying
///   error.
///
/// Cross-DB validation (`vault_meta` row present, vault id matches
/// committed identity) is intentionally out of scope here — that
/// requires opening the `SQLite` store, which `status` must not do.
#[must_use]
pub fn probe_vault_binding(vault_root: &Path) -> VaultBinding {
    let sentinel = vault_root.join(".cairn").join("vault.id");
    match std::fs::read_to_string(&sentinel) {
        Ok(raw) => match VaultId::parse(raw.trim()) {
            Ok(_) => VaultBinding::Bound,
            Err(e) => VaultBinding::Invalid(format!("invalid vault.id: {e}")),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Missing vault.id with a binding sentinel means the identity
            // layer thinks this vault was bound — refuse to treat it as
            // a fresh / unbound directory.
            let cairn_dir = vault_root.join(".cairn");
            if cairn_dir.join("vault.binding").exists()
                || cairn_dir.join("vault.binding.pending").exists()
            {
                return VaultBinding::Invalid(format!(
                    "vault.id lost — binding sentinel exists at {} but \
                     .cairn/vault.id is missing; run `cairn identity \
                     vault-id-recover` to restore it",
                    cairn_dir.display()
                ));
            }
            VaultBinding::Unbound
        }
        Err(e) => VaultBinding::Invalid(format!("read {}: {e}", sentinel.display())),
    }
}

/// Run `cairn status`. Exits 0 on success.
#[must_use]
pub fn run(json: bool) -> ExitCode {
    run_with_context(json, None, None, false)
}

/// Run `cairn status` with optional vault root and config for capability probing.
///
/// When `vault_root` and `config` are supplied, the embedding-model presence
/// is stat-checked and `CapabilitySet::semantic_search` is wired accordingly.
/// Without them (e.g. the `bootstrap` / `vault` / `mcp` fast paths) the old
/// P0 empty list is returned.
///
/// `require_bound` — when `true`, the caller has already confirmed the
/// vault is bound (resolution source ≠ `CwdFallback`). Any non-`Bound`
/// sentinel observed here therefore indicates the binding was lost
/// between the caller's probe and this re-probe (file removed, mount
/// disappeared, race with `cairn forget --vault`); fail closed with
/// `EX_CONFIG` instead of falling through to the empty-capability path.
/// Without this flag a TOCTOU window would let a real-source vault
/// silently downgrade to `capabilities: []` + exit 0 (round-8 review #3).
#[must_use]
pub fn run_with_context(
    json: bool,
    vault_root: Option<&Path>,
    config: Option<&CairnConfig>,
    require_bound: bool,
) -> ExitCode {
    // Single binding probe per invocation — the result feeds both the
    // fail-closed gate below and `compute_capabilities` so the sentinel
    // is only stat'd once per `cairn status` call (was three times
    // previously).
    let binding = vault_root.map(probe_vault_binding);

    if let Some(b) = binding.as_ref() {
        match b {
            VaultBinding::Bound => {}
            VaultBinding::Invalid(reason) => {
                // A damaged sentinel is always fatal: treating it as
                // Unbound would silently advertise zero capabilities for
                // a vault the operator believes is bound (same class of
                // bug as the round-1 `unwrap_or_default` finding).
                eprintln!("cairn status: vault binding error — {reason}");
                return ExitCode::from(78); // EX_CONFIG
            }
            VaultBinding::Unbound => {
                if require_bound {
                    eprintln!(
                        "cairn status: vault at {} lost its binding between \
                         resolution and capability probe (.cairn/vault.id removed?) \
                         — refusing to advertise empty capabilities",
                        vault_root
                            .expect("invariant: binding probed only when vault_root is Some")
                            .display()
                    );
                    return ExitCode::from(78); // EX_CONFIG
                }
                // CwdFallback path: caller intentionally accepted an
                // unbound CWD; the empty capability list below is the
                // documented response.
            }
        }
    }

    let incarnation = new_operation_id();
    let started_at = chrono_like_now();

    let bound = matches!(binding, Some(VaultBinding::Bound));
    let caps = compute_capabilities(vault_root, config, bound);

    let resp = StatusResponse {
        contract: "cairn.mcp.v1".to_owned(),
        server_info: StatusResponseServerInfo {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            build: build_profile(),
            started_at: started_at.clone(),
            incarnation: incarnation.clone(),
        },
        capabilities: caps,
        extensions: vec![],
        // Advertise the live routing policy (issue #217). The
        // `capture_trace` verb dispatches through the same
        // `DefaultRegistry` (see `crates/cairn-cli/src/verbs/capture_trace.rs`
        // — `dispatch(event, &DefaultRegistry)`), so this advertisement
        // is exact for the runtime, not a placeholder. When a
        // deployment-supplied `ToolSchemaLookup` lands, the call here
        // and the matching call in `capture_trace` move in lockstep to
        // the live registry; the wire schema's family-granular shape
        // makes the two sides un-divergeable.
        pipeline_dispatch: Some(pipeline_dispatch_advertisement(&DefaultRegistry)),
    };

    if json {
        emit_json(&resp);
    } else {
        println!("contract:    {}", resp.contract);
        println!("version:     {}", resp.server_info.version);
        println!("build:       {}", resp.server_info.build);
        println!("started_at:  {started_at}");
        println!("incarnation: {}", incarnation.0);
        if resp.capabilities.is_empty() {
            println!("capabilities: (none advertised — store not wired in this build)");
        } else {
            for cap in &resp.capabilities {
                println!(
                    "  capability: {}",
                    serde_json::to_string(cap).unwrap_or_default()
                );
            }
        }
    }
    ExitCode::SUCCESS
}

/// Derive the `Capabilities` list from the active config and filesystem state.
///
/// `vault_root` is used only to stat-check the embedding-model directory;
/// no I/O is performed when it is `None`. `bound` is the
/// already-probed binding state from the caller — passing it in keeps
/// the binding sentinel from being re-stat'd here (and removes the
/// TOCTOU window the third probe used to open). When `config` is
/// `None` we fall back to the empty list — the IDL declares
/// `cairn.mcp.v1.policy_trace` (#95) and store-driven mode
/// capabilities, but they're advertised only once verb runtime can
/// honor them end-to-end (#9 / #61 / #62).
fn compute_capabilities(
    vault_root: Option<&Path>,
    config: Option<&CairnConfig>,
    bound: bool,
) -> Vec<Capabilities> {
    let Some(config) = config else {
        // No config available — return empty list (P0 stub path).
        return vec![];
    };

    // Vault-presence gate: only advertise capabilities when there is an
    // actual executable backend behind them. `.cairn/vault.id` is the
    // bootstrap sentinel (brief §3.1); without it the directory is not
    // a Cairn vault and every verb would fail at store-open. Mirrors
    // the SDK contract: `Sdk::new` (no store) advertises nothing,
    // `Sdk::with_store` advertises capabilities the store actually backs.
    if !bound {
        return vec![];
    }

    let model_present = vault_root.is_some_and(|root| {
        let models_root = root.join(".cairn").join("models");
        let cache = cairn_embeddings_local::ModelCache::new(&models_root);
        let kind: EmbeddingModelKind = config.search.embedding_model;
        cache.is_present(kind)
    });

    capabilities_for_config(config, model_present)
}

/// Project a [`CairnConfig`] + model-on-disk state into the wire-format
/// capability list, *without* the vault-presence gate. Use when probing
/// the per-build capability surface (e.g. `--explain` gate at parse
/// time, before any vault is resolved). Mirrors `cairn-sdk`'s
/// `Sdk::advertised_capabilities` derivation.
fn capabilities_for_config(config: &CairnConfig, model_present: bool) -> Vec<Capabilities> {
    let cap_set = config.capabilities(model_present);
    let mut out = vec![Capabilities::CairnMcpV1SearchKeyword];
    if cap_set.semantic_search {
        out.push(Capabilities::CairnMcpV1SearchSemantic);
    }
    if cap_set.hybrid_search {
        out.push(Capabilities::CairnMcpV1SearchHybrid);
    }
    // policy_trace is always true at P0 (config always sets it; a future
    // config knob could opt out). Advertise it so `--explain` is not
    // rejected by the capability gate when a vault config is loaded.
    if cap_set.policy_trace {
        out.push(Capabilities::CairnMcpV1PolicyTrace);
    }
    out
}

/// True if `capability` is in the current `status.capabilities` list.
/// Used by capability-gated args (e.g. `search --explain`) to fail closed
/// before verb dispatch when the required capability is not advertised
/// (CLAUDE.md §4.6).
///
/// Uses the P0 default config for the capability check. This includes
/// `cairn.mcp.v1.policy_trace`, which is always `true` at P0 (the config
/// unconditionally sets `policy_trace: true` — see `CairnConfig::capabilities`
/// and its tests). Semantic/hybrid search require a model on disk and are
/// therefore absent when probed without a vault root.
#[must_use]
pub fn p0_capabilities_advertises(capability: &str) -> bool {
    // Use the default config (no vault root → model not present). This
    // conservatively excludes semantic/hybrid search while including
    // policy_trace which the P0 config always enables.
    //
    // Bypasses the vault-presence gate because this is a per-build
    // capability probe (called at arg-parse time, before any vault is
    // resolved). Without that bypass, `--explain` would be rejected even
    // for users running inside a real bound vault.
    let default_config = CairnConfig::default();
    capabilities_for_config(&default_config, false)
        .iter()
        .any(|c| {
            serde_json::to_value(c)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .as_deref()
                == Some(capability)
        })
}

/// Return the current UTC time as an RFC-3339 string without sub-second precision.
fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("invariant: system clock is after Unix epoch")
        .as_secs();
    let (y, mo, d, h, mi, s) = secs_to_ymdhms(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

fn secs_to_ymdhms(mut s: u64) -> (u64, u64, u64, u64, u64, u64) {
    let sec = s % 60;
    s /= 60;
    let min = s % 60;
    s /= 60;
    let hour = s % 24;
    s /= 24;
    let mut days = s;
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let leap = is_leap(year);
    let months = [
        31u64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1u64;
    for &m in &months {
        if days < m {
            break;
        }
        days -= m;
        month += 1;
    }
    (year, month, days + 1, hour, min, sec)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

fn build_profile() -> String {
    if cfg!(debug_assertions) {
        "debug".to_owned()
    } else {
        "release".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrono_like_now_is_valid_rfc3339() {
        let now = chrono_like_now();
        // Format: YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(now.len(), 20, "RFC-3339 must be 20 chars: {now}");
        assert!(now.ends_with('Z'), "RFC-3339 must end with Z: {now}");
        assert!(
            now.contains('T'),
            "RFC-3339 must contain T separator: {now}"
        );
        // Simple validation of structure
        let parts: Vec<&str> = now.split('T').collect();
        assert_eq!(parts.len(), 2, "RFC-3339 must have exactly one T: {now}");
        let date_part = parts[0];
        assert!(date_part.contains('-'), "date must have dashes: {now}");
    }

    #[test]
    fn secs_to_ymdhms_epoch() {
        let (y, mo, d, h, mi, s) = secs_to_ymdhms(0);
        assert_eq!(y, 1970);
        assert_eq!(mo, 1);
        assert_eq!(d, 1);
        assert_eq!(h, 0);
        assert_eq!(mi, 0);
        assert_eq!(s, 0);
    }

    #[test]
    fn secs_to_ymdhms_day_boundary() {
        let (y, mo, d, h, mi, s) = secs_to_ymdhms(86400); // One day after epoch
        assert_eq!(y, 1970);
        assert_eq!(mo, 1);
        assert_eq!(d, 2);
        assert_eq!(h, 0);
        assert_eq!(mi, 0);
        assert_eq!(s, 0);
    }

    #[test]
    fn secs_to_ymdhms_dec31_non_leap() {
        // 1999-12-31 23:59:59 UTC — day before Y2K
        // secs from epoch: 946684799
        let (y, mo, d, h, mi, s) = secs_to_ymdhms(946_684_799);
        assert_eq!((y, mo, d, h, mi, s), (1999, 12, 31, 23, 59, 59));
    }

    #[test]
    fn secs_to_ymdhms_leap_day_2000() {
        // 2000-02-29 00:00:00 UTC — century leap year
        // secs from epoch: 951782400
        let (y, mo, d, h, mi, s) = secs_to_ymdhms(951_782_400);
        assert_eq!((y, mo, d), (2000, 2, 29));
        assert_eq!((h, mi, s), (0, 0, 0));
    }

    #[test]
    fn secs_to_ymdhms_year_boundary_y2k() {
        // 2000-01-01 00:00:00 UTC
        // secs from epoch: 946684800
        let (y, mo, d, h, mi, s) = secs_to_ymdhms(946_684_800);
        assert_eq!((y, mo, d, h, mi, s), (2000, 1, 1, 0, 0, 0));
    }

    #[test]
    fn is_leap_known_values() {
        assert!(is_leap(2000), "2000 is leap");
        assert!(!is_leap(1900), "1900 is not leap");
        assert!(is_leap(2004), "2004 is leap");
        assert!(!is_leap(2001), "2001 is not leap");
    }

    #[test]
    fn build_profile_returns_string() {
        let profile = build_profile();
        assert!(!profile.is_empty());
        assert!(profile == "debug" || profile == "release");
    }

    #[test]
    fn compute_capabilities_no_config_returns_empty() {
        let caps = compute_capabilities(None, None, false);
        assert!(caps.is_empty(), "no config → no capabilities");
    }

    #[test]
    fn compute_capabilities_default_config_no_vault_returns_empty() {
        let config = CairnConfig::default();
        // No vault_root + bound=false → vault-presence gate fails
        // closed → empty list. Mirrors `Sdk::new` (no store): the
        // runtime cannot honor any capability without a real vault
        // behind it.
        let caps = compute_capabilities(None, Some(&config), false);
        assert!(
            caps.is_empty(),
            "no vault root → no capabilities; got {caps:?}"
        );
    }

    #[test]
    fn compute_capabilities_unbound_vault_dir_returns_empty() {
        // A tempdir without `.cairn/vault.id` is not a Cairn vault.
        // Caller passes `bound=false`; the CLI's status surface must
        // return empty capabilities so clients do not negotiate against
        // a non-existent backend.
        let config = CairnConfig::default();
        let tmp = tempfile::tempdir().unwrap();
        let caps = compute_capabilities(Some(tmp.path()), Some(&config), false);
        assert!(
            caps.is_empty(),
            "tempdir without vault.id → empty caps; got {caps:?}"
        );
    }

    #[test]
    fn compute_capabilities_bound_vault_includes_keyword() {
        let config = CairnConfig::default();
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".cairn")).unwrap();
        std::fs::write(
            tmp.path().join(".cairn").join("vault.id"),
            b"01HZZ0000000000000000000AB\n",
        )
        .unwrap();
        let caps = compute_capabilities(Some(tmp.path()), Some(&config), true);
        assert!(
            caps.contains(&Capabilities::CairnMcpV1SearchKeyword),
            "keyword present once vault is bound; got {caps:?}"
        );
        assert!(
            !caps.contains(&Capabilities::CairnMcpV1SearchSemantic),
            "semantic absent when model not on disk; got {caps:?}"
        );
        assert!(
            !caps.contains(&Capabilities::CairnMcpV1SearchHybrid),
            "hybrid gates on model presence too (round-2 fix); got {caps:?}"
        );
    }

    #[test]
    fn compute_capabilities_local_embeddings_off_no_semantic() {
        let mut config = CairnConfig::default();
        config.search.local_embeddings = false;
        // Bind the vault so the presence gate doesn't short-circuit.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".cairn")).unwrap();
        std::fs::write(
            tmp.path().join(".cairn").join("vault.id"),
            b"01HZZ0000000000000000000AB\n",
        )
        .unwrap();
        let caps = compute_capabilities(Some(tmp.path()), Some(&config), true);
        assert!(
            !caps.contains(&Capabilities::CairnMcpV1SearchSemantic),
            "semantic absent when local_embeddings: false"
        );
        assert!(
            !caps.contains(&Capabilities::CairnMcpV1SearchHybrid),
            "hybrid absent when local_embeddings: false"
        );
    }
}
