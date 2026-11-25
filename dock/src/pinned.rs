//! Pinned items. For now we import the user's existing taskbar pins so the
//! dock is useful on first launch.

use std::path::PathBuf;

pub struct Pinned {
    pub name: String,
    pub path: String,
}

fn taskbar_pins_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(
        PathBuf::from(appdata)
            .join("Microsoft\\Internet Explorer\\Quick Launch\\User Pinned\\TaskBar"),
    )
}

pub fn load() -> Vec<Pinned> {
    let mut items = Vec::new();
    if let Some(dir) = taskbar_pins_dir() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                let is_lnk = p
                    .extension()
                    .map(|x| x.eq_ignore_ascii_case("lnk"))
                    .unwrap_or(false);
                if !is_lnk {
                    continue;
                }
                let path = p.to_string_lossy().into_owned();
                // The name the taskbar shows ("파일 탐색기"), not the file's
                // ("File Explorer.lnk").
                let name = crate::apps::localized_file_name(&path)
                    .or_else(|| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
                    .unwrap_or_default();
                items.push(Pinned { name, path });
            }
        }
    }
    if items.is_empty() {
        items.push(Pinned {
            name: crate::apps::explorer_display_name().unwrap_or_else(|| "File Explorer".into()),
            path: "C:\\Windows\\explorer.exe".into(),
        });
    }
    items
}
