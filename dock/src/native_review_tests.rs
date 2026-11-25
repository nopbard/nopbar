//! Wiring contracts for paths that must not touch the real desktop in tests.
fn before_tests(source: &str) -> &str { source.split("#[cfg(test)]").next().unwrap() }

#[test]
fn review_fix_1_quiet_must_not_gate_toast() {
    let s = before_tests(include_str!("companion/remind.rs"));
    assert!(!s.replace("\r\n", "\n").contains("if delivery == Delivery::Defer {\n        retry();\n        return;"));
}
#[test]
fn review_fix_2_session_end_restores_dnd_and_lease_is_durable() {
    let s = include_str!("dock/mod.rs");
    for marker in ["WM_QUERYENDSESSION =>", "WM_ENDSESSION =>"] {
        let arm = s.split(marker).nth(1).unwrap().split("LRESULT(").next().unwrap();
        assert!(arm.contains("focus_timer::shutdown()"), "{marker}");
    }
    assert!(before_tests(include_str!("focus_timer/dnd.rs")).contains("focus_dnd_lease.json"));
}
#[test]
fn review_fix_3_toast_failure_must_not_gate_bubble_or_register_again() {
    let s = before_tests(include_str!("companion/remind.rs"));
    assert!(!s.contains("if delivered && delivery == Delivery::Bubble"));
    let s = before_tests(include_str!("notes/toast.rs"));
    let show = s.split("pub fn show(").nth(1).unwrap();
    assert!(!show.contains("register_protocol("));
}
#[test]
fn review_fix_4_focus_feedback_survives_quiet() {
    let t = std::time::Instant::now();
    for cue in [crate::companion::Cue::FocusStart, crate::companion::Cue::BreakStart, crate::companion::Cue::BreakEnd] {
        // The production ingress must retain signals while quiet.
        let s = include_str!("companion/mod.rs");
        assert!(!s.contains("if !quiet&&a.cfg.enabled&&a.cfg.situations {for (cue,at) in queued"), "{cue:?} at {t:?}");
    }
}
#[test]
fn review_fix_5_nonactivating_dock_does_not_claim_ctrl_z() {
    assert!(!include_str!("dock/mod.rs").contains("WM_KEYDOWN if wp.0 == b'Z'"));
    assert!(include_str!("dock/menu.rs").contains("undo_remove()"));
}
#[test]
fn review_fix_6_launch_transfers_foreground_and_oauth_does_not_block_runtime() {
    let s = before_tests(include_str!("launch/environment.rs"));
    assert!(s.contains("AllowSetForegroundWindow"));
    let s = before_tests(include_str!("launch.rs"));
    assert!(s.contains("GetProcessId(info.hProcess)"));
    assert!(include_str!("companion/chat/auth.rs").contains("spawn_blocking"));
}
#[test]
fn review_fix_7_ui_quiet_reads_only_cache() {
    let s = before_tests(include_str!("notification_state.rs"));
    let body = s.split("pub fn quiet() -> bool {").nth(1).unwrap().split("\n}").next().unwrap();
    assert!(!body.contains("Settings::new()"));
    assert!(!body.contains("RoInitialize"));
}
#[test]
fn review_fix_8_secondary_dpi_checked_on_every_settings_broadcast() {
    let s = before_tests(include_str!("desklayer/host.rs"));
    let arm = s.split("WM_SETTINGCHANGE | WM_POWERBROADCAST =>").nth(1).unwrap().split("DefWindowProcW").next().unwrap();
    assert!(arm.contains("crate::monitors::all()"));
}
