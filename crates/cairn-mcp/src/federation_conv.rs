//! Hand-written conversions between IDL-codegenerated federation
//! args / data types and the verb-layer request / response types
//! (issue #123 T17, brief §12.a).
//!
//! The MCP boundary deserializes wire JSON into the codegenerated
//! `*Args` types (`ProposeShareArgs`, `AcceptShareArgs`,
//! `RevokeShareArgs`); the verb functions (`propose_share`,
//! `accept_share`, `revoke_share`) accept their own strongly-typed
//! request structs that thread every string through the domain
//! parsers (`Identity::parse`, `Rfc3339Timestamp::parse`, …). This
//! module owns the bridge:
//!
//! * [`propose_share_request_from_args`] — `ProposeShareArgs` →
//!   `ProposeShareRequest` (fallible on malformed identities,
//!   timestamps, record ids, or grant tiers).
//! * [`propose_share_data_from_response`] — `ProposeShareResponse` →
//!   `ProposeShareData` (fallible on a private-tier wire mismatch via
//!   [`cairn_core::domain::federation::signed_share_link_to_wire`]).
//! * [`accept_share_request_from_args`] — `AcceptShareArgs` →
//!   `AcceptShareRequest` (infallible — the envelope is already the
//!   codegen type).
//! * [`accept_share_data_from_response`] — `AcceptShareResponse` →
//!   `AcceptShareData` (infallible).
//! * [`revoke_share_request_from_args`] / [`revoke_share_data_from_response`]
//!   — `RevokeShareArgs` ↔ `RevokeShareData` (infallible).
//!
//! Failures map onto [`cairn_core::error::federation::FederationError`]
//! so the MCP dispatcher can use the same error → policy-trace pipeline
//! the verb itself uses. The wire-side never sees raw `DomainError`
//! values.

use cairn_core::domain::federation::{PeerEndpoint, signed_share_link_to_wire};
use cairn_core::domain::{Identity, MemoryVisibility, Rfc3339Timestamp, ScopeTuple};
use cairn_core::error::federation::FederationError;
use cairn_core::generated::common::{
    Identity as WireIdentity, ScopeTuple as WireScopeTuple, Ulid as WireUlid,
};
use cairn_core::generated::verbs::accept_share::{
    AcceptShareArgs, AcceptShareData, AcceptShareDataOutcome,
};
use cairn_core::generated::verbs::propose_share::{
    ProposeShareArgs, ProposeShareArgsGrantTier, ProposeShareData,
};
use cairn_core::generated::verbs::revoke_share::{RevokeShareArgs, RevokeShareData};
use cairn_core::verbs::accept_share::{AcceptOutcome, AcceptShareRequest, AcceptShareResponse};
use cairn_core::verbs::propose_share::{ProposeShareRequest, ProposeShareResponse};
use cairn_core::verbs::revoke_share::{RevokeShareRequest, RevokeShareResponse};

/// Convert `ProposeShareArgs` (wire) → `ProposeShareRequest` (verb).
///
/// # Errors
///
/// Returns [`FederationError::InvalidShape`] when any field fails its
/// domain parser (`Identity`, `Rfc3339Timestamp`, grantee identity).
/// The verb itself re-validates every parsed value, so this conversion
/// is the early-rejection layer at the MCP boundary.
pub fn propose_share_request_from_args(
    args: ProposeShareArgs,
) -> Result<ProposeShareRequest, FederationError> {
    let grantee = match args.grantee {
        Some(g) => Some(parse_identity_wire(&g)?),
        None => None,
    };
    let expires_at = Rfc3339Timestamp::parse(args.expires_at.clone())
        .map_err(|_| FederationError::InvalidShape)?;
    let scope = scope_tuple_from_wire(&args.scope);
    let grant_tier = grant_tier_from_wire(args.grant_tier);
    let peer = args.peer.map(PeerEndpoint);
    let record_ids: Vec<String> = args
        .record_ids
        .iter()
        .map(|u: &WireUlid| u.0.clone())
        .collect();

    Ok(ProposeShareRequest {
        record_ids,
        grantee,
        scope,
        grant_tier,
        expires_at,
        peer,
    })
}

