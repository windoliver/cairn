//! `cairn status` handler — capability discovery (§8.0.a).
//!
//! Returns the contract version, advertised capabilities, and server info.
//! For P0 (no daemon), a fresh incarnation ULID is minted per invocation.
//! When the store adapter lands, read the incarnation from the daemon table.
//! P0 status includes compiled local sensor capabilities; store capabilities
//! remain unwired.

use std::process::ExitCode;

use cairn_core::config::{CairnConfig, ScreenBackend};
use cairn_core::generated::common::Capabilities;
use cairn_core::generated::status::{
    StatusResponse, StatusResponseSensors, StatusResponseSensorsScreen,
    StatusResponseSensorsScreenBackend, StatusResponseSensorsScreenDegradation,
    StatusResponseSensorsScreenDegradationCode, StatusResponseSensorsScreenMode,
    StatusResponseSensorsScreenOcrEngine, StatusResponseSensorsScreenPermission,
    StatusResponseSensorsScreenState, StatusResponseServerInfo,
};
use cairn_sensors_local::screen::{
    self, ResolvedScreenOcrEngine, ScreenDegradationCode, ScreenMode, ScreenPermission,
    ScreenProbe, ScreenState,
};

use crate::config::CliOverrides;

use super::envelope::{emit_json, new_operation_id};

/// Run `cairn status`. Exits 0 on success.
#[must_use]
pub fn run(json: bool) -> ExitCode {
    let incarnation = new_operation_id();
    let started_at = chrono_like_now();
    let config = load_status_config();
    let resp = build_response(&config, started_at.clone(), incarnation.clone());

    if json {
        emit_json(&resp);
    } else {
        println!("contract:    {}", resp.contract);
        println!("version:     {}", resp.server_info.version);
        println!("build:       {}", resp.server_info.build);
        println!("started_at:  {started_at}");
        println!("incarnation: {}", incarnation.0);
        if resp.capabilities.is_empty() {
            println!("capabilities: (none — store not wired in this P0 build)");
        } else {
            for cap in &resp.capabilities {
                println!(
                    "  capability: {}",
                    serde_json::to_string(cap).unwrap_or_default()
                );
            }
        }
        println!(
            "screen:      {} {}",
            status_screen_backend_label(resp.sensors.screen.backend),
            status_screen_state_label(resp.sensors.screen.state)
        );
    }
    ExitCode::SUCCESS
}

fn load_status_config() -> CairnConfig {
    let overrides = CliOverrides::default();
    let Ok(current_dir) = std::env::current_dir() else {
        return CairnConfig::default();
    };

    match crate::config::load(&current_dir, &overrides) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("warning: failed to load config for status; using defaults: {err}");
            CairnConfig::default()
        }
    }
}

fn build_response(
    config: &CairnConfig,
    started_at: String,
    incarnation: cairn_core::generated::common::Ulid,
) -> StatusResponse {
    let mut capabilities = p0_capabilities();
    capabilities.extend(screen::compiled_capabilities());
    capabilities.sort_by_key(|capability| serde_json::to_string(capability).unwrap_or_default());
    capabilities.dedup();

    StatusResponse {
        contract: "cairn.mcp.v1".to_owned(),
        server_info: StatusResponseServerInfo {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            build: build_profile(),
            started_at,
            incarnation,
        },
        capabilities,
        extensions: vec![],
        sensors: map_screen_probe(&screen::probe_config(&config.sensors.screen)),
    }
}

/// P0 advertises no capabilities — the store adapter is not wired yet.
/// Update this list when store adapters land (issue #9).
fn p0_capabilities() -> Vec<Capabilities> {
    vec![]
}

fn map_screen_probe(probe: &ScreenProbe) -> StatusResponseSensors {
    StatusResponseSensors {
        screen: StatusResponseSensorsScreen {
            backend: map_screen_backend(probe.backend),
            degradation: probe.degradation.as_ref().map(|degradation| {
                StatusResponseSensorsScreenDegradation {
                    code: map_screen_degradation_code(degradation.code),
                    message: degradation.message.clone(),
                }
            }),
            mode: map_screen_mode(probe.mode),
            ocr_engine: map_screen_ocr_engine(probe.ocr_engine),
            permission: map_screen_permission(probe.permission),
            state: map_screen_state(probe.state),
        },
    }
}

fn map_screen_backend(backend: ScreenBackend) -> StatusResponseSensorsScreenBackend {
    match backend {
        ScreenBackend::Xcap => StatusResponseSensorsScreenBackend::Xcap,
        ScreenBackend::Screenpipe => StatusResponseSensorsScreenBackend::Screenpipe,
        // The current status schema is closed; unknown config backends are
        // reported as xcap while `probe_config` separately degrades them.
        _ => StatusResponseSensorsScreenBackend::Xcap,
    }
}

