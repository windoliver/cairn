//! `cairn assemble_hot` handler.
#![allow(
    clippy::result_large_err,
    reason = "CLI helpers return complete response envelopes for direct JSON emission"
)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use cairn_core::config::{CairnConfig, HotMemoryRecipeStep};
use cairn_core::contract::identity_registry::IdentityVisibility;
use cairn_core::contract::memory_store::{ListArgs, MemoryStore};
use cairn_core::domain::canonical::canonical_bytes_signed_intent;
use cairn_core::domain::consent_timeline::ConsentModel;
use cairn_core::domain::identity::keys::SecretHandle;
use cairn_core::domain::{
    Identity, MemoryKind, MemoryRecord, MemoryVisibility, RecordId, ScopeTuple,
};
use cairn_core::generated::common::{Ed25519Signature, Ulid};
use cairn_core::generated::envelope::{
    RequestArgs, RequestVerb, Response, ResponseData, ResponsePolicyTrace,
    ResponsePolicyTraceResult, ResponseStatus, ResponseVerb, SignedIntent, SignedIntentScope,
    SignedIntentScopeTier,
};
use cairn_core::generated::verbs::assemble_hot::AssembleHotArgs;
use clap::ArgMatches;
use sha2::Digest as _;

use super::envelope::{
    emit_json, human_error, internal_error_response, invalid_args_response, new_operation_id,
};

const DEFAULT_ASSEMBLE_ISSUER: &str = "agt:cairn-cli:default:writer:v1";
const DEFAULT_TENANT: &str = "default";
const ASSEMBLE_ENTITY: &str = "ingest";

/// In-process fallback cache used when `SqliteHotPrefixCache::open`
/// fails (e.g., transient `SQLite` error during runtime). Always misses
/// and always returns default watermarks so `cached_assemble` falls
/// through to direct assembly.
#[derive(Debug, Default)]
struct NoopHotPrefixCache;

#[async_trait::async_trait]
impl cairn_core::contract::hot_prefix_cache::HotPrefixCache for NoopHotPrefixCache {
    async fn current_watermarks(
        &self,
    ) -> Result<
        cairn_core::domain::hot_prefix::SourceWatermarks,
        cairn_core::contract::hot_prefix_cache::CacheError,
    > {
        Ok(cairn_core::domain::hot_prefix::SourceWatermarks::default())
    }

    async fn get(
        &self,
        _agent: &cairn_core::domain::Identity,
        _recipe_hash: &str,
    ) -> Result<
        Option<cairn_core::contract::hot_prefix_cache::CachedPrefix>,
        cairn_core::contract::hot_prefix_cache::CacheError,
    > {
        Ok(None)
    }

    async fn put(
        &self,
        _agent: &cairn_core::domain::Identity,
        _recipe_hash: &str,
        _entry: &cairn_core::contract::hot_prefix_cache::CachedPrefix,
    ) -> Result<(), cairn_core::contract::hot_prefix_cache::CacheError> {
        Ok(())
    }

    async fn bump(
        &self,
        _classes: &[cairn_core::domain::hot_prefix::SourceClass],
    ) -> Result<
        cairn_core::domain::hot_prefix::SourceWatermarks,
        cairn_core::contract::hot_prefix_cache::CacheError,
    > {
        Ok(cairn_core::domain::hot_prefix::SourceWatermarks::default())
    }
}

struct ReadAuthorization {
    operation_id: Ulid,
    scope: ScopeTuple,
    max_visibility: MemoryVisibility,
    issuer: Identity,
}

struct LoadedHotBodies {
    bodies: Vec<String>,
    files: usize,
    records: Vec<LoadedRecordTrace>,
}

#[derive(Clone)]
struct LoadedRecordTrace {
    record_id: RecordId,
    consent_model: Option<ConsentModel>,
}

impl From<&MemoryRecord> for LoadedRecordTrace {
    fn from(record: &MemoryRecord) -> Self {
        Self {
            record_id: record.id.clone(),
            consent_model: record.consent_model,
        }
    }
}

fn reject_invalid_args(json: bool, field: &str, reason: &str) -> ExitCode {
    let resp = invalid_args_response(ResponseVerb::AssembleHot, field, reason);
    if json {
        emit_json(&resp);
    } else {
        human_error("assemble_hot", "InvalidArgs", reason, &resp.operation_id);
    }
    ExitCode::from(78) // EX_CONFIG
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr = vec![0; b_chars.len() + 1];
    for (i, a_ch) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, b_ch) in b_chars.iter().enumerate() {
            let cost = usize::from(a_ch != *b_ch);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b_chars.len()]
}

fn nearest_recipe_name<'a>(requested: &str, names: &[&'a str]) -> Option<&'a str> {
    // Case-insensitive matching so a typo like `DEBUG` suggests `debug`.
    let lowered = requested.to_lowercase();
    names
        .iter()
        .copied()
        .min_by_key(|candidate| edit_distance(&lowered, &candidate.to_lowercase()))
}

