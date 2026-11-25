//! Uses the same full-color body raster as the live companion.
use super::*;

fn sprite(
    raster: &mut render::Raster,
    frame: &str,
    size: f32,
    dpi: f32,
    light: bool,
) -> Vec<u8> {
    let mut pose = Motion::new(0.).pose();
    pose.frame = frame.into();
    let (x, y) = (110., 210.);
    let mut px = vec![0; (raster.w * raster.h * 4) as usize];
    unsafe {
        raster
            .draw(&pose, x, y, size, dpi, false, 0., false, light, &mut px)
            .unwrap();
    }
    for pixel in px.as_chunks_mut::<4>().0.iter_mut() {
        let background = if light { 246. } else { 30. };
        let inverse = 1. - pixel[3] as f32 / 255.;
        for c in &mut pixel[..3] {
            *c = (*c as f32 + background * inverse).round() as u8;
        }
        pixel[3] = 255;
    }
    px
}

#[test]
#[ignore = "writes companion-polish sprite matrix and all-frame color atlas offscreen"]
fn companion_polish_sprites() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/ui-snapshots/companion-polish");
    let device = kit::Canvas::at(1., 1., 1.);
    let dc = unsafe {
        device
            .dc
            .GetDevice()
            .unwrap()
            .CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)
            .unwrap()
    };
    let mut index = String::from("# Sprite visual acceptance\n\nSame runtime raster, no desktop capture.\n\n");
    for dpi in [1., 1.5] {
        let (w, h) = ((220. * dpi) as u32, (240. * dpi) as u32);
        let mut raster = unsafe { render::Raster::new(dc.clone(), w, h).unwrap() };
        for size in [0.28, 0.42, 0.56] {
            for light in [true, false] {
                for (name, frame) in [("idle", "idle_0"), ("sign-in-stretch", "stretch_2")] {
                    let px = sprite(&mut raster, frame, size, dpi, light);
                    let file = format!(
                        "sprites/{}-{size}-{dpi}x-{name}.png",
                        if light { "light" } else { "dark" }
                    );
                    kit::save_png(&px, w, h, &root.join(&file));
                    index += &format!("- [{file}]({file}) {frame}\n");
                }
            }
        }
    }
    // Every shipped frame, indexed, so the original colors can be reviewed on every posture.
    let mut frames = clips::catalog()
        .frames
        .keys()
        .filter(|name| clips::png(name, "full").is_some())
        .cloned()
        .collect::<Vec<_>>();
    frames.sort();
    let mut raster = unsafe { render::Raster::new(dc, 220, 240).unwrap() };
    for (page, chunk) in frames.chunks(24).enumerate() {
        let mut atlas = vec![246; 220 * 6 * 240 * 4 * 4];
        for p in atlas.as_chunks_mut::<4>().0.iter_mut() {
            p[3] = 255;
        }
        let file = format!("sprites/frames-{page:02}.png");
        index += &format!("\n## [{file}]({file})\n\nRows left to right, six columns:\n\n");
        for (i, frame) in chunk.iter().enumerate() {
            let px = sprite(&mut raster, frame, 0.56, 1., true);
            for row in 0..240 {
                let target = (((i / 6) * 240 + row) * 1320 + (i % 6) * 220) * 4;
                atlas[target..target + 880].copy_from_slice(&px[row * 880..(row + 1) * 880]);
            }
            index += &format!("{}. {frame}\n", i + 1);
        }
        kit::save_png(&atlas, 1320, 960, &root.join(file));
    }
    std::fs::write(root.join("SPRITES.md"), index).unwrap();
    println!(
        "24 sprite states + {} color sheets in {}",
        frames.len().div_ceil(24),
        root.display()
    );
}