fn status_screen_backend_label(backend: StatusResponseSensorsScreenBackend) -> &'static str {
    match backend {
        StatusResponseSensorsScreenBackend::Xcap => "xcap",
        StatusResponseSensorsScreenBackend::Screenpipe => "screenpipe",
        _ => "xcap",
    }
}

fn status_screen_state_label(state: StatusResponseSensorsScreenState) -> &'static str {
    match state {
        StatusResponseSensorsScreenState::Disabled => "disabled",
        StatusResponseSensorsScreenState::Enabled => "enabled",
        StatusResponseSensorsScreenState::PermissionMissing => "permission_missing",
        StatusResponseSensorsScreenState::Degraded => "degraded",
        _ => "degraded",
    }
}

fn map_screen_state(state: ScreenState) -> StatusResponseSensorsScreenState {
    match state {
        ScreenState::Disabled => StatusResponseSensorsScreenState::Disabled,
        ScreenState::Enabled => StatusResponseSensorsScreenState::Enabled,
        ScreenState::PermissionMissing => StatusResponseSensorsScreenState::PermissionMissing,
        ScreenState::Degraded => StatusResponseSensorsScreenState::Degraded,
    }
}

fn map_screen_mode(mode: ScreenMode) -> StatusResponseSensorsScreenMode {
    match mode {
        ScreenMode::Off => StatusResponseSensorsScreenMode::Off,
        ScreenMode::Snapshot => StatusResponseSensorsScreenMode::Snapshot,
        ScreenMode::Continuous => StatusResponseSensorsScreenMode::Continuous,
    }
}

fn map_screen_ocr_engine(
    ocr_engine: ResolvedScreenOcrEngine,
) -> StatusResponseSensorsScreenOcrEngine {
    match ocr_engine {
        ResolvedScreenOcrEngine::Vision => StatusResponseSensorsScreenOcrEngine::Vision,
        ResolvedScreenOcrEngine::Winrt => StatusResponseSensorsScreenOcrEngine::Winrt,
        ResolvedScreenOcrEngine::Tesseract => StatusResponseSensorsScreenOcrEngine::Tesseract,
        ResolvedScreenOcrEngine::Off => StatusResponseSensorsScreenOcrEngine::Off,
    }
}

fn map_screen_permission(permission: ScreenPermission) -> StatusResponseSensorsScreenPermission {
    match permission {
        ScreenPermission::NotRequested => StatusResponseSensorsScreenPermission::NotRequested,
        ScreenPermission::Granted => StatusResponseSensorsScreenPermission::Granted,
        ScreenPermission::Denied => StatusResponseSensorsScreenPermission::Denied,
        ScreenPermission::Revoked => StatusResponseSensorsScreenPermission::Revoked,
    }
}

fn map_screen_degradation_code(
    code: ScreenDegradationCode,
) -> StatusResponseSensorsScreenDegradationCode {
    match code {
        ScreenDegradationCode::Disabled => {
            StatusResponseSensorsScreenDegradationCode::ScreenDisabled
        }
        ScreenDegradationCode::PermissionMissing => {
            StatusResponseSensorsScreenDegradationCode::ScreenPermissionMissing
        }
        ScreenDegradationCode::BackendUnavailable => {
            StatusResponseSensorsScreenDegradationCode::ScreenBackendUnavailable
        }
        ScreenDegradationCode::Degraded => {
            StatusResponseSensorsScreenDegradationCode::ScreenDegraded
        }
    }
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
    fn p0_capabilities_returns_empty() {
        let caps = p0_capabilities();
        assert!(caps.is_empty(), "P0 must advertise no capabilities");
    }

    #[test]
    fn map_screen_probe_reports_disabled_screen() {
        use cairn_core::generated::status::{
            StatusResponseSensorsScreenBackend, StatusResponseSensorsScreenDegradationCode,
            StatusResponseSensorsScreenMode, StatusResponseSensorsScreenOcrEngine,
            StatusResponseSensorsScreenPermission, StatusResponseSensorsScreenState,
        };

        let config = CairnConfig::default();
        let sensors = map_screen_probe(&screen::probe_config(&config.sensors.screen));
        let screen = sensors.screen;

        assert_eq!(screen.backend, StatusResponseSensorsScreenBackend::Xcap);
        assert_eq!(screen.state, StatusResponseSensorsScreenState::Disabled);
        assert_eq!(screen.mode, StatusResponseSensorsScreenMode::Off);
        assert_eq!(
            screen.ocr_engine,
            map_screen_ocr_engine(ResolvedScreenOcrEngine::from_config(
                config.sensors.screen.ocr.engine
            ))
        );
        assert_eq!(
            screen.permission,
            StatusResponseSensorsScreenPermission::NotRequested
        );

        let degradation = screen.degradation.expect("disabled screen degradation");
        assert_eq!(
            degradation.code,
            StatusResponseSensorsScreenDegradationCode::ScreenDisabled
        );
        assert_eq!(degradation.message, "screen sensor is disabled in config");
    }
}