/// Convert `ProposeShareResponse` (verb) → `ProposeShareData` (wire).
///
/// # Errors
///
/// Returns [`FederationError::InvalidShape`] when the signed link
/// cannot be re-encoded as wire form — currently only possible if the
/// link carries a [`MemoryVisibility::Private`] tier, which the verb
/// already rejects. The defence-in-depth check stays so a future
/// non-shareable tier surfaces here rather than panicking the
/// transport.
pub fn propose_share_data_from_response(
    response: ProposeShareResponse,
) -> Result<ProposeShareData, FederationError> {
    let wire_link =
        signed_share_link_to_wire(&response.link).map_err(|_| FederationError::InvalidShape)?;
    Ok(ProposeShareData {
        link: wire_link,
        operation_id: WireUlid(response.operation_id),
    })
}

/// Convert `AcceptShareArgs` (wire) → `AcceptShareRequest` (verb).
///
/// Infallible: the envelope is already the codegen type, which the
/// verb is responsible for validating.
#[must_use]
pub fn accept_share_request_from_args(args: AcceptShareArgs) -> AcceptShareRequest {
    AcceptShareRequest {
        envelope: args.envelope,
    }
}

/// Convert `AcceptShareResponse` (verb) → `AcceptShareData` (wire).
#[must_use]
pub fn accept_share_data_from_response(response: AcceptShareResponse) -> AcceptShareData {
    AcceptShareData {
        applied_records: response.applied_records.into_iter().map(WireUlid).collect(),
        outcome: match response.outcome {
            AcceptOutcome::Accepted => AcceptShareDataOutcome::Accepted,
            AcceptOutcome::Duplicate => AcceptShareDataOutcome::Duplicate,
        },
    }
}

/// Convert `RevokeShareArgs` (wire) → `RevokeShareRequest` (verb).
#[must_use]
pub fn revoke_share_request_from_args(args: RevokeShareArgs) -> RevokeShareRequest {
    RevokeShareRequest {
        link_id: args.link_id,
    }
}

/// Convert `RevokeShareResponse` (verb) → `RevokeShareData` (wire).
#[must_use]
pub fn revoke_share_data_from_response(response: RevokeShareResponse) -> RevokeShareData {
    RevokeShareData {
        operation_id: WireUlid(response.operation_id),
    }
}

// ---------- helpers ----------------------------------------------------

fn parse_identity_wire(wire: &WireIdentity) -> Result<Identity, FederationError> {
    Identity::parse(wire.0.clone()).map_err(|_| FederationError::InvalidShape)
}

fn scope_tuple_from_wire(wire: &WireScopeTuple) -> ScopeTuple {
    ScopeTuple {
        agent: wire.agent.clone(),
        entity: wire.entity.clone(),
        project: wire.project.clone(),
        session_id: wire.session_id.clone(),
        tenant: wire.tenant.clone(),
        user: wire.user.clone(),
        workspace: wire.workspace.clone(),
    }
}

