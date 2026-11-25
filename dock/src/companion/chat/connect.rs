//! Edit only our named entry. Pure editors are tested on fixtures; production
//! paths are touched only following the corresponding settings switch.
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
const KEY: &str = "nopbar-companion";
fn ours(command: Option<&str>, args: Option<Vec<&str>>) -> bool {
    let Some(name) = command.and_then(|c| c.rsplit(['/', '\\']).next()) else {
        return false;
    };
    (name.eq_ignore_ascii_case("nopbar-mcp.exe") && args.as_ref().is_none_or(|a| a.is_empty()))
        || (name.eq_ignore_ascii_case("nopbar.exe") && args == Some(vec!["--mcp"]))
}
fn arguments(exe: &str) -> Vec<&str> {
    if exe
        .rsplit(['/', '\\'])
        .next()
        .is_some_and(|n| n.eq_ignore_ascii_case("nopbar.exe"))
    {
        vec!["--mcp"]
    } else {
        vec![]
    }
}
pub fn claude(text: &str, exe: &str, on: bool) -> Result<String, String> {
    let source = if text.trim().is_empty() {
        "{}"
    } else {
        text.trim_start_matches('\u{feff}')
    };
    let v: Value = serde_json::from_str(source).map_err(|_| crate::i18n::t("chat.error.claude_read"))?;
    if !v.is_object() {
        return Err(crate::i18n::t("chat.error.claude_format").into());
    }
    if let Some(old) = v.get("mcpServers").and_then(|m| m.get(KEY)) {
        if !ours(
            old["command"].as_str(),
            old["args"]
                .as_array()
                .and_then(|a| a.iter().map(Value::as_str).collect()),
        ) {
            return Err(crate::i18n::t("chat.error.mcp_conflict").into());
        }
    }
    let root: std::collections::HashMap<String, &serde_json::value::RawValue> =
        serde_json::from_str(source).map_err(|_| crate::i18n::t("chat.error.mcp_read"))?;
    let servers = root.get("mcpServers").map_or("{}", |v| v.get());
    if !on && !root.contains_key("mcpServers") {
        return Ok(text.into());
    }
    let entry = json!({"type":"stdio","command":exe,"args":arguments(exe)}).to_string();
    let servers = super::json_edit::set(servers, KEY, on.then_some(entry.as_str()))?;
    let edited = super::json_edit::set(source, "mcpServers", Some(&servers))?;
    Ok(if text.starts_with('\u{feff}') {
        format!("\u{feff}{edited}")
    } else {
        edited
    })
}
pub fn codex(text: &str, exe: &str, on: bool) -> Result<String, String> {
    use toml_edit::{value, Array, DocumentMut, Item, Table};
    let mut doc = text
        .parse::<DocumentMut>()
        .map_err(|_| crate::i18n::t("chat.error.codex_read"))?;
    if !doc.contains_key("mcp_servers") {
        if !on {
            return Ok(text.into());
        }
        doc["mcp_servers"] = Item::Table(Table::new());
    }
    let servers = doc["mcp_servers"]
        .as_table_mut()
        .ok_or(crate::i18n::t("chat.error.codex_format"))?;
    if let Some(old) = servers.get(KEY) {
        if !ours(
            old.get("command").and_then(Item::as_str),
            old.get("args").and_then(Item::as_array).and_then(|a| {
                a.iter()
                    .map(toml_edit::Value::as_str)
                    .collect::<Option<Vec<_>>>()
            }),
        ) {
            return Err(crate::i18n::t("chat.error.mcp_conflict").into());
        }
    }
    if on {
        let mut t = Table::new();
        t["command"] = value(exe);
        let mut args = Array::new();
        for arg in arguments(exe) {
            args.push(arg);
        }
        t["args"] = value(args);
        servers.insert(KEY, Item::Table(t));
    } else {
        servers.remove(KEY);
    }
    Ok(doc.to_string())
}
fn write(path: &Path, exe: &str, on: bool, is_claude: bool) -> Result<(), String> {
    let old = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !on {
                return Ok(());
            }
            String::new()
        }
        Err(_) => return Err(crate::i18n::t("chat.error.mcp_file").into()),
    };
    let new = if is_claude {
        claude(&old, exe, on)?
    } else {
        codex(&old, exe, on)?
    };
    if new == old {
        return Ok(());
    }
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|_| crate::i18n::t("chat.error.mcp_directory"))?;
    }
    // Detect concurrent CLI edits before replacing the original.
    if std::fs::read_to_string(path).unwrap_or_default() != old {
        return Err(crate::i18n::t("chat.error.cli_changed").into());
    }
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let saved = std::fs::write(&temp, new).and_then(|_| std::fs::rename(&temp, path));
    if saved.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    saved.map_err(|_| crate::i18n::t("chat.error.mcp_save").into())
}
pub fn paths() -> (Vec<PathBuf>, PathBuf) {
    let home = PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default());
    let mut claude = vec![std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .map(|p| p.join(".claude.json"))
        .unwrap_or_else(|| home.join(".claude.json"))];
    if let Some(app) = std::env::var_os("APPDATA") {
        let root = PathBuf::from(app).join("orca/claude-profiles");
        if let Ok(dirs) = std::fs::read_dir(root) {
            for d in dirs.flatten() {
                let f = d.path().join("home/.claude.json");
                if f.is_file() {
                    claude.push(f);
                }
            }
        }
    }
    claude.sort();
    claude.dedup();
    let codex = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
        .join("config.toml");
    (claude, codex)
}
pub fn antigravity_path() -> PathBuf {
    PathBuf::from(std::env::var_os("USERPROFILE").unwrap_or_default()).join(".gemini/config/mcp_config.json")
}
pub fn set_antigravity(on: bool) -> Result<(), String> {
    let _lock=super::credentials::Lock::named("mcp-config").map_err(String::from)?;
    let exe=if on {super::shim::install()?}else{super::shim::target()?};
    // Same mcpServers/command/args schema; byte-preserving owned-entry editor.
    write(&antigravity_path(), &exe.to_string_lossy(), on, true)?;
    if !on && !has_registration(){super::shim::remove()?;}
    Ok(())
}
pub fn set(is_claude: bool, on: bool) -> Result<(), String> {
    let _lock = super::credentials::Lock::named("mcp-config").map_err(String::from)?;
    let (claude, codex) = paths();
    let exe = if on {
        super::shim::install()?
    } else {
        super::shim::target()?
    };
    let exe = exe.to_string_lossy().into_owned();
    for path in if is_claude { claude } else { vec![codex] } {
        write(&path, &exe, on, is_claude)?;
    }
    if !on && !has_registration() {
        super::shim::remove()?;
    }
    Ok(())
}
fn has_registration() -> bool {
    let (mut paths, codex) = paths();
    paths.push(codex);
    paths.push(antigravity_path());
    paths.iter().any(|p| match std::fs::read_to_string(p) {
        Ok(s) => s.contains(KEY),
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    })
}
pub fn uninstall() -> Vec<String> {
    [set(true, false), set(false, false), set_antigravity(false)]
        .into_iter()
        .filter_map(Result::err)
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_preserves_other_entries_and_rejects_collisions() {
        let exe = r"C:\Apps\NopBar\nopbar.exe";
        let original = r#"{"theme":"dark","mcpServers":{"other":{"command":"tool","args":["x"]}},"projects":{"x":{"trusted":true}}}"#;
        let added = claude(original, exe, true).unwrap();
        let removed = claude(&added, exe, false).unwrap();
        assert_eq!(
            removed, original,
            "unrelated JSON bytes and key order are preserved"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&removed).unwrap(),
            serde_json::from_str::<Value>(original).unwrap()
        );
        assert!(claude(&added.replace("nopbar.exe", "different.exe"), exe, true).is_err());
        assert!(claude(&added, r"D:\Other\nopbar.exe", false).is_ok());
        let original="# keep this comment\nmodel = 'x'\n[mcp_servers.other]\ncommand = 'tool'\nargs = ['x']\n";
        let added = codex(original, exe, true).unwrap();
        assert!(added.contains("# keep this comment"));
        let removed = codex(&added, exe, false).unwrap();
        assert_eq!(removed, original);
        assert!(codex(&added, r"D:\Other\nopbar.exe", false).is_ok());
    }
    #[test]
    fn antigravity_map_preserves_unrelated_bytes_and_foreign_entries() {
        let root = std::env::temp_dir().join(format!("nopbar-agy-{}", uuid::Uuid::new_v4()));
        let path = root.join("mcp_config.json");
        std::fs::create_dir_all(&root).unwrap();
        let original = "{\n  \"mcpServers\": {\"other\": {\"command\": \"keep\", \"env\": {\"A\": \"b\"}}},\n  \"custom\": true\n}\n";
        std::fs::write(&path, original).unwrap();
        write(&path, "C:/NopBar/nopbar-mcp.exe", true, true).unwrap();
        let added = std::fs::read_to_string(&path).unwrap();
        assert!(added.contains(KEY));
        write(&path, "C:/NopBar/nopbar-mcp.exe", false, true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn temp_files_only() {
        let root = std::env::temp_dir().join(format!("nopbar-mcp-{}", uuid::Uuid::new_v4()));
        let path = root.join("config.toml");
        write(&path, "nopbar.exe", true, false).unwrap();
        write(&path, "nopbar.exe", false, false).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(KEY));
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn shim_identity_ignores_install_path_and_failed_rename_leaves_no_temp() {
        use std::os::windows::fs::OpenOptionsExt;
        let original = r#"{ "z":1, "mcpServers":{ "other":{"command":"keep"}, "nopbar-companion":{"command":"D:/old/NOPBAR-MCP.EXE","args":[]}}, "a":2 }"#;
        let removed = claude(original, "C:/new/nopbar-mcp.exe", false).unwrap();
        assert_eq!(
            removed,
            r#"{ "z":1, "mcpServers":{ "other":{"command":"keep"}}, "a":2 }"#
        );
        assert!(!ours(Some("nopbar.exe"), Some(vec![])));
        let root = std::env::temp_dir().join(format!("nopbar-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join(".claude.json");
        std::fs::write(&path, original).unwrap();
        let locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        assert!(write(&path, "C:/new/nopbar-mcp.exe", false, true).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        drop(locked);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
