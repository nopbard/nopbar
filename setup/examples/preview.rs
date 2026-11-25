//! Renders the setup window offscreen into PNGs (light and dark, each
//! state) without running any installer:
//!   cargo run -p nopbar-setup --example preview -- <folder> [scale]
fn main() {
    let mut args = std::env::args_os().skip(1);
    let dir = std::path::PathBuf::from(args.next().unwrap_or_else(|| "setup-preview".into()));
    let scale: f32 = args.next().and_then(|s| s.to_str()?.parse().ok()).unwrap_or(2.0);
    match nopbar_setup::ui::preview::render_all(&dir, scale) {
        Ok(files) => files.iter().for_each(|f| println!("{}", f.display())),
        Err(e) => {
            eprintln!("preview: {e}");
            std::process::exit(1);
        }
    }
}