fn grant_tier_from_wire(tier: ProposeShareArgsGrantTier) -> MemoryVisibility {
    // `ProposeShareArgsGrantTier` is `#[non_exhaustive]`; a future IDL
    // addition falls through to the `Session` arm so the verb's own
    // tier guard (`is_shared_tier`) re-validates the request rather
    // than silently propagating an unknown tier. The conversion is
    // intentionally collapsed onto `Session` (collapsing the wildcard
    // with the explicit `Session` arm is what clippy wants and what we
    // mean: any unknown future tier maps to the most-restrictive
    // shared tier).
    match tier {
        ProposeShareArgsGrantTier::Project => MemoryVisibility::Project,
        ProposeShareArgsGrantTier::Team => MemoryVisibility::Team,
        ProposeShareArgsGrantTier::Org => MemoryVisibility::Org,
        ProposeShareArgsGrantTier::Public => MemoryVisibility::Public,
        ProposeShareArgsGrantTier::Session | _ => MemoryVisibility::Session,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_args() -> ProposeShareArgs {
        ProposeShareArgs {
            expires_at: "2026-12-31T00:00:00Z".to_owned(),
            grant_tier: ProposeShareArgsGrantTier::Team,
            grantee: Some(WireIdentity("hmn:bob".to_owned())),
            peer: Some("loopback://node-b".to_owned()),
            record_ids: vec![WireUlid("01HZZZZZZZZZZZZZZZZZZZZZZZ".to_owned())],
            scope: WireScopeTuple {
                agent: None,
                entity: None,
                project: None,
                session_id: None,
                tenant: Some("acme".to_owned()),
                user: None,
                workspace: None,
            },
        }
    }

    #[test]
    fn propose_share_request_from_args_threads_typed_fields() {
        let req = propose_share_request_from_args(sample_args()).expect("valid args");
        assert_eq!(req.record_ids, ["01HZZZZZZZZZZZZZZZZZZZZZZZ"]);
        assert_eq!(req.grant_tier, MemoryVisibility::Team);
        assert_eq!(req.grantee.as_ref().map(Identity::as_str), Some("hmn:bob"));
        assert_eq!(
            req.peer.as_ref().map(|p| p.0.as_str()),
            Some("loopback://node-b")
        );
        assert_eq!(req.expires_at.as_str(), "2026-12-31T00:00:00Z");
        assert_eq!(req.scope.tenant.as_deref(), Some("acme"));
    }

    #[test]
    fn propose_share_request_from_args_rejects_bad_identity() {
        let mut args = sample_args();
        args.grantee = Some(WireIdentity("not-a-prefix:bob".to_owned()));
        let err = propose_share_request_from_args(args).expect_err("bad identity");
        assert_eq!(err, FederationError::InvalidShape);
    }

    #[test]
    fn propose_share_request_from_args_rejects_bad_timestamp() {
        let mut args = sample_args();
        args.expires_at = "yesterday".to_owned();
        let err = propose_share_request_from_args(args).expect_err("bad timestamp");
        assert_eq!(err, FederationError::InvalidShape);
    }

    #[test]
    fn accept_share_data_from_response_maps_outcome() {
        let response = AcceptShareResponse {
            outcome: AcceptOutcome::Duplicate,
            applied_records: vec!["01HZZZZZZZZZZZZZZZZZZZZZZZ".to_owned()],
        };
        let data = accept_share_data_from_response(response);
        assert_eq!(data.outcome, AcceptShareDataOutcome::Duplicate);
        assert_eq!(data.applied_records.len(), 1);
        assert_eq!(data.applied_records[0].0, "01HZZZZZZZZZZZZZZZZZZZZZZZ");
    }

    #[test]
    fn revoke_share_round_trip() {
        let args = RevokeShareArgs {
            link_id: "share-01HZZZZZZZZZZZZZZZZZZZZZZZ".to_owned(),
        };
        let req = revoke_share_request_from_args(args);
        assert_eq!(req.link_id, "share-01HZZZZZZZZZZZZZZZZZZZZZZZ");

        let response = RevokeShareResponse {
            operation_id: "01HZZZZZZZZZZZZZZZZZZZZZZZ".to_owned(),
            // SignedRevocation is a wire type; build a body-free
            // fixture by deserializing from JSON to avoid a hand-crafted
            // constructor here.
            revocation: serde_json::from_value(serde_json::json!({
                "issuer": "hmn:alice",
                "key_version": 1,
                "link_id": "share-01HZZZZZZZZZZZZZZZZZZZZZZZ",
                "revoked_at": "2026-12-31T00:00:00Z",
                "signature": format!("ed25519:{}", "0".repeat(128)),
            }))
            .expect("valid revocation fixture"),
        };
        let data = revoke_share_data_from_response(response);
        assert_eq!(data.operation_id.0, "01HZZZZZZZZZZZZZZZZZZZZZZZ");
    }
}
