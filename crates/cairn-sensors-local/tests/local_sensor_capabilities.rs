#![allow(missing_docs)]

use cairn_core::contract::sensor_ingress::SensorIngress;
use cairn_sensors_local::{
    CaptureBudget, DropReason, EmitOutcome, LocalSensorConfig, LocalSensorIngress, SensorKind,
    SensorSettings,
};

#[test]
fn local_sensor_ingress_advertises_batch_consent_capabilities() {
    let ingress = LocalSensorIngress;
    let caps = ingress.capabilities();

    assert!(caps.batches);
    assert!(!caps.streaming);
    assert!(caps.consent_aware);
}

#[test]
fn local_sensor_config_can_disable_every_source() {
    let config = LocalSensorConfig::all_disabled();

    assert!(!config.hooks.enabled);
    assert!(!config.ide.enabled);
    assert!(!config.terminal.enabled);
    assert!(!config.clipboard.enabled);
}

#[test]
fn emit_outcome_exposes_drop_reason_without_event() {
    let outcome = EmitOutcome::Dropped {
        sensor: SensorKind::Clipboard,
        reason: DropReason::Disabled,
    };

    assert!(outcome.event().is_none());
    assert_eq!(outcome.sensor(), Some(SensorKind::Clipboard));
    assert_eq!(outcome.drop_reason(), Some(&DropReason::Disabled));
}

#[test]
fn sensor_settings_default_budget_is_unbounded() {
    let settings = SensorSettings::enabled();

    assert!(settings.budget.allows(1, 1024));
    assert_eq!(settings.budget, CaptureBudget::default());
}
