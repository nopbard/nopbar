use std::{path::PathBuf, sync::OnceLock};
pub fn executable() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let root = std::env::temp_dir().join(format!("nopbar-d2-fixture-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("fixture.rs");
        std::fs::write(&source, include_str!("fixtures/cli.rs")).unwrap();
        let exe = root.join("fixture.exe");
        let status = std::process::Command::new("rustc")
            .args(["--edition=2021", "--crate-name", "nopbar_fixture"])
            .arg(&source)
            .arg("-o")
            .arg(&exe)
            .status()
            .unwrap();
        assert!(status.success());
        exe
    })
    .clone()
}
