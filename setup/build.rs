//! Bakes four things into nopbar-setup.exe:
//! - the app version (dock/Cargo.toml) as NOPBAR_APP_VERSION and in the
//!   version resource: the updater checks the setup exe's ProductVersion
//!   against the signed manifest (dock/src/autoupdate/trust.rs);
//! - the icon and the manifest (asInvoker: never elevated; per-monitor DPI);
//! - the Inno Setup installer it carries: the file named by the
//!   NOPBAR_SETUP_INNER variable (scripts/package.ps1 sets it), copied to
//!   OUT_DIR/inner.bin. Unset (cargo test, clippy, dev builds): an empty
//!   payload, and the exe says it has nothing to install.
//! - its words in every language the app speaks: the `setup.*` keys of
//!   dock/locales/*.json, as a Rust table (OUT_DIR/setup_strings.rs; only
//!   those keys, not the whole catalogs).
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest_dir.parent().unwrap();
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let dock_toml = root.join("dock").join("Cargo.toml");
    println!("cargo:rerun-if-changed={}", dock_toml.display());
    let v = app_version(&std::fs::read_to_string(&dock_toml).unwrap()).expect("no version in dock/Cargo.toml");
    println!("cargo:rustc-env=NOPBAR_APP_VERSION={v}");

    println!("cargo:rerun-if-env-changed=NOPBAR_SETUP_INNER");
    let dest = out_dir.join("inner.bin");
    match std::env::var_os("NOPBAR_SETUP_INNER").filter(|p| !p.is_empty()) {
        Some(p) => {
            let p = PathBuf::from(p);
            println!("cargo:rerun-if-changed={}", p.display());
            std::fs::copy(&p, &dest).unwrap_or_else(|e| panic!("NOPBAR_SETUP_INNER {}: {e}", p.display()));
        }
        None => write_if_changed(&dest, b""),
    }

    locales(&root.join("dock").join("locales"), &out_dir.join("setup_strings.rs"));

    // rc.exe reads paths in the ANSI code page (a Korean user name breaks
    // an absolute path): the icon and manifest go next to the generated .rc.
    std::fs::copy(root.join("assets").join("brand").join("nopbar.ico"), out_dir.join("nopbar.ico")).unwrap();
    std::fs::copy(manifest_dir.join("res").join("setup.manifest"), out_dir.join("setup.manifest")).unwrap();
    let mut parts: Vec<u32> = v.split(['.', '-']).filter_map(|p| p.parse().ok()).collect();
    parts.resize(4, 0);
    let comma = format!("{},{},{},{}", parts[0], parts[1], parts[2], parts[3]);
    let rc = std::fs::read_to_string(manifest_dir.join("res").join("setup.rc"))
        .unwrap()
        .replace("NOPBAR_VER_COMMA", &comma)
        .replace("NOPBAR_VER_STR", &format!("\"{v}\""));
    let out = out_dir.join("setup.rc");
    std::fs::write(&out, rc).unwrap();
    println!("cargo:rerun-if-changed=res/setup.rc");
    println!("cargo:rerun-if-changed=res/setup.manifest");
    println!("cargo:rerun-if-changed=../assets/brand/nopbar.ico");
    embed_resource::compile(&out, embed_resource::NONE).manifest_optional().unwrap();
}

/// The `setup.*` keys there are, as strings::Strings fields ("setup.failed.moved"
/// -> failed_moved). Keep in step with that struct.
const FIELDS: [&str; 12] = [
    "window_title",
    "hint",
    "action",
    "action_verb",
    "installing",
    "retry",
    "failed_moved",
    "failed_restart",
    "failed_unpack",
    "failed_empty",
    "failed_start",
    "failed_other",
];

/// Writes `TAGS` (English first, then Korean, then the rest by name: the
/// app's order), `TABLES` (a strings::Strings per tag), `FELL_BACK` (the
/// (tag, key)s a catalog lacks, given English instead) and `UNKNOWN`
/// (setup.* keys in en.json no field reads). Tests keep the last two empty.
fn locales(dir: &Path, out: &Path) {
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut tags: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".json").map(String::from))
        .collect();
    tags.sort_by_key(|t| (t != "en", t != "ko", t.clone()));
    let read = |tag: &str| -> std::collections::BTreeMap<String, String> {
        let p = dir.join(format!("{tag}.json"));
        println!("cargo:rerun-if-changed={}", p.display());
        let text = std::fs::read_to_string(&p).unwrap();
        let all: std::collections::BTreeMap<String, serde_json::Value> = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        all.into_iter().filter(|(k, _)| k.starts_with("setup.")).filter_map(|(k, v)| Some((k, v.as_str()?.to_string()))).collect()
    };
    let key = |field: &str| format!("setup.{}", field.replacen('_', ".", if field.starts_with("failed_") { 1 } else { 0 }));
    let en = read("en");
    let mut fell_back = Vec::new();
    let mut tables = String::new();
    for tag in &tags {
        let m = read(tag);
        tables.push_str("    Strings {
");
        for f in FIELDS {
            let k = key(f);
            let v = match m.get(&k) {
                Some(v) => v,
                None => {
                    fell_back.push(format!("({tag:?}, {k:?})"));
                    en.get(&k).unwrap_or_else(|| panic!("en.json lacks {k}"))
                }
            };
            tables.push_str(&format!("        {f}: {v:?},
"));
        }
        tables.push_str("    },
");
    }
    let known: Vec<String> = FIELDS.iter().map(|f| key(f)).collect();
    let unknown: Vec<String> = en.keys().filter(|k| !known.contains(k)).map(|k| format!("{k:?}")).collect();
    let code = format!(
        "// Generated by build.rs from dock/locales/*.json (the setup.* keys).
         pub static TAGS: [&str; {n}] = [{tags}];
         pub static TABLES: [Strings; {n}] = [
{tables}];
         pub static FELL_BACK: &[(&str, &str)] = &[{fell}];
         pub static UNKNOWN: &[&str] = &[{unknown}];
",
        n = tags.len(),
        tags = tags.iter().map(|t| format!("{t:?}")).collect::<Vec<_>>().join(", "),
        fell = fell_back.join(", "),
        unknown = unknown.join(", "),
    );
    write_if_changed(out, code.as_bytes());
}

/// The first `version = "..."` line (dock's [package] version), as
/// scripts/package.ps1 reads it.
fn app_version(toml: &str) -> Option<String> {
    toml.lines().find_map(|l| {
        let rest = l.trim_start().strip_prefix("version")?.trim_start().strip_prefix('=')?.trim();
        Some(rest.strip_prefix('"')?.split('"').next()?.to_string())
    })
}

fn write_if_changed(path: &Path, bytes: &[u8]) {
    if std::fs::read(path).ok().as_deref() != Some(bytes) {
        std::fs::write(path, bytes).unwrap();
    }
}
