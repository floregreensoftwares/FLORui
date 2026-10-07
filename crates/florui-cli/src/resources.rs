//! The identity a native executable carries: the icon and the version
//! information shown in Explorer, generated from the resolved configuration
//! and written into the staged copy of the executable (never into the file
//! cargo produced).
//!
//! The icon is rasterized from the declared SVG (the pinned `resvg`, no
//! network, no fonts) or resized from the declared PNG, at the sizes Windows
//! asks for, never above the source's own resolution and never cropped. Edges
//! stay transparent. Generation is deterministic, so there is no cache to
//! disagree with a clean build.

use std::path::{Path, PathBuf};

use image::{ImageFormat, RgbaImage, imageops};
use serde::Serialize;
use sha2::{Digest, Sha256};

use florui_assets::{RasterFit, RasterImage, decode_png, parse_svg, rasterize_svg};
use florui_config::IconsConfig;

/// The sizes of an application icon: the small ones for title bars and lists,
/// 256 for large views.
const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

pub struct Identity<'a> {
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub publisher: Option<&'a str>,
    pub copyright: Option<&'a str>,
    pub version: &'a str,
    /// The locale whose name and description the executable shows by default.
    pub default_locale: &'a str,
    /// Every declared locale, with the name and description it resolves to.
    pub locales: &'a [LocaleIdentity],
}

/// A locale's name and description after the fallback chain.
pub struct LocaleIdentity {
    pub tag: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct SkippedSize {
    pub size: u32,
    pub reason: String,
}

/// What the icon was made from and what came out of it.
#[derive(Serialize, Clone)]
pub struct IconAsset {
    /// `app.icons.windows` or `app.icons.source`.
    pub declared_as: &'static str,
    pub source: String,
    pub kind: &'static str,
    pub source_sha256: String,
    /// `None` for a vector source, which has no pixel size of its own.
    pub source_pixels: Option<(u32, u32)>,
    pub sizes: Vec<u32>,
    pub skipped: Vec<SkippedSize>,
    /// The source is not square and was centered on a transparent square.
    pub padded_to_square: bool,
}

/// What the report says about one locale's strings.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct LocalizedFields {
    pub tag: String,
    /// The Windows language identifier, as four hex digits (`0416`), when the
    /// tag has one.
    pub language_id: Option<String>,
    pub product_name: String,
    pub file_description: String,
    pub embedded: bool,
    /// Why the strings are not in the executable.
    pub reason: Option<String>,
}

/// One string table of the version resource.
#[derive(Clone, Debug)]
pub struct Table {
    language: u16,
    product_name: String,
    file_description: String,
}

#[derive(Serialize, Clone)]
pub struct VersionFields {
    pub product_name: String,
    pub file_description: String,
    pub company_name: Option<String>,
    pub legal_copyright: Option<String>,
    pub file_version: String,
    pub product_version: String,
    pub original_filename: String,
    pub internal_name: String,
    /// Each declared locale, embedded or not.
    pub localized: Vec<LocalizedFields>,
    /// The tables written, the default locale's first.
    #[serde(skip)]
    pub tables: Vec<Table>,
}

#[derive(Serialize, Clone)]
pub struct ResourcesReport {
    /// `applied`, `not_declared` (nothing to embed beyond version information
    /// the configuration lacks) or `unsupported_host`.
    pub status: &'static str,
    pub icon: Option<IconAsset>,
    pub version_info: Vec<VersionFields>,
    pub warnings: Vec<String>,
    pub notes: Vec<&'static str>,
}

/// The generated icon: one PNG per size, ascending.
pub struct Icon {
    pub entries: Vec<(u32, Vec<u8>)>,
    pub asset: IconAsset,
    pub warnings: Vec<String>,
}

pub fn unsupported_host() -> ResourcesReport {
    ResourcesReport {
        status: "unsupported_host",
        icon: None,
        version_info: Vec::new(),
        warnings: Vec::new(),
        notes: vec!["native executable resources are only written on Windows hosts"],
    }
}

/// Picks the declared icon: the Windows one, else the common one.
pub fn declared_icon(icons: &IconsConfig) -> Option<(&'static str, &Path)> {
    icons
        .windows
        .as_deref()
        .map(|path| ("app.icons.windows", path))
        .or_else(|| {
            icons
                .source
                .as_deref()
                .map(|path| ("app.icons.source", path))
        })
}

