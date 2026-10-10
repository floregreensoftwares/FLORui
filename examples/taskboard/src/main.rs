use florui_platform::WindowOptions;
use florui_style::Rgba;
use taskboard::app::{App, AppProps};

fn main() {
    let css = taskboard::stylesheet();
    let options = WindowOptions {
        size: Some((1180.0, 760.0)),
        min_size: Some((900.0, 600.0)),
        ..WindowOptions::default()
    };
    if let Err(error) = florui_platform::run_with_options(
        "Taskboard",
        &css,
        Rgba::opaque(0x2a, 0x1c, 0x12),
        options,
        || App(AppProps {}),
    ) {
        eprintln!("taskboard: {error}");
        std::process::exit(1);
    }
}
