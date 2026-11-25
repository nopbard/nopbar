//! Bakes the NopBar icon and version info into the exe: fills the version
//! into res/nopbar.rc and compiles it.
fn main() {
    companion_assets();
    let v = env!("CARGO_PKG_VERSION");
    let mut parts: Vec<u32> = v.split(['.', '-']).filter_map(|p| p.parse().ok()).collect();
    parts.resize(4, 0);
    let comma = format!("{},{},{},{}", parts[0], parts[1], parts[2], parts[3]);
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("res");
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // rc.exe reads paths in the ANSI code page: a non-ASCII checkout path
    // (e.g. a Korean user name) breaks an absolute icon path. Copy the icon
    // next to the generated .rc and refer to it by name.
    std::fs::copy(dir.join("nopbar.ico"), out_dir.join("nopbar.ico")).unwrap();
    let rc = std::fs::read_to_string(dir.join("nopbar.rc"))
        .unwrap()
        .replace("NOPBAR_VER_COMMA", &comma)
        .replace("NOPBAR_VER_STR", &format!("\"{v}\""));
    let out = out_dir.join("nopbar.rc");
    std::fs::write(&out, rc).unwrap();
    println!("cargo:rerun-if-changed=res/nopbar.rc");
    println!("cargo:rerun-if-changed=res/nopbar.ico");
    embed_resource::compile(&out, embed_resource::NONE).manifest_optional().unwrap();
}
// Every future frame is embedded by filename; adding a sheet's cut PNGs
// and updating the JSON catalogs does not require a Rust source edit.
fn companion_assets() {
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("../assets/companion");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "png"))
        .collect();
    files.extend(std::fs::read_dir(dir.join("fx")).unwrap().map(|e|e.unwrap().path()).filter(|p|p.extension().is_some_and(|e|e=="png")));
    files.sort();
    let mut src = String::from("pub static PNGS: &[(&str, &[u8])] = &[\n");
    for path in files {
        src.push_str(&format!(
            "({:?}, include_bytes!({:?})),\n",
            path.strip_prefix(&dir).unwrap().to_str().unwrap().replace('\\', "/"),
            path.canonicalize().unwrap()
        ));
    }
    src.push_str("];\n");
    std::fs::write(
        std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("companion_assets.rs"),
        src,
    )
    .unwrap();
}
