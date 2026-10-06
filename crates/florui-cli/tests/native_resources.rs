//! What `florui build` writes into the staged executable: the icon and the
//! version information, read back through the Windows API the way Explorer
//! reads them.

#![cfg(windows)]

use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use windows_sys::Win32::Foundation::FreeLibrary;
use windows_sys::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows_sys::Win32::System::LibraryLoader::{
    FindResourceW, LOAD_LIBRARY_AS_DATAFILE, LoadLibraryExW, LoadResource, LockResource,
    SizeofResource,
};

const RT_ICON: usize = 3;
const RT_GROUP_ICON: usize = 14;
const RT_VERSION: usize = 16;

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64"><circle cx="32" cy="32" r="30" fill="#c81e3c"/></svg>"##;

const CONFIG: &str = "schema_version = 1\n\n[app]\nname = \"Garden\"\nidentifier = \"com.example.garden\"\ndescription = \"A workspace for ideas\"\n\n[app.icons]\nsource = \"icon.svg\"\n\n[bundle]\npublisher = \"Floregreen\"\n";

fn write(dir: &Path, relative: &str, text: &str) {
    let path = dir.join(relative);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn app(dir: &Path, version: &str, config: Option<&str>) {
    write(
        dir,
        "Cargo.toml",
        &format!("[package]\nname = \"garden\"\nversion = \"{version}\"\nedition = \"2021\"\n"),
    );
    write(dir, "src/main.rs", "fn main() { println!(\"running\"); }\n");
    if let Some(config) = config {
        write(dir, "florui.config.toml", config);
    }
}

fn florui_build(dir: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_florui"))
        .arg("build")
        .current_dir(dir)
        .env_remove("CARGO_TARGET_DIR")
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn staged(dir: &Path) -> PathBuf {
    dir.join("target/florui-build/garden/native")
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// One resource's bytes, read from the file without running it.
fn resource(exe: &Path, kind: usize, id: usize) -> Option<Vec<u8>> {
    // SAFETY: the path is NUL-terminated; the resource memory is copied
    // before the module is released.
    unsafe {
        let module = LoadLibraryExW(
            wide(exe).as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_AS_DATAFILE,
        );
        assert!(!module.is_null(), "could not load {}", exe.display());
        let found = FindResourceW(module, id as _, kind as _);
        let bytes = if found.is_null() {
            None
        } else {
            let loaded = LoadResource(module, found);
            let size = SizeofResource(module, found) as usize;
            let data = LockResource(loaded) as *const u8;
            Some(std::slice::from_raw_parts(data, size).to_vec())
        };
        FreeLibrary(module);
        bytes
    }
}

/// A string of the executable's version information, as Explorer shows it.
fn version_string(exe: &Path, name: &str) -> Option<String> {
    let path = wide(exe);
    // SAFETY: buffers are sized by the API's own answer; the returned
    // pointer points into `buffer`, which outlives the read.
    unsafe {
        let mut handle = 0;
        let size = GetFileVersionInfoSizeW(path.as_ptr(), &mut handle);
        if size == 0 {
            return None;
        }
        let mut buffer = vec![0u8; size as usize];
        if GetFileVersionInfoW(path.as_ptr(), 0, size, buffer.as_mut_ptr().cast()) == 0 {
            return None;
        }
        let block: Vec<u16> = format!("\\StringFileInfo\\040904b0\\{name}")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut value: *mut core::ffi::c_void = std::ptr::null_mut();
        let mut length = 0u32;
        if VerQueryValueW(
            buffer.as_ptr().cast(),
            block.as_ptr(),
            &mut value,
            &mut length,
        ) == 0
        {
            return None;
        }
        let units = std::slice::from_raw_parts(value as *const u16, length as usize);
        let end = units
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(units.len());
        Some(String::from_utf16_lossy(&units[..end]))
    }
}

fn png_size(png: &[u8]) -> (u32, u32) {
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let be = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    (be(16), be(20))
}

#[test]
fn the_staged_executable_carries_the_declared_icon_and_identity_and_cargos_file_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    app(dir.path(), "1.2.3-rc.1+build5", Some(CONFIG));
    write(dir.path(), "icon.svg", SVG);

    let output = florui_build(dir.path());

    let all = text(&output);
    assert_eq!(output.status.code(), Some(0), "{all}");
    let exe = staged(dir.path()).join("garden.exe");
    assert_eq!(
        version_string(&exe, "ProductName").as_deref(),
        Some("Garden")
    );
    assert_eq!(
        version_string(&exe, "CompanyName").as_deref(),
        Some("Floregreen")
    );
    assert_eq!(
        version_string(&exe, "FileDescription").as_deref(),
        Some("A workspace for ideas")
    );
    assert_eq!(
        version_string(&exe, "ProductVersion").as_deref(),
        Some("1.2.3-rc.1+build5")
    );
    assert_eq!(
        version_string(&exe, "OriginalFilename").as_deref(),
        Some("garden.exe")
    );

    let group = resource(&exe, RT_GROUP_ICON, 1).expect("no icon group");
    let count = u16::from_le_bytes([group[4], group[5]]) as usize;
    assert_eq!(count, 7, "{all}");
    for index in 0..count {
        let entry = &group[6 + 14 * index..6 + 14 * (index + 1)];
        let id = u16::from_le_bytes([entry[12], entry[13]]) as usize;
        let png = resource(&exe, RT_ICON, id).expect("an icon image is missing");
        let declared = if entry[0] == 0 {
            256
        } else {
            u32::from(entry[0])
        };
        assert_eq!(png_size(&png), (declared, declared));
        assert_eq!(
            u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize,
            png.len()
        );
    }

    let cargo_output = dir.path().join("target/release/garden.exe");
    assert!(
        resource(&cargo_output, RT_GROUP_ICON, 1).is_none(),
        "cargo's file was patched"
    );
    assert!(
        resource(&cargo_output, RT_VERSION, 1).is_none(),
        "cargo's file was patched"
    );

    // The staged executable runs, and on its own.
    let alone = tempfile::tempdir().unwrap();
    let copy = alone.path().join("garden.exe");
    std::fs::copy(&exe, &copy).unwrap();
    let run = Command::new(&copy).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "running");
}

#[test]
fn the_report_lists_the_icon_its_source_and_the_version_information() {
    let dir = tempfile::tempdir().unwrap();
    app(dir.path(), "0.4.0", Some(CONFIG));
    write(dir.path(), "icon.svg", SVG);

    assert_eq!(florui_build(dir.path()).status.code(), Some(0));

    let text = std::fs::read_to_string(staged(dir.path()).join("report.json")).unwrap();
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    let resources = &report["resources"];
    assert_eq!(resources["status"], "applied");
    assert_eq!(resources["icon"]["declared_as"], "app.icons.source");
    assert_eq!(resources["icon"]["source"], "icon.svg");
    assert_eq!(
        resources["icon"]["sizes"],
        serde_json::json!([16, 24, 32, 48, 64, 128, 256])
    );
    assert_eq!(
        resources["icon"]["source_sha256"].as_str().unwrap().len(),
        64
    );
    assert_eq!(resources["version_info"][0]["company_name"], "Floregreen");
    let files: Vec<&str> = report["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["file"].as_str().unwrap())
        .collect();
    assert!(
        files.contains(&"icon.ico") && files.contains(&"garden.exe"),
        "{files:?}"
    );
    assert!(staged(dir.path()).join("icon.ico").is_file());
}

#[test]
fn a_declared_icon_that_cannot_be_converted_fails_the_build_and_stages_nothing() {
    let dir = tempfile::tempdir().unwrap();
    app(dir.path(), "0.1.0", Some(CONFIG));
    write(dir.path(), "icon.svg", "<svg");
    // An earlier good build must not survive as if it were this one.
    write(
        dir.path(),
        "target/florui-build/garden/native/garden.exe",
        "old",
    );

    let output = florui_build(dir.path());

    let all = text(&output);
    assert_eq!(output.status.code(), Some(1), "{all}");
    assert!(all.contains("app.icons.source"), "{all}");
    assert!(!staged(dir.path()).join("garden.exe").exists(), "{all}");
    assert!(!staged(dir.path()).join("report.json").exists(), "{all}");
}

#[test]
fn without_a_declared_icon_the_executable_has_identity_but_the_default_icon() {
    let dir = tempfile::tempdir().unwrap();
    app(dir.path(), "0.1.0", None);

    let output = florui_build(dir.path());

    let all = text(&output);
    assert_eq!(output.status.code(), Some(0), "{all}");
    let exe = staged(dir.path()).join("garden.exe");
    assert!(resource(&exe, RT_GROUP_ICON, 1).is_none());
    assert_eq!(
        version_string(&exe, "ProductName").as_deref(),
        Some("garden")
    );
    assert_eq!(
        version_string(&exe, "CompanyName"),
        None,
        "a publisher must not be invented"
    );
    let text = std::fs::read_to_string(staged(dir.path()).join("report.json")).unwrap();
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(report["resources"]["icon"].is_null());
    assert!(
        report["resources"]["notes"][0]
            .as_str()
            .unwrap()
            .contains("default icon")
    );
}

#[test]
fn a_small_png_builds_with_a_warning_and_without_the_sizes_it_cannot_fill() {
    let dir = tempfile::tempdir().unwrap();
    app(
        dir.path(),
        "0.1.0",
        Some(&CONFIG.replace("icon.svg", "icon.png")),
    );
    let image = image::RgbaImage::from_pixel(64, 64, image::Rgba([10, 120, 200, 255]));
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).unwrap();
    std::fs::write(dir.path().join("icon.png"), png.into_inner()).unwrap();

    let output = florui_build(dir.path());

    let all = text(&output);
    assert_eq!(output.status.code(), Some(0), "{all}");
    let exe = staged(dir.path()).join("garden.exe");
    let group = resource(&exe, RT_GROUP_ICON, 1).unwrap();
    assert_eq!(u16::from_le_bytes([group[4], group[5]]), 5, "16 to 64 only");
    let text = std::fs::read_to_string(staged(dir.path()).join("report.json")).unwrap();
    let report: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["outcome"], "built_with_warnings");
    assert_eq!(report["resources"]["icon"]["skipped"][1]["size"], 256);
}

#[test]
fn two_builds_of_the_same_inputs_stage_the_same_executable_resources() {
    let dir = tempfile::tempdir().unwrap();
    app(dir.path(), "0.1.0", Some(CONFIG));
    write(dir.path(), "icon.svg", SVG);
    assert_eq!(florui_build(dir.path()).status.code(), Some(0));
    let first = std::fs::read(staged(dir.path()).join("icon.ico")).unwrap();
    let first_group = resource(&staged(dir.path()).join("garden.exe"), RT_GROUP_ICON, 1);

    assert_eq!(florui_build(dir.path()).status.code(), Some(0));

    assert_eq!(
        std::fs::read(staged(dir.path()).join("icon.ico")).unwrap(),
        first
    );
    assert_eq!(
        resource(&staged(dir.path()).join("garden.exe"), RT_GROUP_ICON, 1),
        first_group
    );
}
