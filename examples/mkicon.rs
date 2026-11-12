//! Rasterises `assets/icon/icon.svg` into the PNG sizes macOS wants.
//! Run through `scripts/make-icon.sh`, which then calls `iconutil`.

use resvg::{tiny_skia, usvg};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let svg = args.next().ok_or("usage: mkicon <icon.svg> <out-dir>")?;
    let out = std::path::PathBuf::from(args.next().ok_or("usage: mkicon <icon.svg> <out-dir>")?);
    std::fs::create_dir_all(&out)?;
    let tree = usvg::Tree::from_data(&std::fs::read(&svg)?, &usvg::Options::default())?;
    let render = |px: u32| -> Result<tiny_skia::Pixmap, Box<dyn std::error::Error>> {
        let mut pixmap = tiny_skia::Pixmap::new(px, px).ok_or("bad size")?;
        let scale = px as f32 / tree.size().width();
        resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
        Ok(pixmap)
    };
    for (name, px) in [
        ("icon_16x16.png", 16),
        ("icon_16x16@2x.png", 32),
        ("icon_32x32.png", 32),
        ("icon_32x32@2x.png", 64),
        ("icon_128x128.png", 128),
        ("icon_128x128@2x.png", 256),
        ("icon_256x256.png", 256),
        ("icon_256x256@2x.png", 512),
        ("icon_512x512.png", 512),
        ("icon_512x512@2x.png", 1024),
    ] {
        render(px)?.save_png(out.join(name))?;
    }
    Ok(())
}