/// Reads and converts the declared icon. `project_dir` only shortens the
/// source path shown in the report.
pub fn generate_icon(
    declared_as: &'static str,
    path: &Path,
    project_dir: &Path,
) -> Result<Icon, String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("{declared_as}: could not read {}: {error}", path.display()))?;
    let extension = path
        .extension()
        .map(|extension| extension.to_string_lossy().to_ascii_lowercase());
    let problem = |detail: String| format!("{declared_as}: {}: {detail}", path.display());

    let mut warnings = Vec::new();
    let mut skipped = Vec::new();
    let (kind, source_pixels, entries, padded) = match extension.as_deref() {
        Some("svg") => {
            let text = String::from_utf8(bytes.clone())
                .map_err(|_| problem("the SVG is not valid UTF-8 text".to_string()))?;
            let vector = parse_svg(&text).map_err(|error| problem(error.to_string()))?;
            let intrinsic = vector.intrinsic_size;
            let padded = (intrinsic.width - intrinsic.height).abs() > f32::EPSILON;
            let mut entries = Vec::new();
            for size in SIZES {
                let raster = rasterize_svg(
                    &vector,
                    RasterFit::Contain {
                        width: size,
                        height: size,
                    },
                )
                .map_err(|error| problem(error.to_string()))?;
                entries.push((size, encode_png(&raster).map_err(&problem)?));
            }
            ("svg", None, entries, padded)
        }
        Some("png") => {
            let source = decode_png(&bytes).map_err(|error| problem(error.to_string()))?;
            let longest = source.width.max(source.height);
            let mut entries = Vec::new();
            for size in SIZES {
                if size > longest {
                    skipped.push(SkippedSize {
                        size,
                        reason: format!(
                            "the source is {}x{} and is not enlarged",
                            source.width, source.height
                        ),
                    });
                    continue;
                }
                let raster = fit_into_square(&source, size);
                entries.push((size, encode_png(&raster).map_err(&problem)?));
            }
            (
                "png",
                Some((source.width, source.height)),
                entries,
                source.width != source.height,
            )
        }
        other => {
            return Err(problem(format!(
                "unsupported icon type ({}); only .svg and .png are supported",
                other.unwrap_or("no extension")
            )));
        }
    };

    if entries.is_empty() {
        return Err(problem(format!(
            "the source is smaller than {}x{} pixels, the smallest icon size",
            SIZES[0], SIZES[0]
        )));
    }
    if !skipped.is_empty() {
        warnings.push(format!(
            "{declared_as} is smaller than {0}x{0}: the {0} px entry used by large icon views was not generated",
            SIZES[SIZES.len() - 1]
        ));
    }

    let source_shown = path
        .strip_prefix(project_dir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    let sizes = entries.iter().map(|(size, _)| *size).collect();
    Ok(Icon {
        asset: IconAsset {
            declared_as,
            source: source_shown,
            kind,
            source_sha256: hex(&Sha256::digest(&bytes)),
            source_pixels,
            sizes,
            skipped,
            padded_to_square: padded,
        },
        entries,
        warnings,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn encode_png(raster: &RasterImage) -> Result<Vec<u8>, String> {
    let image = RgbaImage::from_raw(raster.width, raster.height, raster.rgba.clone())
        .ok_or_else(|| "internal error: raster size does not match its pixels".to_string())?;
    let mut out = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut out, ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(out.into_inner())
}

/// Scales `source` down to fit a `size` x `size` square, keeping its
/// proportions and centering it on transparent pixels. Colors are scaled
/// premultiplied, so a transparent edge does not darken or tint the pixels
/// beside it.
fn fit_into_square(source: &RasterImage, size: u32) -> RasterImage {
    let longest = source.width.max(source.height) as f64;
    let scale = (size as f64 / longest).min(1.0);
    let width = ((source.width as f64 * scale).round() as u32).max(1);
    let height = ((source.height as f64 * scale).round() as u32).max(1);

    let mut premultiplied = source.rgba.clone();
    for pixel in premultiplied.chunks_exact_mut(4) {
        let alpha = pixel[3] as u32;
        for channel in &mut pixel[..3] {
            *channel = ((*channel as u32 * alpha + 127) / 255) as u8;
        }
    }
    let input = RgbaImage::from_raw(source.width, source.height, premultiplied)
        .expect("a decoded raster has width*height*4 bytes");
    let mut resized = if (width, height) == (source.width, source.height) {
        input
    } else {
        imageops::resize(&input, width, height, imageops::FilterType::Lanczos3)
    };
    for pixel in resized.pixels_mut() {
        let alpha = pixel[3] as u32;
        if alpha > 0 {
            for channel in &mut pixel.0[..3] {
                *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }

    let mut canvas = RgbaImage::new(size, size);
    imageops::replace(
        &mut canvas,
        &resized,
        ((size - width) / 2) as i64,
        ((size - height) / 2) as i64,
    );
    RasterImage {
        rgba: canvas.into_raw(),
        width: size,
        height: size,
    }
}

/// The `.ico` file: a directory of PNG-compressed images.
pub fn ico_file(entries: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend([0, 0, 1, 0]);
    out.extend((entries.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * entries.len() as u32;
    for (size, png) in entries {
        let byte = if *size >= 256 { 0 } else { *size as u8 };
        out.extend([byte, byte, 0, 0]);
        out.extend(1u16.to_le_bytes());
        out.extend(32u16.to_le_bytes());
        out.extend((png.len() as u32).to_le_bytes());
        out.extend(offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in entries {
        out.extend(png);
    }
    out
}

/// The `RT_GROUP_ICON` resource: the directory with resource ids instead of
/// file offsets (the image of entry `i` is `RT_ICON` id `i + 1`).
pub fn group_icon_resource(entries: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend([0, 0, 1, 0]);
    out.extend((entries.len() as u16).to_le_bytes());
    for (index, (size, png)) in entries.iter().enumerate() {
        let byte = if *size >= 256 { 0 } else { *size as u8 };
        out.extend([byte, byte, 0, 0]);
        out.extend(1u16.to_le_bytes());
        out.extend(32u16.to_le_bytes());
        out.extend((png.len() as u32).to_le_bytes());
        out.extend((index as u16 + 1).to_le_bytes());
    }
    out
}

/// Version information for one executable. The numeric fields are the
/// leading `major.minor.patch` of the version; the strings keep the whole
/// version, prerelease and build metadata included.
pub fn version_fields(identity: &Identity<'_>, file_name: &str) -> VersionFields {
    let (tables, localized) = locale_tables(identity);
    let default = &tables[0];
    VersionFields {
        product_name: default.product_name.clone(),
        file_description: default.file_description.clone(),
        company_name: identity.publisher.map(str::to_string),
        legal_copyright: identity.copyright.map(str::to_string),
        file_version: identity.version.to_string(),
        product_version: identity.version.to_string(),
        original_filename: file_name.to_string(),
        internal_name: Path::new(file_name).file_stem().map_or_else(
            || file_name.to_string(),
            |stem| stem.to_string_lossy().into_owned(),
        ),
        localized,
        tables,
    }
}

/// Every locale the configuration declares, and the default one even when it
/// declares nothing, each with the name and description its fallback chain
/// gives.
pub fn declared_locales(app: &florui_config::AppConfig) -> Vec<LocaleIdentity> {
    let mut tags: Vec<&str> = app.locales.locales.keys().map(String::as_str).collect();
    if !tags
        .iter()
        .any(|tag| tag.eq_ignore_ascii_case(&app.locales.default_locale))
    {
        tags.push(&app.locales.default_locale);
    }
    tags.into_iter()
        .map(|tag| {
            let identity = app.localized_identity(tag);
            LocaleIdentity {
                tag: tag.to_string(),
                name: identity.name.to_string(),
                description: identity.description.map(str::to_string),
            }
        })
        .collect()
}

/// The Windows language identifier of a locale tag, for the languages this
/// tool maps. `None` is a tag it does not know, which is reported and not
/// guessed.
pub fn windows_language_id(tag: &str) -> Option<u16> {
    Some(match tag.to_ascii_lowercase().as_str() {
        "en" | "en-us" => 0x0409,
        "en-gb" => 0x0809,
        "pt-br" | "pt" => 0x0416,
        "pt-pt" => 0x0816,
        "es" | "es-es" => 0x0C0A,
        "es-mx" => 0x080A,
        "fr" | "fr-fr" => 0x040C,
        "de" | "de-de" => 0x0407,
        "it" | "it-it" => 0x0410,
        "nl" | "nl-nl" => 0x0413,
        "sv" | "sv-se" => 0x041D,
        "pl" | "pl-pl" => 0x0415,
        "ru" | "ru-ru" => 0x0419,
        "tr" | "tr-tr" => 0x041F,
        "ja" | "ja-jp" => 0x0411,
        "ko" | "ko-kr" => 0x0412,
        "zh-cn" | "zh-hans" | "zh" => 0x0804,
        "zh-tw" | "zh-hant" => 0x0404,
        _ => return None,
    })
}

/// The string tables to write and what the report says of every locale. The
/// default locale's table is first (Windows falls back to the first table
/// when no language matches); a locale whose tag has no mapping, or maps to a
/// language already written, is reported and left out.
fn locale_tables(identity: &Identity<'_>) -> (Vec<Table>, Vec<LocalizedFields>) {
    let default = identity
        .locales
        .iter()
        .find(|locale| locale.tag.eq_ignore_ascii_case(identity.default_locale));
    let default_name = default.map_or(identity.name, |locale| locale.name.as_str());
    let default_description = default
        .and_then(|locale| locale.description.as_deref())
        .or(identity.description)
        .unwrap_or(default_name);
    let default_language = windows_language_id(identity.default_locale).unwrap_or(0x0409);
    let mut tables = vec![Table {
        language: default_language,
        product_name: default_name.to_string(),
        file_description: default_description.to_string(),
    }];
    let mut report = Vec::new();
    for locale in identity.locales {
        let description = locale
            .description
            .as_deref()
            .unwrap_or(&locale.name)
            .to_string();
        let language = windows_language_id(&locale.tag);
        let is_default = locale.tag.eq_ignore_ascii_case(identity.default_locale);
        let (embedded, reason) = match language {
            // The default locale's table always exists; without a mapping it is
            // written as US English, and that is said.
            None if is_default => (
                true,
                Some(format!(
                    "\"{}\" has no Windows language mapping, so the default strings are written as language 0409",
                    locale.tag
                )),
            ),
            None => (
                false,
                Some(format!(
                    "\"{}\" has no Windows language mapping, so its name and description are not written into the executable",
                    locale.tag
                )),
            ),
            Some(_) if is_default => (true, None),
            Some(id) if tables.iter().any(|table| table.language == id) => (
                false,
                Some(format!(
                    "\"{}\" is the Windows language {id:04x}, which another locale's strings already use",
                    locale.tag
                )),
            ),
            Some(id) => {
                tables.push(Table {
                    language: id,
                    product_name: locale.name.clone(),
                    file_description: description.clone(),
                });
                (true, None)
            }
        };
        report.push(LocalizedFields {
            tag: locale.tag.clone(),
            language_id: language
                .or_else(|| is_default.then_some(default_language))
                .map(|id| format!("{id:04x}")),
            product_name: locale.name.clone(),
            file_description: description,
            embedded,
            reason,
        });
    }
    (tables, report)
}

/// `major.minor.patch.0` from the leading numbers of `version`.
pub fn numeric_version(version: &str) -> Result<[u16; 4], String> {
    let core = version.split(['-', '+']).next().unwrap_or(version);
    let mut numbers = [0u16; 4];
    for (slot, part) in numbers.iter_mut().zip(core.split('.')) {
        *slot = part.parse::<u16>().map_err(|_| {
            format!(
                "app.version \"{version}\" does not map to a Windows version: \"{part}\" is not a number from 0 to 65535"
            )
        })?;
    }
    numbers[3] = 0;
    Ok(numbers)
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn pad4(bytes: &mut Vec<u8>) {
    while !bytes.len().is_multiple_of(4) {
        bytes.push(0);
    }
}

/// One node of the version resource: header, key, value, children.
fn node(key: &str, value_length: u16, text: bool, value: &[u8], children: &[Vec<u8>]) -> Vec<u8> {
    let mut out = vec![0, 0];
    out.extend(value_length.to_le_bytes());
    out.extend(u16::from(text).to_le_bytes());
    out.extend(utf16(key));
    pad4(&mut out);
    out.extend(value);
    for child in children {
        pad4(&mut out);
        out.extend(child);
    }
    let length = out.len() as u16;
    out[0..2].copy_from_slice(&length.to_le_bytes());
    out
}

/// The `RT_VERSION` resource (`VS_VERSIONINFO`) for `fields`, in the
/// language-neutral Unicode block.
pub fn version_resource(fields: &VersionFields) -> Result<Vec<u8>, String> {
    let numbers = numeric_version(&fields.file_version)?;
    let high = (u32::from(numbers[0]) << 16) | u32::from(numbers[1]);
    let low = (u32::from(numbers[2]) << 16) | u32::from(numbers[3]);
    let mut fixed = Vec::new();
    for value in [
        0xFEEF_04BD_u32,
        0x0001_0000,
        high,
        low,
        high,
        low,
        0x3F,
        0,
        0x0004_0004,
        1,
        0,
        0,
        0,
    ] {
        fixed.extend(value.to_le_bytes());
    }

    let tables: Vec<Vec<u8>> = fields
        .tables
        .iter()
        .map(|table| {
            let mut strings: Vec<(&str, &str)> = vec![
                ("FileDescription", &table.file_description),
                ("FileVersion", &fields.file_version),
                ("InternalName", &fields.internal_name),
                ("OriginalFilename", &fields.original_filename),
                ("ProductName", &table.product_name),
                ("ProductVersion", &fields.product_version),
            ];
            if let Some(company) = &fields.company_name {
                strings.push(("CompanyName", company));
            }
            if let Some(copyright) = &fields.legal_copyright {
                strings.push(("LegalCopyright", copyright));
            }
            strings.sort_by_key(|(key, _)| *key);
            let entries: Vec<Vec<u8>> = strings
                .iter()
                .map(|(key, value)| {
                    let words = value.encode_utf16().count() as u16 + 1;
                    node(key, words, true, &utf16(value), &[])
                })
                .collect();
            node(
                &format!("{:04x}04b0", table.language),
                0,
                true,
                &[],
                &entries,
            )
        })
        .collect();
    let string_info = node("StringFileInfo", 0, true, &[], &tables);
    let pairs: Vec<u8> = fields
        .tables
        .iter()
        .flat_map(|table| (0x04B0_0000_u32 | u32::from(table.language)).to_le_bytes())
        .collect();
    let translation = node("Translation", pairs.len() as u16, false, &pairs, &[]);
    let var_info = node("VarFileInfo", 0, true, &[], &[translation]);
    Ok(node(
        "VS_VERSION_INFO",
        fixed.len() as u16,
        false,
        &fixed,
        &[string_info, var_info],
    ))
}

/// Everything to write into one executable.
pub struct Payload<'a> {
    pub icon: Option<&'a [(u32, Vec<u8>)]>,
    pub version: Vec<u8>,
}

#[cfg(windows)]
pub fn apply(executable: &Path, payload: &Payload<'_>) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::{
        BeginUpdateResourceW, EndUpdateResourceW, UpdateResourceW,
    };

    const RT_ICON: usize = 3;
    const RT_GROUP_ICON: usize = 14;
    const RT_VERSION: usize = 16;

    let path: Vec<u16> = executable
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let describe = |what: &str| {
        format!(
            "could not write the {what} into {}: {}",
            executable.display(),
            std::io::Error::last_os_error()
        )
    };
    // SAFETY: `path` is NUL-terminated and outlives the calls; every buffer
    // passed to `UpdateResourceW` is alive and its length is passed with it;
    // the update handle is finished (or discarded) before returning.
    unsafe {
        let handle = BeginUpdateResourceW(path.as_ptr(), 0);
        if handle.is_null() {
            return Err(describe("resources (could not open it for update)"));
        }
        let write = |kind: usize, id: usize, data: &[u8]| -> bool {
            UpdateResourceW(
                handle,
                kind as _,
                id as _,
                0,
                data.as_ptr().cast(),
                data.len() as u32,
            ) != 0
        };
        let mut ok = true;
        if let Some(entries) = payload.icon {
            for (index, (_, png)) in entries.iter().enumerate() {
                ok &= write(RT_ICON, index + 1, png);
            }
            ok &= write(RT_GROUP_ICON, 1, &group_icon_resource(entries));
        }
        ok &= write(RT_VERSION, 1, &payload.version);
        let failure = (!ok).then(|| describe("resources"));
        if EndUpdateResourceW(handle, i32::from(!ok)) == 0 && failure.is_none() {
            return Err(describe("resources (could not save them)"));
        }
        if let Some(failure) = failure {
            return Err(failure);
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn apply(_executable: &Path, _payload: &Payload<'_>) -> Result<(), String> {
    Err("native executable resources are only written on Windows hosts".to_string())
}

/// What an executable on disk carries, read without running it.
#[derive(Debug, Default, PartialEq)]
pub struct ExecutableResources {
    /// The strings of the version information that are present.
    pub version_strings: std::collections::BTreeMap<String, String>,
    /// The product name and description of each string table, by its Windows
    /// language identifier (four hex digits), in the order the file lists them.
    pub localized: Vec<(String, String, String)>,
    /// The number of images in the application icon, `None` when it has none.
    pub icon_entries: Option<usize>,
}

/// The version-information strings this tool writes, which are the ones read
/// back.
pub const VERSION_KEYS: [&str; 8] = [
    "ProductName",
    "CompanyName",
    "FileDescription",
    "FileVersion",
    "ProductVersion",
    "OriginalFilename",
    "InternalName",
    "LegalCopyright",
];

/// Reads the version information and the icon group out of `executable` as a
/// data file: nothing in it is executed.
#[cfg(windows)]
pub fn read_back(executable: &Path) -> Result<ExecutableResources, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::FreeLibrary;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
    };
    use windows_sys::Win32::System::LibraryLoader::{
        FindResourceW, LOAD_LIBRARY_AS_DATAFILE, LoadLibraryExW, LoadResource, LockResource,
        SizeofResource,
    };

    const RT_GROUP_ICON: usize = 14;

    let path: Vec<u16> = executable
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut resources = ExecutableResources::default();
    // SAFETY: `path` is NUL-terminated and outlives every call; the module is
    // released before returning; the version buffer is sized by the API's own
    // answer and the pointers `VerQueryValueW` returns point into it while it
    // is alive; resource memory is copied before the module is released.
    unsafe {
        let module = LoadLibraryExW(
            path.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_AS_DATAFILE,
        );
        if module.is_null() {
            return Err(format!(
                "could not open {} as a data file: {}",
                executable.display(),
                std::io::Error::last_os_error()
            ));
        }
        let group = FindResourceW(module, 1 as _, RT_GROUP_ICON as _);
        if !group.is_null() {
            let loaded = LoadResource(module, group);
            let size = SizeofResource(module, group) as usize;
            let data = LockResource(loaded) as *const u8;
            if size >= 6 && !data.is_null() {
                let header = std::slice::from_raw_parts(data, 6);
                resources.icon_entries = Some(u16::from_le_bytes([header[4], header[5]]) as usize);
            }
        }
        FreeLibrary(module);

        let mut handle = 0;
        let size = GetFileVersionInfoSizeW(path.as_ptr(), &mut handle);
        if size == 0 {
            return Ok(resources);
        }
        let mut buffer = vec![0u8; size as usize];
        if GetFileVersionInfoW(path.as_ptr(), 0, size, buffer.as_mut_ptr().cast()) == 0 {
            return Err(format!(
                "could not read the version information of {}: {}",
                executable.display(),
                std::io::Error::last_os_error()
            ));
        }
        let query = |block: &str| -> Option<String> {
            let block: Vec<u16> = block.encode_utf16().chain(Some(0)).collect();
            let mut value: *mut core::ffi::c_void = std::ptr::null_mut();
            let mut length = 0u32;
            if VerQueryValueW(
                buffer.as_ptr().cast(),
                block.as_ptr(),
                &mut value,
                &mut length,
            ) == 0
                || value.is_null()
            {
                return None;
            }
            let units = std::slice::from_raw_parts(value as *const u16, length as usize);
            let end = units
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(units.len());
            Some(String::from_utf16_lossy(&units[..end]))
        };
        // The `Translation` array lists the language and code page of every
        // string table, the default one first.
        let translations: Vec<(u16, u16)> = {
            let block: Vec<u16> = "\\VarFileInfo\\Translation"
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
                || value.is_null()
            {
                Vec::new()
            } else {
                std::slice::from_raw_parts(value as *const u16, length as usize / 2)
                    .chunks_exact(2)
                    .map(|pair| (pair[0], pair[1]))
                    .collect()
            }
        };
        let table = |(language, code_page): (u16, u16)| format!("{language:04x}{code_page:04x}");
        if let Some(&first) = translations.first() {
            for key in VERSION_KEYS {
                if let Some(value) = query(&format!("\\StringFileInfo\\{}\\{key}", table(first))) {
                    resources.version_strings.insert(key.to_string(), value);
                }
            }
        }
        for &pair in &translations {
            let read = |key: &str| {
                query(&format!("\\StringFileInfo\\{}\\{key}", table(pair))).unwrap_or_default()
            };
            resources.localized.push((
                format!("{:04x}", pair.0),
                read("ProductName"),
                read("FileDescription"),
            ));
        }
    }
    Ok(resources)
}

#[cfg(not(windows))]
pub fn read_back(_executable: &Path) -> Result<ExecutableResources, String> {
    Err("executable resources are only read on Windows hosts".to_string())
}

/// Where the managed `.ico` goes inside the staging directory.
pub fn ico_path(staging: &Path) -> PathBuf {
    staging.join("icon.ico")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_bytes(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let image = RgbaImage::from_fn(width, height, |x, y| image::Rgba(pixel(x, y)));
        let mut out = std::io::Cursor::new(Vec::new());
        image.write_to(&mut out, ImageFormat::Png).unwrap();
        out.into_inner()
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    const SQUARE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64"><circle cx="32" cy="32" r="30" fill="#c81e3c"/></svg>"##;
    const WIDE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="32" viewBox="0 0 64 32"><rect width="64" height="32" fill="#c81e3c"/></svg>"##;

    fn decode(png: &[u8]) -> RgbaImage {
        image::load_from_memory(png).unwrap().to_rgba8()
    }

    #[test]
    fn an_svg_gives_every_size_with_transparent_corners() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "a.svg", SQUARE_SVG.as_bytes());

        let icon = generate_icon("app.icons.source", &path, dir.path()).unwrap();

        assert_eq!(icon.asset.sizes, SIZES);
        assert!(icon.warnings.is_empty());
        let big = decode(&icon.entries.last().unwrap().1);
        assert_eq!(big.dimensions(), (256, 256));
        assert_eq!(
            big.get_pixel(0, 0)[3],
            0,
            "the corner must stay transparent"
        );
        assert_eq!(big.get_pixel(128, 128).0, [200, 30, 60, 255]);
    }

    #[test]
    fn a_png_is_never_enlarged_and_says_what_it_left_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "a.png",
            &png_bytes(64, 64, |_, _| [10, 20, 30, 255]),
        );

        let icon = generate_icon("app.icons.source", &path, dir.path()).unwrap();

        assert_eq!(icon.asset.sizes, [16, 24, 32, 48, 64]);
        let skipped: Vec<u32> = icon.asset.skipped.iter().map(|s| s.size).collect();
        assert_eq!(skipped, [128, 256]);
        assert_eq!(icon.warnings.len(), 1);
        assert_eq!(icon.asset.source_pixels, Some((64, 64)));
    }

    #[test]
    fn a_source_smaller_than_the_smallest_size_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(dir.path(), "a.png", &png_bytes(8, 8, |_, _| [1, 2, 3, 255]));

        let error = generate_icon("app.icons.source", &path, dir.path())
            .err()
            .unwrap();

        assert!(error.contains("smaller than 16x16"), "{error}");
    }

    #[test]
    fn a_wide_source_is_centered_on_a_square_not_cropped_or_stretched() {
        let dir = tempfile::tempdir().unwrap();
        let svg = write(dir.path(), "w.svg", WIDE_SVG.as_bytes());
        let png = write(
            dir.path(),
            "w.png",
            &png_bytes(64, 32, |_, _| [200, 30, 60, 255]),
        );

        for path in [svg, png] {
            let icon = generate_icon("app.icons.source", &path, dir.path()).unwrap();
            assert!(icon.asset.padded_to_square);
            let entry = decode(&icon.entries.iter().find(|(s, _)| *s == 64).unwrap().1);
            assert_eq!(entry.get_pixel(32, 0)[3], 0, "top band is padding");
            assert_eq!(entry.get_pixel(32, 63)[3], 0, "bottom band is padding");
            assert_eq!(
                entry.get_pixel(0, 32).0,
                [200, 30, 60, 255],
                "full width kept"
            );
            assert_eq!(entry.get_pixel(63, 32).0, [200, 30, 60, 255]);
        }
    }

    #[test]
    fn scaling_down_does_not_darken_the_pixels_beside_a_transparent_edge() {
        let dir = tempfile::tempdir().unwrap();
        // Left half opaque red, right half fully transparent but carrying green.
        // At 32 px the edge (column 54 of 100) falls inside output pixel 17.
        let png = png_bytes(100, 100, |x, _| {
            if x < 54 {
                [255, 0, 0, 255]
            } else {
                [0, 255, 0, 0]
            }
        });
        let path = write(dir.path(), "edge.png", &png);

        let icon = generate_icon("app.icons.source", &path, dir.path()).unwrap();

        let small = decode(&icon.entries.iter().find(|(s, _)| *s == 32).unwrap().1);
        let edge = small.get_pixel(17, 16);
        assert!(edge[3] > 0 && edge[3] < 255, "{edge:?}");
        assert!(
            edge[0] >= 240 && edge[1] <= 15,
            "edge pixel was tinted: {edge:?}"
        );
    }

    #[test]
    fn an_unsupported_or_missing_source_is_an_error_naming_the_field() {
        let dir = tempfile::tempdir().unwrap();
        let ico = write(dir.path(), "a.ico", b"not really");
        let bad_svg = write(dir.path(), "b.svg", b"<svg");

        let unsupported = generate_icon("app.icons.windows", &ico, dir.path())
            .err()
            .unwrap();
        let broken = generate_icon("app.icons.source", &bad_svg, dir.path())
            .err()
            .unwrap();
        let missing = generate_icon("app.icons.source", &dir.path().join("nope.svg"), dir.path())
            .err()
            .unwrap();

        assert!(
            unsupported.contains("app.icons.windows") && unsupported.contains("only .svg and .png")
        );
        assert!(broken.contains("app.icons.source"), "{broken}");
        assert!(missing.contains("could not read"), "{missing}");
    }

    #[test]
    fn the_windows_icon_is_preferred_over_the_common_one() {
        let icons = IconsConfig {
            source: Some(PathBuf::from("common.svg")),
            windows: Some(PathBuf::from("win.svg")),
            ..IconsConfig::default()
        };
        assert_eq!(declared_icon(&icons).unwrap().0, "app.icons.windows");
        let only_common = IconsConfig {
            source: Some(PathBuf::from("common.svg")),
            ..IconsConfig::default()
        };
        assert_eq!(declared_icon(&only_common).unwrap().0, "app.icons.source");
        assert!(declared_icon(&IconsConfig::default()).is_none());
    }

    #[test]
    fn the_ico_directory_matches_its_images() {
        let entries = vec![(16, vec![0x89, b'P', 1, 2, 3]), (256, vec![0x89, b'P', 9])];
        let ico = ico_file(&entries);

        assert_eq!(&ico[0..4], [0, 0, 1, 0]);
        assert_eq!(u16::from_le_bytes([ico[4], ico[5]]), 2);
        let first = &ico[6..22];
        let second = &ico[22..38];
        assert_eq!(first[0], 16);
        assert_eq!(second[0], 0, "256 is stored as 0");
        let length = |entry: &[u8]| u32::from_le_bytes(entry[8..12].try_into().unwrap());
        let offset = |entry: &[u8]| u32::from_le_bytes(entry[12..16].try_into().unwrap());
        assert_eq!((offset(first), length(first)), (38, 5));
        assert_eq!((offset(second), length(second)), (43, 3));
        assert_eq!(&ico[38..43], &entries[0].1[..]);
        assert_eq!(&ico[43..], &entries[1].1[..]);
    }

    #[test]
    fn the_group_resource_points_at_icon_ids_in_order() {
        let entries = vec![(16, vec![1, 2, 3]), (32, vec![4, 5])];
        let group = group_icon_resource(&entries);

        assert_eq!(group.len(), 6 + 14 * 2);
        let id = |index: usize| {
            let start = 6 + 14 * index + 12;
            u16::from_le_bytes([group[start], group[start + 1]])
        };
        assert_eq!((id(0), id(1)), (1, 2));
        assert_eq!(
            u32::from_le_bytes(group[14..18].try_into().unwrap()),
            3,
            "the image size of the first entry"
        );
    }

    #[test]
    fn versions_map_to_four_numbers_and_keep_the_full_string() {
        assert_eq!(numeric_version("1.2.3").unwrap(), [1, 2, 3, 0]);
        assert_eq!(numeric_version("1.2.3-rc.1+build5").unwrap(), [1, 2, 3, 0]);
        assert_eq!(numeric_version("2.0").unwrap(), [2, 0, 0, 0]);
        assert!(numeric_version("1.70000.0").unwrap_err().contains("70000"));

        let identity = Identity {
            name: "Garden",
            description: None,
            publisher: Some("Floregreen"),
            copyright: Some("Copyright 2026 Floregreen"),
            version: "1.2.3-rc.1+build5",
            default_locale: "en",
            locales: &[],
        };
        let fields = version_fields(&identity, "garden.exe");
        assert_eq!(fields.file_version, "1.2.3-rc.1+build5");
        assert_eq!(fields.file_description, "Garden", "falls back to the name");
        assert_eq!(fields.internal_name, "garden");
        assert_eq!(fields.original_filename, "garden.exe");
    }

    /// Reads the strings back out of a `VS_VERSIONINFO` the way a parser
    /// would: node lengths drive the walk, so a wrong length breaks it.
    fn read_tables(resource: &[u8]) -> Vec<(String, Vec<(String, String)>)> {
        fn utf16_at(bytes: &[u8], start: usize) -> (String, usize) {
            let mut units = Vec::new();
            let mut at = start;
            loop {
                let unit = u16::from_le_bytes([bytes[at], bytes[at + 1]]);
                at += 2;
                if unit == 0 {
                    break;
                }
                units.push(unit);
            }
            (String::from_utf16(&units).unwrap(), at)
        }
        fn header(bytes: &[u8], at: usize) -> (usize, usize, String, usize) {
            let length = u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
            let value = u16::from_le_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
            let (key, after) = utf16_at(bytes, at + 6);
            (length, value, key, after.next_multiple_of(4))
        }
        let (total, fixed_length, root, mut at) = header(resource, 0);
        assert_eq!(total, resource.len(), "the root length covers everything");
        assert_eq!(root, "VS_VERSION_INFO");
        at += fixed_length;
        let mut out: Vec<(String, Vec<(String, String)>)> = Vec::new();
        let (info_length, _, info_key, mut cursor) = header(resource, at.next_multiple_of(4));
        let info_end = at.next_multiple_of(4) + info_length;
        assert_eq!(info_key, "StringFileInfo");
        while cursor < info_end {
            let (table_length, _, table_key, first) = header(resource, cursor);
            let table_end = cursor + table_length;
            let mut entry = first;
            let mut strings = Vec::new();
            while entry < table_end {
                let (length, words, key, value_at) = header(resource, entry);
                let (value, _) = utf16_at(resource, value_at);
                assert_eq!(value.encode_utf16().count() + 1, words, "{key}");
                strings.push((key, value));
                entry += length;
                entry = entry.next_multiple_of(4);
            }
            out.push((table_key, strings));
            cursor = table_end.next_multiple_of(4);
        }
        out
    }

    /// The strings of the first (default) table.
    fn read_strings(resource: &[u8]) -> Vec<(String, String)> {
        let mut tables = read_tables(resource);
        let (key, strings) = tables.remove(0);
        assert_eq!(key, "040904b0");
        strings
    }

    #[test]
    fn the_version_resource_parses_back_with_every_field() {
        let identity = Identity {
            name: "Garden",
            description: Some("A workspace"),
            publisher: Some("Floregreen"),
            copyright: Some("Copyright 2026 Floregreen"),
            version: "1.2.3-rc.1+build5",
            default_locale: "en",
            locales: &[],
        };
        let fields = version_fields(&identity, "garden.exe");

        let resource = version_resource(&fields).unwrap();
        let strings = read_strings(&resource);

        let get = |key: &str| {
            strings
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("ProductName"), Some("Garden"));
        assert_eq!(get("CompanyName"), Some("Floregreen"));
        assert_eq!(get("FileDescription"), Some("A workspace"));
        assert_eq!(get("FileVersion"), Some("1.2.3-rc.1+build5"));
        assert_eq!(get("ProductVersion"), Some("1.2.3-rc.1+build5"));
        assert_eq!(get("OriginalFilename"), Some("garden.exe"));
        assert_eq!(get("InternalName"), Some("garden"));
    }

    #[test]
    fn a_publisher_that_is_not_configured_is_left_out_not_invented() {
        let identity = Identity {
            name: "Garden",
            description: None,
            publisher: None,
            copyright: None,
            version: "1.0.0",
            default_locale: "en",
            locales: &[],
        };
        let resource = version_resource(&version_fields(&identity, "garden.exe")).unwrap();
        assert!(
            read_strings(&resource)
                .iter()
                .all(|(k, _)| k != "CompanyName")
        );
    }

    fn locale(tag: &str, name: &str, description: Option<&str>) -> LocaleIdentity {
        LocaleIdentity {
            tag: tag.to_string(),
            name: name.to_string(),
            description: description.map(str::to_string),
        }
    }

    fn garden<'a>(default_locale: &'a str, locales: &'a [LocaleIdentity]) -> Identity<'a> {
        Identity {
            name: "Garden",
            description: Some("A workspace"),
            publisher: Some("Floregreen"),
            copyright: None,
            version: "1.0.0",
            default_locale,
            locales,
        }
    }

    #[test]
    fn locale_tags_map_to_windows_languages_and_unknown_ones_do_not() {
        assert_eq!(windows_language_id("pt-BR"), Some(0x0416));
        assert_eq!(
            windows_language_id("PT-br"),
            Some(0x0416),
            "case does not matter"
        );
        assert_eq!(windows_language_id("en"), Some(0x0409));
        assert_eq!(windows_language_id("ja"), Some(0x0411));
        assert_eq!(windows_language_id("tlh"), None, "no guessing");
    }

    #[test]
    fn each_mapped_locale_gets_a_table_with_the_default_first() {
        let locales = [
            locale("pt-BR", "Jardim", Some("Um espaco para ideias")),
            locale("en", "Garden", Some("A workspace for ideas")),
            locale("ja", "Niwa", None),
        ];
        let fields = version_fields(&garden("en", &locales), "garden.exe");
        let tables = read_tables(&version_resource(&fields).unwrap());

        let keys: Vec<&str> = tables.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(
            keys,
            ["040904b0", "041604b0", "041104b0"],
            "default first, then by tag"
        );
        let get = |table: usize, key: &str| {
            tables[table]
                .1
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get(0, "ProductName"), Some("Garden"));
        assert_eq!(get(0, "FileDescription"), Some("A workspace for ideas"));
        assert_eq!(get(1, "ProductName"), Some("Jardim"));
        assert_eq!(get(1, "FileDescription"), Some("Um espaco para ideias"));
        assert_eq!(
            get(2, "FileDescription"),
            Some("Niwa"),
            "no description falls back to the name"
        );
        // What is not localized is the same in every table.
        for table in 0..3 {
            assert_eq!(get(table, "CompanyName"), Some("Floregreen"));
            assert_eq!(get(table, "OriginalFilename"), Some("garden.exe"));
        }
        assert!(fields.localized.iter().all(|locale| locale.embedded));
        assert_eq!(fields.product_name, "Garden");
    }

    #[test]
    fn a_default_locale_other_than_english_leads_and_names_the_executable() {
        let locales = [
            locale("en", "Garden", None),
            locale("pt-BR", "Jardim", Some("Um espaco")),
        ];
        let fields = version_fields(&garden("pt-BR", &locales), "garden.exe");
        let tables = read_tables(&version_resource(&fields).unwrap());
        assert_eq!(tables[0].0, "041604b0");
        assert_eq!(
            fields.product_name, "Jardim",
            "the default locale is the report's name"
        );
        assert_eq!(tables.len(), 2);
    }

    #[test]
    fn a_tag_with_no_windows_language_is_reported_and_not_written() {
        let locales = [locale("en", "Garden", None), locale("tlh", "Beq", None)];
        let fields = version_fields(&garden("en", &locales), "garden.exe");
        assert_eq!(read_tables(&version_resource(&fields).unwrap()).len(), 1);
        let unmapped = fields.localized.iter().find(|l| l.tag == "tlh").unwrap();
        assert!(!unmapped.embedded);
        assert_eq!(unmapped.language_id, None);
        assert!(
            unmapped
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("no Windows language"))
        );
    }

    #[test]
    fn two_tags_for_one_windows_language_keep_the_first_and_say_so() {
        let locales = [locale("en", "Garden", None), locale("en-US", "Yard", None)];
        let fields = version_fields(&garden("en", &locales), "garden.exe");
        assert_eq!(read_tables(&version_resource(&fields).unwrap()).len(), 1);
        let second = fields.localized.iter().find(|l| l.tag == "en-US").unwrap();
        assert!(!second.embedded);
        assert!(
            second
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("already use"))
        );
    }

    #[test]
    fn an_unmapped_default_locale_is_written_as_us_english_and_says_so() {
        let locales = [locale("tlh", "Beq", Some("Qap"))];
        let fields = version_fields(&garden("tlh", &locales), "garden.exe");
        let tables = read_tables(&version_resource(&fields).unwrap());
        assert_eq!(tables[0].0, "040904b0");
        let entry = &fields.localized[0];
        assert!(entry.embedded);
        assert_eq!(entry.language_id.as_deref(), Some("0409"));
        assert!(entry.reason.is_some());
    }

    /// The real operating system reads every table back from a real
    /// executable: the strings by language, and the default's as the first.
    #[cfg(windows)]
    #[test]
    fn the_operating_system_reads_each_locale_back_from_an_executable() {
        let dir = std::env::temp_dir().join(format!("florui-locales-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("garden.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();

        let locales = [
            locale("en", "Garden", Some("A workspace")),
            locale("pt-BR", "Jardim", Some("Um espaco para ideias")),
        ];
        let fields = version_fields(&garden("en", &locales), "garden.exe");
        let payload = Payload {
            icon: None,
            version: version_resource(&fields).unwrap(),
        };
        apply(&exe, &payload).unwrap();
        let found = read_back(&exe).unwrap();
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(
            found.version_strings.get("ProductName").map(String::as_str),
            Some("Garden")
        );
        assert_eq!(
            found.localized,
            vec![
                (
                    "0409".to_string(),
                    "Garden".to_string(),
                    "A workspace".to_string()
                ),
                (
                    "0416".to_string(),
                    "Jardim".to_string(),
                    "Um espaco para ideias".to_string()
                ),
            ]
        );
    }
}