/// Run `cairn assemble_hot`.
#[must_use]
pub fn run(sub: &ArgMatches, vault_root: PathBuf, mut config: CairnConfig) -> ExitCode {
    let json = sub.get_flag("json");
    // `--recipe <name>` selects a named preset from
    // `vault.hot_memory.recipes`. Resolve it up-front: the resolved
    // recipe name becomes the new `default_recipe` so every downstream
    // call to `config.vault.hot_memory.resolve_recipe(None)` selects
    // it, and the flat `recipe`/`max_bytes` fields are synchronized
    // for the upstream loader that still reads them directly.
    let requested_recipe = sub.get_one::<String>("recipe").map(String::as_str);
    let Some(resolved) = config.vault.hot_memory.resolve_recipe(requested_recipe) else {
        let names = config.vault.hot_memory.recipe_names();
        let hint = requested_recipe
            .and_then(|name| nearest_recipe_name(name, &names))
            .map(|name| format!("; did you mean {name:?}?"))
            .unwrap_or_default();
        let requested = requested_recipe.unwrap_or(&config.vault.hot_memory.default_recipe);
        let reason = format!("unknown recipe {requested:?}{hint}");
        return reject_invalid_args(json, "recipe", &reason);
    };
    let resolved_name = resolved.name.clone();
    let resolved_steps = resolved.steps.to_vec();
    let resolved_max = resolved.max_bytes;
    config.vault.hot_memory.default_recipe = resolved_name;
    config.vault.hot_memory.recipe = resolved_steps;
    config.vault.hot_memory.max_bytes = resolved_max;

    let args = match assemble_args_from_matches(sub) {
        Ok(args) => args,
        Err(resp) => {
            if json {
                emit_json(&resp);
            } else {
                emit_human(&resp);
            }
            return ExitCode::from(64);
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let resp =
                internal_error_response(ResponseVerb::AssembleHot, &format!("runtime build: {e}"));
            if json {
                emit_json(&resp);
            } else {
                emit_human(&resp);
            }
            return ExitCode::FAILURE;
        }
    };

    let resp = rt.block_on(run_async(args, vault_root, config));
    if json {
        emit_json(&resp);
    } else {
        emit_human(&resp);
    }
    response_exit_code(&resp)
}

