#![allow(missing_docs)]

use cairn_core::contract::sensor_ingress::SensorIngress;
use cairn_core::domain::SourceFamily;
use cairn_sensors_local::{
    CaptureBudget, DropReason, EmitOutcome, LocalSensorConfig, LocalSensorIngress, SensorKind,
    SensorSettings,
};

#[test]
fn local_sensor_ingress_advertises_batch_streaming_consent_capabilities() {
    let ingress = LocalSensorIngress;
    let caps = ingress.capabilities();

    assert!(caps.batches);
    assert!(caps.streaming);
    assert!(caps.consent_aware);
}

#[test]
fn local_sensor_config_can_disable_every_source() {
    let config = LocalSensorConfig::all_disabled();

    assert!(!config.hooks.enabled);
    assert!(!config.ide.enabled);
    assert!(!config.terminal.enabled);
    assert!(!config.clipboard.enabled);
    assert!(!config.voice.enabled);
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

#[test]
fn sensor_kind_maps_local_source_families_only() {
    assert_eq!(
        SensorKind::from_source_family(SourceFamily::Hook),
        Some(SensorKind::Hook)
    );
    assert_eq!(
        SensorKind::from_source_family(SourceFamily::Ide),
        Some(SensorKind::Ide)
    );
    assert_eq!(
        SensorKind::from_source_family(SourceFamily::Terminal),
        Some(SensorKind::Terminal)
    );
    assert_eq!(
        SensorKind::from_source_family(SourceFamily::Clipboard),
        Some(SensorKind::Clipboard)
    );
    assert_eq!(
        SensorKind::from_source_family(SourceFamily::Voice),
        Some(SensorKind::Voice)
    );
    assert_eq!(SensorKind::from_source_family(SourceFamily::Cli), None);
}
