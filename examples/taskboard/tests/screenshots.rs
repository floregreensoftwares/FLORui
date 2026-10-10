//! Renders each screen headless and writes it as a PNG, to look at the design
//! without a window. Not part of the normal run:
//!
//! `TASKBOARD_SHOTS=some/dir cargo test --test screenshots -- --ignored`

use florui_test::{Harness, by_id};
use taskboard::app::{App, AppProps};

fn write_png(path: &std::path::Path, width: u32, height: u32, premultiplied: &[u8]) {
    // The frame is premultiplied; a screen is opaque, so the bytes are as they are.
    let file = std::fs::File::create(path).expect("a file to write");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .expect("a header")
        .write_image_data(premultiplied)
        .expect("the pixels");
}

#[test]
#[ignore = "writes images for looking at"]
fn each_screen_as_a_picture() {
    let dir = std::env::var("TASKBOARD_SHOTS").expect("TASKBOARD_SHOTS names a directory");
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the directory");

    let mut mounted = Harness::new(|| App(AppProps {}))
        .css(&taskboard::stylesheet())
        .viewport(1180.0, 760.0)
        .mount();
    for (name, key) in [
        ("desk", None),
        ("stack", Some("nav-stack")),
        ("settings", Some("nav-settings")),
    ] {
        if let Some(id) = key {
            let node = mounted.get(by_id(id));
            mounted.click(node);
        }
        let frame = mounted.frame();
        write_png(
            &dir.join(format!("{name}.png")),
            frame.width,
            frame.height,
            &frame.rgba,
        );
    }
}