#[allow(
    clippy::too_many_lines,
    reason = "dispatch fans through bootstrap, cache open, pre-load snapshot, body load, cached_assemble, explain"
)]
async fn run_async(args: AssembleHotArgs, vault_root: PathBuf, config: CairnConfig) -> Response {
    let ctx =
        match super::signed::open_context(ResponseVerb::AssembleHot, &vault_root, config).await {
            Ok(ctx) => ctx,
            Err(resp) => return resp,
        };
    let operation_id = new_operation_id();
    let auth = match signed_read_authorization(&ctx, &args, operation_id).await {
        Ok(auth) => auth,
        Err(resp) => return resp,
    };
    let budget = effective_assemble_budget(&ctx.config.vault.hot_memory, args.budget);
    if ctx.config.vault.hot_memory.recipe.len() > cairn_core::verbs::assemble_hot::MAX_SEGMENTS {
        return super::signed::aborted(
            ResponseVerb::AssembleHot,
            "assemble_hot recipe exceeds max segment count",
        );
    }

    // Open cache + metrics sink. Both failures degrade gracefully:
    // on cache open failure, fall back to direct assembly (no caching
    // for this call). On metrics open failure, use a no-op sink so
    // the verb still succeeds.
    let cache: Box<dyn cairn_core::contract::hot_prefix_cache::HotPrefixCache> =
        match cairn_store_sqlite::SqliteHotPrefixCache::open(&ctx.vault_root).await {
            Ok(c) => Box::new(c),
            Err(e) => {
                tracing::warn!(error = %e, "hot-prefix cache open failed; bypassing for this call");
                Box::new(NoopHotPrefixCache)
            }
        };
    let metrics: Box<dyn cairn_core::contract::metrics::MetricsSink> =
        match crate::metrics::JsonlMetricsSink::open(&ctx.vault_root).await {
            Ok(s) => Box::new(s),
            Err(e) => {
                tracing::warn!(error = %e, "metrics sink open failed; using noop");
                Box::new(cairn_core::contract::metrics::NoopMetricsSink)
            }
        };

    // Codex review round 3 finding 1: capture (watermarks, fs_fingerprint)
    // BEFORE load_hot_bodies. cached_assemble compares this pre-load
    // snapshot to its own post-assembly snapshot; any mutation that
    // commits during body loading or assembly is detected and the put
    // is skipped to avoid poisoning the cache.
    let pre_load = cairn_core::verbs::assemble_hot::cached::pre_load_snapshot(
        cache.as_ref(),
        Some(&ctx.vault_root),
    )
    .await
    .ok();

    let loaded = match load_hot_bodies(
        &ctx.store,
        &ctx.vault_root,
        &ctx.config,
        &auth,
        args.session_id.as_deref(),
        budget,
    )
    .await
    {
        Ok(loaded) => loaded,
        Err(resp) => return merge_policy_trace(read_policy_trace(&auth, 0, &[]), resp),
    };
    let policy_trace = read_policy_trace(&auth, loaded.files, &loaded.records);
    record_access(&ctx.store, &loaded.records, "assemble_hot").await;

    let vault_id = std::fs::read_to_string(ctx.vault_root.join(".cairn/vault.id"))
        .unwrap_or_default()
        .trim()
        .to_owned();

    // Bodies are loaded eagerly so the policy trace records the same
    // records on hit and miss. A future refactor could push body
    // loading into the cached_assemble closure to skip the work on
    // hits — see issue #83 follow-up.
    let bodies_for_closure = loaded.bodies;

    match cairn_core::verbs::assemble_hot::cached::cached_assemble(
        &ctx.config.vault.hot_memory,
        &auth.issuer,
        &vault_id,
        Some(&ctx.vault_root),
        args.session_id.as_deref(),
        pre_load.as_ref(),
        cache.as_ref(),
        metrics.as_ref(),
        Some(budget),
        move || Ok(bodies_for_closure),
    )
    .await
    {
        Ok(mut data) => {
            // `--explain` (Args.explain) layers a typed per-step debug
            // trace on top of the assembled prefix. The trace runs the
            // pure source modules + admissibility predicate over the
            // same per-kind candidate sets the CLI just queried; the
            // prefix bytes themselves still come from the auth-aware
            // loader above. Bodies and trace use the same record set,
            // but the source modules' top-K caps may differ slightly
            // from the loader's byte-trim heuristic — acceptable for
            // a debug surface.
            if args.explain.unwrap_or(false) {
                match build_explain_debug(
                    &ctx.store,
                    &ctx.vault_root,
                    &ctx.config,
                    &auth,
                    args.session_id.as_deref(),
                    budget,
                )
                .await
                {
                    Ok(debug) => data.debug = Some(debug),
                    Err(resp) => return merge_policy_trace(policy_trace, resp),
                }
            }
            super::signed::committed(
                ResponseVerb::AssembleHot,
                auth.operation_id,
                ResponseData::AssembleHot(data),
                policy_trace,
            )
        }
        Err(e) => {
            let resp =
                super::signed::aborted(ResponseVerb::AssembleHot, format!("assemble_hot: {e}"));
            merge_policy_trace(policy_trace, resp)
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "hot-memory recipe loading is a linear dispatch over configured recipe steps"
)]
async fn load_hot_bodies(
    store: &cairn_store_sqlite::SqliteMemoryStore,
    vault_root: &Path,
    config: &CairnConfig,
    auth: &ReadAuthorization,
    session_id: Option<&str>,
    budget: u64,
) -> Result<LoadedHotBodies, Response> {
    let mut bodies = Vec::with_capacity(config.vault.hot_memory.recipe.len());
    let mut loaded_records = Vec::new();
    let mut loaded_files = 0_usize;
    let mut used_bytes = 0_u64;

    for step in &config.vault.hot_memory.recipe {
        let remaining = budget.saturating_sub(used_bytes);
        let mut body = match step {
            HotMemoryRecipeStep::Purpose => {
                loaded_files += 1;
                cairn_core::verbs::assemble_hot::loader::read_vault_markdown_file(
                    vault_root,
                    Path::new("purpose.md"),
                    remaining,
                )
                .map_err(|e| {
                    internal_error_response(
                        ResponseVerb::AssembleHot,
                        &format!("read purpose.md: {e}"),
                    )
                })?
            }
            HotMemoryRecipeStep::Index => {
                loaded_files += 1;
                cairn_core::verbs::assemble_hot::loader::read_vault_markdown_file(
                    vault_root,
                    Path::new("index.md"),
                    remaining,
                )
                .map_err(|e| {
                    internal_error_response(
                        ResponseVerb::AssembleHot,
                        &format!("read index.md: {e}"),
                    )
                })?
            }
            HotMemoryRecipeStep::PinnedFeedback => {
                if remaining == 0 {
                    String::new()
                } else {
                    let records = load_records_for_kinds(
                        store,
                        &[MemoryKind::User, MemoryKind::Feedback],
                        auth,
                        None,
                        16,
                    )
                    .await?;
                    let records = records
                        .into_iter()
                        .filter(is_pinned_record)
                        .collect::<Vec<_>>();
                    loaded_records.extend(records.iter().map(LoadedRecordTrace::from));
                    render_records_section("Pinned Feedback", &records, remaining)
                }
            }
            HotMemoryRecipeStep::TopSalienceProject => {
                if remaining == 0 {
                    String::new()
                } else {
                    let mut records =
                        load_records_for_kinds(store, &[MemoryKind::Project], auth, None, 32)
                            .await?;
                    records.sort_by(|a, b| {
                        b.salience
                            .partial_cmp(&a.salience)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    records.truncate(6);
                    loaded_records.extend(records.iter().map(LoadedRecordTrace::from));
                    render_records_section("Project Memory", &records, remaining)
                }
            }
            HotMemoryRecipeStep::ActivePlaybook => {
                if remaining == 0 {
                    String::new()
                } else {
                    let mut records =
                        load_records_for_kinds(store, &[MemoryKind::Playbook], auth, None, 16)
                            .await?;
                    records.sort_by(|a, b| b.updated_at.as_str().cmp(a.updated_at.as_str()));
                    records.truncate(1);
                    loaded_records.extend(records.iter().map(LoadedRecordTrace::from));
                    render_records_section("Active Playbook", &records, remaining)
                }
            }
            HotMemoryRecipeStep::RecentUserSignal => {
                if remaining == 0 {
                    String::new()
                } else {
                    let mut records = load_records_for_kinds(
                        store,
                        &[MemoryKind::UserSignal],
                        auth,
                        session_id,
                        16,
                    )
                    .await?;
                    records.sort_by(|a, b| b.updated_at.as_str().cmp(a.updated_at.as_str()));
                    records.truncate(5);
                    loaded_records.extend(records.iter().map(LoadedRecordTrace::from));
                    render_records_section("Recent User Signal", &records, remaining)
                }
            }
            _ => {
                return Err(internal_error_response(
                    ResponseVerb::AssembleHot,
                    "unsupported hot-memory recipe step",
                ));
            }
        };
        truncate_body_to_budget(&mut body, remaining);
        used_bytes = used_bytes.saturating_add(body.len() as u64);
        bodies.push(body);
    }

    Ok(LoadedHotBodies {
        bodies,
        files: loaded_files,
        records: loaded_records,
    })
}

fn effective_assemble_budget(
    config: &cairn_core::config::HotMemoryConfig,
    budget_override: Option<u64>,
) -> u64 {
    budget_override
        .unwrap_or_else(|| u64::from(config.max_bytes))
        .min(u64::from(config.max_bytes))
        .min(cairn_core::verbs::assemble_hot::segments::MAX_BYTES)
}

fn truncate_body_to_budget(body: &mut String, budget: u64) {
    if body.len() as u64 <= budget {
        return;
    }
    let mut end = usize::try_from(budget).unwrap_or(0).min(body.len());
    while !body.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    body.truncate(end);
}

/// Build the `--explain` debug payload by re-running the pure source
/// modules (`assemble_hot_with_inputs`) over the same per-kind record
/// set the loader queried. The payload contains per-step
/// inclusion + redacted exclusion traces; the prefix bytes returned
/// to the caller still come from the auth-aware loader above.
async fn build_explain_debug(
    store: &cairn_store_sqlite::SqliteMemoryStore,
    vault_root: &Path,
    config: &CairnConfig,
    auth: &ReadAuthorization,
    session_id: Option<&str>,
    budget: u64,
) -> Result<cairn_core::generated::verbs::assemble_hot::HotMemoryDebug, Response> {
    use cairn_core::verbs::assemble_hot::loader::read_vault_markdown_file;
    use cairn_core::verbs::assemble_hot::{HotMemoryInputs, assemble_hot_with_inputs};

    let pinned_records = load_records_for_kinds(
        store,
        &[MemoryKind::User, MemoryKind::Feedback],
        auth,
        None,
        64,
    )
    .await?;
    let project_records =
        load_records_for_kinds(store, &[MemoryKind::Project], auth, None, 64).await?;
    let playbook_records =
        load_records_for_kinds(store, &[MemoryKind::Playbook], auth, None, 64).await?;
    let signal_records =
        load_records_for_kinds(store, &[MemoryKind::UserSignal], auth, session_id, 64).await?;

    let purpose_md = read_vault_markdown_file(vault_root, Path::new("purpose.md"), budget)
        .or_else(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(String::new())
            } else {
                Err(e)
            }
        })
        .map_err(|e| {
            internal_error_response(
                ResponseVerb::AssembleHot,
                &format!("explain read purpose.md: {e}"),
            )
        })?;
    let index_md = read_vault_markdown_file(vault_root, Path::new("index.md"), budget)
        .or_else(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Ok(String::new())
            } else {
                Err(e)
            }
        })
        .map_err(|e| {
            internal_error_response(
                ResponseVerb::AssembleHot,
                &format!("explain read index.md: {e}"),
            )
        })?;

    let pinned_refs: Vec<&MemoryRecord> = pinned_records.iter().collect();
    let project_refs: Vec<&MemoryRecord> = project_records.iter().collect();
    let playbook_refs: Vec<&MemoryRecord> = playbook_records.iter().collect();
    let signal_refs: Vec<&MemoryRecord> = signal_records.iter().collect();

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    #[allow(
        clippy::cast_possible_wrap,
        reason = "unix seconds fit in i64 for all practical dates"
    )]
    let secs_i64 = now_secs as i64;
    let now = cairn_core::domain::Rfc3339Timestamp::from_unix_secs(secs_i64).unwrap_or_else(|_| {
        cairn_core::domain::Rfc3339Timestamp::parse("1970-01-01T00:00:00Z").expect("epoch literal")
    });

    let auth_vis = effective_explain_visibility(auth.max_visibility);
    let inputs = HotMemoryInputs {
        purpose_md: &purpose_md,
        index_md: &index_md,
        pinned_candidates: &pinned_refs,
        project_candidates: &project_refs,
        playbook_candidates: &playbook_refs,
        rolling_summary_candidates: &[],
        user_signal_candidates: &signal_refs,
        now,
        scope: auth.scope.clone(),
        authorized_visibility: &auth_vis,
        include_debug: true,
    };

    let mut explain_cfg = config.vault.hot_memory.clone();
    if let Ok(b) = u32::try_from(budget) {
        explain_cfg.max_bytes = b.min(explain_cfg.max_bytes);
    }

    match assemble_hot_with_inputs(&inputs, &explain_cfg) {
        Ok(data) => {
            Ok(data
                .debug
                .unwrap_or(cairn_core::generated::verbs::assemble_hot::HotMemoryDebug {
                    steps: Vec::new(),
                }))
        }
        Err(e) => Err(internal_error_response(
            ResponseVerb::AssembleHot,
            &format!("explain assemble: {e}"),
        )),
    }
}

