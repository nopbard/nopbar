use super::*;
#[test]
#[ignore = "offscreen device battery settings, injected accounts and tools"]
fn device_battery_settings() {
    let _lang = crate::i18n::test_lock("ko");
    let _account = crate::companion::chat::session::test_snapshot(Default::default());
    let _tools = crate::companion::chat::cli::test_detections(
        crate::companion::chat::cli::Status::Missing,
        crate::companion::chat::cli::Status::Missing,
    );
    let root = out_root().parent().unwrap().join("device-battery/settings");
    let mut out = Vec::new();
    let mut h = harness(false);
    h.app.section = h
        .app
        .sections
        .iter()
        .position(|s| s.tab == model::Tab::General)
        .unwrap();
    for enabled in [false, true] {
        h.app.cfg.widgets.device_battery = enabled;
        for (light, finish) in [(false, "glass"), (true, "flat")] {
            set_look_in(&mut h, light, finish);
            for scale in [1., 1.5] {
                let height = full_height(&mut h);
                shoot(
                    &mut h,
                    &root,
                    &format!(
                        "ko-{}-{finish}-{}-{}",
                        if light { "light" } else { "dark" },
                        if enabled { "on" } else { "off" },
                        (scale * 100.) as u32
                    ),
                    scale,
                    height,
                    &mut out,
                );
            }
        }
    }
    assert_eq!(out.len(), 8);
    widget_inbox_settings(&mut h);
}

// Keep the native offscreen harness alive across all widget setting states.
// This follows pages(): one harness, several configurations and themes.
fn widget_inbox_settings(h:&mut Harness){
    let root=out_root().parent().unwrap().join("widget-system/settings");let mut out=vec![];
    for (notes,inbox,state) in [(true,true,"on"),(true,false,"off"),(false,false,"service-off")]{
        h.app.cfg.widgets.notes=notes;h.app.cfg.widgets.inbox=inbox;
        for light in [false,true]{set_look_in(h,light,"glass");for scale in [1.,1.5]{let height=full_height(h);shoot(h,&root,&format!("inbox-{state}-{}-{}",if light{"light"}else{"dark"},(scale*100.) as u32),scale,height,&mut out);}}
    }
    assert_eq!(out.len(),12);
}
