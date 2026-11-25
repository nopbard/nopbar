//! Deterministic UI state injection: no vault, CLI, installer or network calls.
use super::*;
use crate::companion::chat::{cli, install, session, state::Provider};

#[test]
#[ignore = "writes the companion-polish acceptance matrix offscreen"]
fn companion_polish_settings() {
    let _lang = crate::i18n::test_lock("ko");
    let root = out_root().parent().unwrap().join("companion-polish");
    let mut out = Vec::new();
    let mut h = harness(false);
    let mut index = String::from("# Companion polish visual acceptance\n\nSettings: ko; dark-glass / light-flat; 1x / 1.5x. Offline injected states, synthetic account.\n\n");
    for (name, claude, agy, active, account, mode, companion) in [
        (
            "checking",
            cli::Status::Checking,
            cli::Status::Checking,
            None,
            false,
            None,
            false,
        ),
        (
            "disconnected",
            cli::Status::Ready,
            cli::Status::Missing,
            None,
            false,
            None,
            false,
        ),
        (
            "claude-active",
            cli::Status::Ready,
            cli::Status::Missing,
            Some(Provider::ClaudeCode),
            false,
            None,
            false,
        ),
        (
            "chatgpt-active",
            cli::Status::Ready,
            cli::Status::Missing,
            Some(Provider::ChatGpt),
            true,
            None,
            false,
        ),
        (
            "agy-active",
            cli::Status::Ready,
            cli::Status::Ready,
            Some(Provider::Antigravity),
            false,
            None,
            false,
        ),
        (
            "agy-update",
            cli::Status::Ready,
            cli::Status::Unsupported,
            None,
            false,
            None,
            false,
        ),
        (
            "claude-login",
            cli::Status::Login,
            cli::Status::Missing,
            None,
            false,
            None,
            false,
        ),
        (
            "install-dialog",
            cli::Status::Ready,
            cli::Status::Missing,
            None,
            false,
            Some(install::Mode::Install),
            false,
        ),
        (
            "update-dialog",
            cli::Status::Ready,
            cli::Status::Unsupported,
            None,
            false,
            Some(install::Mode::Update),
            false,
        ),
        (
            "login-dialog",
            cli::Status::Login,
            cli::Status::Missing,
            None,
            false,
            Some(install::Mode::Login),
            false,
        ),
        (
            "companion",
            cli::Status::Ready,
            cli::Status::Missing,
            None,
            false,
            None,
            true,
        ),
        (
            "companion-connected",
            cli::Status::Ready,
            cli::Status::Missing,
            Some(Provider::ClaudeCode),
            false,
            None,
            true,
        ),
        (
            "active-needs-update",
            cli::Status::Ready,
            cli::Status::Unsupported,
            Some(Provider::Antigravity),
            false,
            None,
            false,
        ),
        (
            "agents",
            cli::Status::Ready,
            cli::Status::Ready,
            None,
            false,
            None,
            false,
        ),
        (
            "failed",
            cli::Status::Failed,
            cli::Status::Unknown,
            None,
            false,
            None,
            false,
        ),
    ] {
        let _detection = cli::test_detections(claude, agy);
        let _account = session::test_snapshot(if account {
            session::Snapshot {
                key: Some("snapshot-only".into()),
                label: "노움 친구 · example@test.invalid".into(),
                accounts: vec!["노움 친구 · example@test.invalid".into()],
            }
        } else {
            session::Snapshot::default()
        });
        h.app.cfg.chat = Default::default();
        if let Some(p) = active {
            h.app.cfg.chat.activate(p);
        }
        h.app.section = h
            .app
            .sections
            .iter()
            .position(|s| {
                s.tab
                    == if name == "agents" {
                        model::Tab::Agents
                    } else if companion {
                        model::Tab::Companion
                    } else {
                        model::Tab::CompanionAi
                    }
            })
            .unwrap();
        h.app.scroll = 0.;
        for (light, finish) in [(false, "glass"), (true, "flat")] {
            set_look_in(&mut h, light, finish);
            for scale in [1., 1.5] {
                let _dialog = install::test_snapshot(None);
                let ht = full_height(&mut h);
                let _modal = install::test_snapshot(mode.map(|mode| install::Snapshot {
                    provider: if mode == install::Mode::Login {
                        Provider::ClaudeCode
                    } else {
                        Provider::Antigravity
                    },
                    mode,
                    stage: install::Stage::Confirm,
                    details: false,
                    scroll: 0.,
                    output: String::new(),
                    generation: 0,
                }));
                let dir = root.join(format!(
                    "ko-{}-{finish}-{scale}x",
                    if light { "light" } else { "dark" }
                ));
                // Modal at actual default window height: no oversized capture hides clipping.
                shoot(
                    &mut h,
                    &dir,
                    name,
                    scale,
                    if mode.is_some() { super::super::H0 } else { ht },
                    &mut out,
                );
                let path = out
                    .last()
                    .unwrap()
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                index += &format!("- [{path}]({path}) — {name}\n");
                if let Some(mode) = mode {
                    for (suffix, stage, details) in [
                        ("commands", install::Stage::Confirm, true),
                        (
                            "running",
                            if mode == install::Mode::Login {
                                install::Stage::Login
                            } else {
                                install::Stage::Installing
                            },
                            false,
                        ),
                        ("verify", install::Stage::Verify, false),
                        ("complete", install::Stage::Done, false),
                    ] {
                        let _state = install::test_snapshot(Some(install::Snapshot {
                            provider: if mode == install::Mode::Login {
                                Provider::ClaudeCode
                            } else {
                                Provider::Antigravity
                            },
                            mode,
                            stage,
                            details,
                            scroll: 0.,
                            output: String::new(),
                            generation: 0,
                        }));
                        let state_name = format!("{name}-{suffix}");
                        shoot(&mut h, &dir, &state_name, scale, super::super::H0, &mut out);
                        let path = out
                            .last()
                            .unwrap()
                            .strip_prefix(&root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/");
                        index += &format!("- [{path}]({path}) ? {state_name}\n");
                    }
                }
                if name == "claude-active" {
                    h.app.keyboard = true;
                    h.app.focus = Some(Hit::Info(super::super::tooltip::card_tip(
                        "chat.ai.privacy",
                    )));
                    shoot(
                        &mut h,
                        &dir,
                        "claude-active-title-focus",
                        scale,
                        ht,
                        &mut out,
                    );
                    let path = out
                        .last()
                        .unwrap()
                        .strip_prefix(&root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    index += &format!("- [{path}]({path}) ? title keyboard focus\n");
                    h.app.keyboard = false;
                    h.app.focus = None;
                }
                if matches!(name, "claude-active" | "agy-active") {
                    let key = if name == "agy-active" {
                        "chat.ai.privacy.agy"
                    } else {
                        "chat.ai.privacy"
                    };
                    let id = super::super::tooltip::card_tip(key);
                    h.app.hover = Some(Hit::Info(id));
                    h.app.tip = Some(super::super::tooltip::Tip {
                        ri: id,
                        from: std::time::Instant::now() - std::time::Duration::from_secs(5),
                        hide: None,
                    });
                    let tooltip_name = format!("{name}-privacy");
                    shoot(&mut h, &dir, &tooltip_name, scale, ht, &mut out);
                    let path = out
                        .last()
                        .unwrap()
                        .strip_prefix(&root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/");
                    index += &format!("- [{path}]({path}) — privacy tooltip\n");
                    h.app.tip = None;
                    h.app.hover = None;
                }
            }
        }
    }
    index += "\nSprites and per-frame color atlas: see [SPRITES.md](SPRITES.md).\n";
    std::fs::write(root.join("INDEX.md"), index).unwrap();
    assert_eq!(out.len(), 120);
    println!("{} settings images: {}", out.len(), root.display());
}