/// Visibility tiers admitted to the explain trace. Mirrors the tiers
/// `load_records_for_kinds` will populate (Project / Private / Session)
/// — never broader than `auth.max_visibility`.
fn effective_explain_visibility(max: MemoryVisibility) -> Vec<MemoryVisibility> {
    let cap = effective_read_visibility(max);
    [
        MemoryVisibility::Private,
        MemoryVisibility::Session,
        MemoryVisibility::Project,
        MemoryVisibility::Team,
        MemoryVisibility::Org,
        MemoryVisibility::Public,
    ]
    .into_iter()
    .filter(|v| *v <= cap)
    .collect()
}

async fn load_records_for_kinds(
    store: &cairn_store_sqlite::SqliteMemoryStore,
    kinds: &[MemoryKind],
    auth: &ReadAuthorization,
    session_id: Option<&str>,
    limit: usize,
) -> Result<Vec<MemoryRecord>, Response> {
    let mut out = Vec::new();
    for kind in kinds {
        let mut base_scope = auth.scope.clone();
        if matches!(kind, MemoryKind::UserSignal) {
            base_scope.session_id = session_id.map(str::to_owned);
        }

        out.extend(
            list_records_for_visibility(
                store,
                *kind,
                base_scope.clone(),
                MemoryVisibility::Project,
                auth,
                session_id,
                limit,
            )
            .await?,
        );

        if let Some(private_scope) = principal_scoped_query(base_scope.clone(), &auth.issuer) {
            out.extend(
                list_records_for_visibility(
                    store,
                    *kind,
                    private_scope,
                    MemoryVisibility::Private,
                    auth,
                    session_id,
                    limit,
                )
                .await?,
            );
        }

        if let Some(session_id) = session_id
            && let Some(mut session_scope) = principal_scoped_query(base_scope, &auth.issuer)
        {
            session_scope.session_id = Some(session_id.to_owned());
            out.extend(
                list_records_for_visibility(
                    store,
                    *kind,
                    session_scope,
                    MemoryVisibility::Session,
                    auth,
                    Some(session_id),
                    limit,
                )
                .await?,
            );
        }
    }
    Ok(out)
}

