//! Settings IA acceptance: injected tool/account states, no external writes.
use super::*;
use crate::companion::chat::{cli, session, state::Provider};

#[test]
#[ignore = "writes the settings IA acceptance matrix offscreen"]
fn settings_ia_matrix() {
    let _lang = crate::i18n::test_lock("ko");
    let _account = session::test_snapshot(session::Snapshot::default());
    let root = out_root().parent().unwrap().join("settings-ia");
    let mut out = Vec::new();
    let mut h = harness(false);
    let mut index = String::from("# Settings IA snapshots\n\nko; dark-glass/light-flat; 1x/1.5x. Offscreen, injected states.\n\n");
    for (name, tab, connected, enabled, missing, tip) in [
        ("companion", model::Tab::Companion, false, false, false, false),
        ("ai-disconnected", model::Tab::CompanionAi, false, false, false, false),
        ("ai-connected", model::Tab::CompanionAi, true, false, false, false),
        ("agents-off", model::Tab::Agents, false, false, false, false),
        ("agents-claude-on", model::Tab::Agents, false, true, false, false),
        ("agents-agy-missing", model::Tab::Agents, false, false, true, false),
        ("agents-tooltip", model::Tab::Agents, false, false, false, true),
    ] {
        let _tools = cli::test_detections(cli::Status::Ready, if missing { cli::Status::Missing } else { cli::Status::Ready });
        h.app.cfg.chat = Default::default();
        h.app.cfg.set_agent_notifications(0, enabled);
        h.app.cfg.set_agent_notifications(1, false);
        h.app.cfg.set_agent_notifications(2, false);
        if connected { h.app.cfg.chat.activate(Provider::ClaudeCode); }
        h.app.section = h.app.sections.iter().position(|s| s.tab == tab).unwrap();
        h.app.scroll = 0.;
        for (light, finish) in [(false, "glass"), (true, "flat")] {
            set_look_in(&mut h, light, finish);
            for scale in [1., 1.5] {
                let dir = root.join(format!("ko-{}-{finish}-{scale}x", if light { "light" } else { "dark" }));
                let height = full_height(&mut h);
                if tip {
                    let id = tooltip::card_tip("settings.intro.agents");
                    h.app.hover = Some(Hit::Info(id));
                    h.app.tip = Some(tooltip::Tip {
                        ri: id,
                        from: std::time::Instant::now() - std::time::Duration::from_secs(5),
                        hide: None,
                    });
                }
                shoot(&mut h, &dir, name, scale, height, &mut out);
                let path = out.last().unwrap().strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
                index += &format!("- [{path}]({path})\n");
                h.app.hover = None;
                h.app.tip = None;
            }
        }
    }
    assert_eq!(out.len(), 28);
    std::fs::write(root.join("INDEX.md"), index).unwrap();
    println!("{} images: {}", out.len(), root.display());
}
