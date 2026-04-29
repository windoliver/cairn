use cairn_core::policy_trace::PolicyOutcome;

#[test]
fn outcomes_serialize_to_wire_form() {
    let cases = [
        (PolicyOutcome::Pass, "pass"),
        (PolicyOutcome::Deny, "deny"),
        (PolicyOutcome::Error, "error"),
    ];
    for (oc, expected) in cases {
        assert_eq!(oc.as_str(), expected);
        assert_eq!(format!("{oc}"), expected);
    }
}