async fn list_records_for_visibility(
    store: &cairn_store_sqlite::SqliteMemoryStore,
    kind: MemoryKind,
    scope: ScopeTuple,
    visibility: MemoryVisibility,
    auth: &ReadAuthorization,
    session_id: Option<&str>,
    limit: usize,
) -> Result<Vec<MemoryRecord>, Response> {
    if visibility > effective_read_visibility(auth.max_visibility) {
        return Ok(Vec::new());
    }
    let page = store
        .list(&ListArgs {
            kind: Some(kind),
            scope: Some(scope),
            visibility_allowlist: vec![visibility],
            limit,
            ..ListArgs::default()
        })
        .await
        .map_err(|e| {
            internal_error_response(ResponseVerb::AssembleHot, &format!("store list: {e}"))
        })?;
    Ok(page
        .records
        .into_iter()
        .filter(|record| record_visible_to_authorization(record, auth, session_id))
        .collect())
}

fn principal_scoped_query(mut scope: ScopeTuple, issuer: &Identity) -> Option<ScopeTuple> {
    match issuer.kind() {
        cairn_core::domain::IdentityKind::Human => {
            scope.user = Some(issuer.as_str().to_owned());
            Some(scope)
        }
        cairn_core::domain::IdentityKind::Agent => {
            scope.agent = Some(issuer.as_str().to_owned());
            Some(scope)
        }
        cairn_core::domain::IdentityKind::Sensor => None,
    }
}

fn effective_read_visibility(max: MemoryVisibility) -> MemoryVisibility {
    if max < MemoryVisibility::Project {
        max
    } else {
        MemoryVisibility::Project
    }
}

