//! `FederationError` <-> `SharingDecisionKind` mapping (brief §12.a, §14).

use cairn_core::domain::sharing::SharingDecisionKind;
use cairn_core::error::federation::FederationError;

#[test]
fn each_sharing_overlap_maps_to_expected_kind() {
    use FederationError as E;
    assert_eq!(E::Expired.sharing_decision(),         Some(SharingDecisionKind::Expired));
    assert_eq!(E::TargetMismatch.sharing_decision(),  Some(SharingDecisionKind::TargetMismatch));
    assert_eq!(E::ScopeMismatch.sharing_decision(),   Some(SharingDecisionKind::ScopeMismatch));
    assert_eq!(E::TierMismatch.sharing_decision(),    Some(SharingDecisionKind::TierMismatch));
    assert_eq!(E::BadSignature.sharing_decision(),    Some(SharingDecisionKind::BadSignature));
    assert_eq!(E::Revoked.sharing_decision(),         Some(SharingDecisionKind::Revoked));
    assert_eq!(E::NotHuman.sharing_decision(),        Some(SharingDecisionKind::NotHuman));
    assert_eq!(E::NoRebacRelation.sharing_decision(), Some(SharingDecisionKind::NoRebacRelation));
    assert_eq!(E::InvalidShape.sharing_decision(),    Some(SharingDecisionKind::InvalidShape));
}

#[test]
fn federation_only_variants_have_no_sharing_overlap() {
    use FederationError as E;
    assert_eq!(E::DuplicateNonce.sharing_decision(),     None);
    assert_eq!(E::UnknownLink.sharing_decision(),        None);
    assert_eq!(E::CapabilityDisabled.sharing_decision(), None);
}

#[test]
fn display_messages_are_body_free() {
    use FederationError as E;
    // Body-free: no record content, no PII. Sample a few variants and
    // confirm Display contains only diagnostic strings.
    assert_eq!(E::Expired.to_string(),         "receipt or link expired");
    assert_eq!(E::BadSignature.to_string(),    "signature verification failed");
    assert_eq!(E::CapabilityDisabled.to_string(), "federation capability not advertised");
}