fn record_visible_to_authorization(
    record: &MemoryRecord,
    auth: &ReadAuthorization,
    session_id: Option<&str>,
) -> bool {
    match record.visibility {
        MemoryVisibility::Private => record_principal_matches_issuer(record, &auth.issuer),
        MemoryVisibility::Session => {
            record_principal_matches_issuer(record, &auth.issuer)
                && session_id.is_some()
                && record.scope.session_id.as_deref() == session_id
        }
        MemoryVisibility::Project => true,
        _ => false,
    }
}

fn record_principal_matches_issuer(record: &MemoryRecord, issuer: &Identity) -> bool {
    match issuer.kind() {
        cairn_core::domain::IdentityKind::Human => {
            record.scope.user.as_deref() == Some(issuer.as_str())
        }
        cairn_core::domain::IdentityKind::Agent => {
            record.scope.agent.as_deref() == Some(issuer.as_str())
        }
        cairn_core::domain::IdentityKind::Sensor => {
            record.provenance.source_sensor.as_str() == issuer.as_str()
        }
    }
}

fn is_pinned_record(record: &MemoryRecord) -> bool {
    record.tags.iter().any(|tag| tag == "pinned")
        || record
            .extra_frontmatter
            .get("pinned")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

fn render_records_section(title: &str, records: &[MemoryRecord], budget: u64) -> String {
    if records.is_empty() || budget == 0 {
        return String::new();
    }
    let mut out = String::new();
    push_capped(&mut out, &format!("# {title}\n"), budget);
    for record in records {
        if out.len() as u64 >= budget {
            break;
        }
        push_capped(&mut out, "- ", budget);
        let mut first = true;
        for word in record.body.split_whitespace() {
            if out.len() as u64 >= budget {
                break;
            }
            if !first {
                push_capped(&mut out, " ", budget);
            }
            push_capped(&mut out, word, budget);
            first = false;
        }
        push_capped(&mut out, "\n", budget);
    }
    out
}

fn push_capped(out: &mut String, text: &str, budget: u64) {
    let remaining = budget.saturating_sub(out.len() as u64);
    if remaining == 0 {
        return;
    }
    if text.len() as u64 <= remaining {
        out.push_str(text);
        return;
    }
    let mut end = usize::try_from(remaining).unwrap_or(0).min(text.len());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    out.push_str(&text[..end]);
}

async fn signed_read_authorization(
    ctx: &super::signed::OpenedVerbContext,
    args: &AssembleHotArgs,
    operation_id: Ulid,
) -> Result<ReadAuthorization, Response> {
    let issuer_wire =
        std::env::var("CAIRN_ISSUER").unwrap_or_else(|_| DEFAULT_ASSEMBLE_ISSUER.to_owned());
    let issuer = Identity::parse(issuer_wire)
        .map_err(|e| super::signed::rejected_from_domain(ResponseVerb::AssembleHot, e))?;
    let active = ctx
        .identity
        .registry
        .get_identity(&issuer, IdentityVisibility::Operational)
        .await
        .map_err(|e| {
            super::signed::aborted(ResponseVerb::AssembleHot, format!("identity lookup: {e}"))
        })?
        .ok_or_else(|| {
            super::signed::rejected_from_domain(
                ResponseVerb::AssembleHot,
                cairn_core::domain::DomainError::Unauthorized {
                    message: format!("issuer {issuer} is not active in this vault"),
                },
            )
        })?;
    let handle = SecretHandle::for_identity(
        ctx.identity.vault_id.clone(),
        issuer.clone(),
        active.current_key_version,
    );
    let signing_key = ctx
        .identity
        .keystore
        .load_signing_key(&handle)
        .await
        .map_err(|e| {
            super::signed::aborted(ResponseVerb::AssembleHot, format!("issuer key load: {e}"))
        })?;
    let target_hash = assemble_args_hash(args)?;
    let mut intent = unsigned_assemble_hot_intent(
        &issuer,
        active.current_key_version.as_u32(),
        operation_id.clone(),
        ctx.config.vault.name.clone(),
        target_hash.clone(),
    );
    let intent_bytes = canonical_bytes_signed_intent(&intent)
        .map_err(|e| super::signed::rejected_from_domain(ResponseVerb::AssembleHot, e))?;
    intent.signature = Ed25519Signature(format!(
        "ed25519:{}",
        hex_lower(&signing_key.sign(&intent_bytes).to_bytes())
    ));
    let request = super::signed::request(
        RequestVerb::AssembleHot,
        RequestArgs::AssembleHot(args.clone()),
        intent,
    );
    let verified = super::signed::verify_request(ctx, request).await?;
    if verified.as_inner().target_hash != target_hash {
        return Err(super::signed::rejected_from_domain(
            ResponseVerb::AssembleHot,
            cairn_core::domain::DomainError::Unauthorized {
                message: "assemble_hot args hash mismatch".to_owned(),
            },
        ));
    }
    Ok(ReadAuthorization {
        operation_id,
        scope: ScopeTuple {
            tenant: Some(verified.as_inner().scope.tenant.clone()),
            workspace: Some(verified.as_inner().scope.workspace.clone()),
            entity: Some(verified.as_inner().scope.entity.clone()),
            ..ScopeTuple::default()
        },
        max_visibility: intent_tier_to_visibility(verified.as_inner().scope.tier),
        issuer,
    })
}

fn unsigned_assemble_hot_intent(
    issuer: &Identity,
    key_version: u32,
    operation_id: Ulid,
    workspace: String,
    target_hash: String,
) -> SignedIntent {
    let issued_at = chrono::Utc::now();
    let expires_at = issued_at + chrono::Duration::minutes(5);
    SignedIntent {
        chain_parents: vec![],
        expires_at: expires_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        issued_at: issued_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        issuer: cairn_core::generated::common::Identity(issuer.as_str().to_owned()),
        key_version: i64::from(key_version),
        nonce: super::envelope::new_nonce(),
        operation_id,
        scope: SignedIntentScope {
            tenant: DEFAULT_TENANT.to_owned(),
            workspace,
            entity: ASSEMBLE_ENTITY.to_owned(),
            tier: SignedIntentScopeTier::Project,
        },
        sequence: Some(read_sequence()),
        server_challenge: None,
        signature: Ed25519Signature(format!("ed25519:{}", "0".repeat(128))),
        target_hash,
    }
}

fn assemble_args_hash(args: &AssembleHotArgs) -> Result<String, Response> {
    serde_json::to_vec(args)
        .map(|bytes| sha256_wire(&bytes))
        .map_err(|e| super::signed::aborted(ResponseVerb::AssembleHot, format!("args hash: {e}")))
}

fn intent_tier_to_visibility(tier: SignedIntentScopeTier) -> MemoryVisibility {
    match tier {
        SignedIntentScopeTier::Session => MemoryVisibility::Session,
        SignedIntentScopeTier::Project => MemoryVisibility::Project,
        SignedIntentScopeTier::Team => MemoryVisibility::Team,
        SignedIntentScopeTier::Org => MemoryVisibility::Org,
        SignedIntentScopeTier::Public => MemoryVisibility::Public,
        _ => MemoryVisibility::Private,
    }
}

fn read_policy_trace(
    auth: &ReadAuthorization,
    loaded_files: usize,
    records: &[LoadedRecordTrace],
) -> Vec<ResponsePolicyTrace> {
    let consent_detail = if records.is_empty() {
        "no_records".to_owned()
    } else if records
        .iter()
        .any(|record| matches!(record.consent_model, Some(ConsentModel::ReceiptTimeline)))
    {
        "receipt_timeline".to_owned()
    } else {
        "legacy_event".to_owned()
    };
    vec![
        ResponsePolicyTrace {
            detail: Some(format!(
                "signed_scope_verified,files={loaded_files},records={}",
                records.len()
            )),
            gate: "read.scope".to_owned(),
            result: ResponsePolicyTraceResult::Pass,
        },
        ResponsePolicyTrace {
            detail: Some(format!(
                "tier<={}",
                effective_read_visibility(auth.max_visibility).as_str()
            )),
            gate: "read.visibility".to_owned(),
            result: ResponsePolicyTraceResult::Pass,
        },
        ResponsePolicyTrace {
            detail: Some(consent_detail),
            gate: "read.consent".to_owned(),
            result: ResponsePolicyTraceResult::Pass,
        },
    ]
}

fn merge_policy_trace(mut prefix: Vec<ResponsePolicyTrace>, mut resp: Response) -> Response {
    prefix.append(&mut resp.policy_trace);
    resp.policy_trace = prefix;
    resp
}

fn assemble_args_from_matches(sub: &ArgMatches) -> Result<AssembleHotArgs, Response> {
    let mut value = serde_json::json!({});
    if let Some(budget) = sub.get_one::<u32>("budget")
        && u64::from(*budget) > cairn_core::verbs::assemble_hot::segments::MAX_BYTES
    {
        return Err(invalid_args_response(
            ResponseVerb::AssembleHot,
            "budget",
            "budget exceeds maximum 4194304 bytes",
        ));
    }
    if let Some(session_id) = sub.get_one::<String>("session_id")
        && session_id.is_empty()
    {
        return Err(invalid_args_response(
            ResponseVerb::AssembleHot,
            "session_id",
            "session_id must not be empty",
        ));
    }
    set_optional(
        &mut value,
        "budget",
        sub.get_one::<u32>("budget").map(|n| u64::from(*n)),
    );
    set_optional(
        &mut value,
        "session_id",
        sub.get_one::<String>("session_id").cloned(),
    );
    if sub.get_flag("explain")
        && let Some(obj) = value.as_object_mut()
    {
        obj.insert("explain".to_owned(), serde_json::Value::Bool(true));
    }
    serde_json::from_value(value)
        .map_err(|e| invalid_args_response(ResponseVerb::AssembleHot, "args", &e.to_string()))
}

fn set_optional<T: serde::Serialize>(value: &mut serde_json::Value, key: &str, item: Option<T>) {
    if let Some(item) = item
        && let Some(obj) = value.as_object_mut()
    {
        obj.insert(
            key.to_owned(),
            serde_json::to_value(item)
                .expect("invariant: assemble_hot CLI args are JSON-serializable"),
        );
    }
}

fn response_exit_code(resp: &Response) -> ExitCode {
    match resp.status {
        ResponseStatus::Committed => ExitCode::SUCCESS,
        ResponseStatus::Rejected => ExitCode::from(64),
        ResponseStatus::Aborted => ExitCode::from(78),
        _ => ExitCode::FAILURE,
    }
}

fn emit_human(resp: &Response) {
    if let (ResponseStatus::Committed, Some(ResponseData::AssembleHot(data))) =
        (&resp.status, resp.data.as_ref())
    {
        let segments = data.segments.as_ref().map_or(0, Vec::len);
        println!(
            "assemble_hot: {} bytes, {} segment(s) (operation_id: {})",
            data.bytes, segments, resp.operation_id.0
        );
    } else {
        let code = resp
            .error
            .as_ref()
            .and_then(|e| e.get("code"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Internal");
        let message = resp
            .error
            .as_ref()
            .and_then(|e| e.get("message"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("assemble_hot failed");
        human_error("assemble_hot", code, message, &resp.operation_id);
    }
}

fn sha256_wire(payload: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(payload);
    format!("sha256:{}", hex_lower(&hasher.finalize()))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn read_sequence() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

fn current_unix_ms_i64() -> i64 {
    i64::try_from(read_sequence()).unwrap_or(i64::MAX)
}

async fn record_access(
    store: &cairn_store_sqlite::SqliteMemoryStore,
    records: &[LoadedRecordTrace],
    reason: &str,
) {
    if records.is_empty() {
        return;
    }
    let record_ids = records
        .iter()
        .map(|record| record.record_id.clone())
        .collect::<Vec<_>>();
    if let Err(e) = store
        .record_access(&record_ids, current_unix_ms_i64(), reason)
        .await
    {
        tracing::warn!(error = %e, reason, "record access tracking failed");
    }
}

/// Sync, filesystem-only body loader used by the lint walker. Returns
/// the body for filesystem-backed steps (`Purpose`, `Index`) and an
/// empty string for store-backed steps. The lint over-budget check
/// thus computes a strict lower bound — false-negatives are possible
/// when store-backed steps push the prefix over budget, but
/// false-positives are not. This is a significant improvement over the
/// #259 canary, which could not weigh any step at all.
///
/// # Errors
///
/// Returns `Err(String)` when a filesystem-backed source exists but
/// cannot be read (I/O error, symlink traversal, path-escape). A file
/// that is absent (not-found / no-such-file) is treated as an empty
/// body so the budget computation degrades gracefully — the
/// `BrokenSourceLink` check separately emits an Error for missing files.
pub(crate) fn lint_step_body_sync(
    vault_root: &std::path::Path,
    config: &cairn_core::config::CairnConfig,
    step: cairn_core::generated::verbs::assemble_hot::HotRecipeStep,
) -> Result<String, String> {
    use cairn_core::generated::verbs::assemble_hot::HotRecipeStep;
    // `config` is currently unused — reserved for future per-step
    // policy gating. Keep the parameter stable for lint dispatch.
    let _ = config;
    // Per-file safety cap: read up to the assembler's absolute hard cap
    // (segments::MAX_BYTES = 4 MiB), NOT the configured budget. Reading
    // only `max_bytes` would mask over-budget content from the walker —
    // a 200-byte purpose.md against an 8-byte budget would be silently
    // truncated to 8 bytes, and `assemble_hot_with_loader` would never
    // see the overflow it is supposed to detect.
    let safety_cap = cairn_core::verbs::assemble_hot::segments::MAX_BYTES;
    let read_file = |rel: &std::path::Path| -> Result<String, String> {
        cairn_core::verbs::assemble_hot::loader::read_vault_markdown_file(
            vault_root, rel, safety_cap,
        )
        .map_err(|e| e.to_string())
        .or_else(|e| {
            // Treat absent files as empty — BrokenSourceLink owns the
            // "missing source" finding; the budget check should not
            // double-fault with a load error for the same condition.
            if e.contains("No such file") || e.contains("not found") || e.contains("os error 2") {
                Ok(String::new())
            } else {
                Err(e)
            }
        })
    };
    match step {
        HotRecipeStep::Purpose => read_file(std::path::Path::new("purpose.md")),
        HotRecipeStep::Index => read_file(std::path::Path::new("index.md")),
        // Store-backed steps cannot be loaded synchronously from the lint
        // dispatch context. Return empty so the budget computation remains a
        // strict lower bound (no false-positives).
        _ => Ok(String::new()),
    }
}
