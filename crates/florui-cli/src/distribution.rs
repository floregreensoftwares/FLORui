//! What a build carries of `[bundle]`'s distribution metadata, and where it
//! ends up: the copyright goes into the executable's version information, the
//! license file is staged beside it as a notice, and what a Windows
//! executable has no place for is recorded as not represented instead of
//! being dropped silently.

use std::path::Path;

use serde::Serialize;

use florui_config::{BundleConfig, FieldProvenance, Provenance};

/// A license notice is text; anything larger is not one.
const MAX_NOTICE_BYTES: u64 = 1024 * 1024;

/// File names the staging directory already uses for its own files.
const RESERVED_NAMES: [&str; 2] = ["report.json", "icon.ico"];

/// The declared license file, read to be staged under its own name.
pub struct Notice {
    pub file_name: String,
    pub bytes: Vec<u8>,
}

pub fn read_notice(path: &Path) -> Result<Notice, String> {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| format!("bundle.license_file: {} has no file name", path.display()))?;
    if RESERVED_NAMES.contains(&file_name.to_ascii_lowercase().as_str()) {
        return Err(format!(
            "bundle.license_file: {file_name} is a name the staging directory uses for its own files"
        ));
    }
    let metadata = std::fs::metadata(path).map_err(|error| {
        format!(
            "bundle.license_file: could not read {}: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "bundle.license_file: {} is not a file",
            path.display()
        ));
    }
    if metadata.len() > MAX_NOTICE_BYTES {
        return Err(format!(
            "bundle.license_file: {} is {} bytes, over the {MAX_NOTICE_BYTES}-byte limit for a notice",
            path.display(),
            metadata.len()
        ));
    }
    let bytes = std::fs::read(path).map_err(|error| {
        format!(
            "bundle.license_file: could not read {}: {error}",
            path.display()
        )
    })?;
    Ok(Notice { file_name, bytes })
}

/// Where a bundle field's value came from, for the report.
pub fn source_of(provenance: &[FieldProvenance], field: &str) -> &'static str {
    match provenance
        .iter()
        .find(|entry| entry.field == field)
        .map(|entry| &entry.provenance)
    {
        Some(Provenance::ConfigFile(_)) | Some(Provenance::Environment(_)) => "config",
        Some(Provenance::CargoManifest) => "cargo",
        _ => "none",
    }
}

#[derive(Serialize)]
pub struct FieldReport {
    pub field: &'static str,
    pub value: Option<String>,
    /// `config`, `cargo` or `none`.
    pub source: &'static str,
    /// `embedded` (in the executable), `staged` (a file beside it),
    /// `not_represented` or `absent`.
    pub mapping: &'static str,
    pub note: Option<&'static str>,
}

#[derive(Serialize)]
pub struct DistributionReport {
    pub fields: Vec<FieldReport>,
}

/// One line per bundle field: its value, its source and what became of it.
pub fn report(
    bundle: &BundleConfig,
    provenance: &[FieldProvenance],
    notice: Option<&Notice>,
) -> DistributionReport {
    let entry = |field: &'static str,
                 value: Option<String>,
                 mapping: &'static str,
                 note: Option<&'static str>| FieldReport {
        source: if value.is_some() {
            source_of(provenance, field)
        } else {
            "none"
        },
        mapping: if value.is_some() { mapping } else { "absent" },
        note: if value.is_some() { note } else { None },
        value,
        field,
    };
    DistributionReport {
        fields: vec![
            entry(
                "bundle.publisher",
                bundle.publisher.clone(),
                "embedded",
                Some("CompanyName in the version information"),
            ),
            entry(
                "bundle.copyright",
                bundle.copyright.clone(),
                "embedded",
                Some("LegalCopyright in the version information"),
            ),
            entry(
                "bundle.license",
                bundle.license.clone(),
                "not_represented",
                Some(
                    "a Windows executable has no license field; the license file is staged as a \
                     notice when one is declared",
                ),
            ),
            entry(
                "bundle.license_file",
                notice.map(|notice| notice.file_name.clone()),
                "staged",
                Some("copied beside the executable under its own name"),
            ),
            entry(
                "bundle.category",
                bundle.category.clone(),
                "not_represented",
                Some("a Windows executable has no category"),
            ),
            entry(
                "bundle.homepage",
                bundle.homepage.clone(),
                "not_represented",
                Some("a Windows executable has no homepage field"),
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance(field: &'static str, provenance: Provenance) -> FieldProvenance {
        FieldProvenance { field, provenance }
    }

    #[test]
    fn a_notice_is_read_under_its_own_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("LICENSE-MIT");
        std::fs::write(&path, "MIT text").unwrap();

        let notice = read_notice(&path).unwrap();

        assert_eq!(notice.file_name, "LICENSE-MIT");
        assert_eq!(notice.bytes, b"MIT text");
    }

    #[test]
    fn a_missing_oversized_or_reserved_notice_is_an_error_naming_the_field() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("BIG");
        std::fs::write(&big, vec![b'x'; (MAX_NOTICE_BYTES + 1) as usize]).unwrap();
        let reserved = dir.path().join("report.json");
        std::fs::write(&reserved, "{}").unwrap();

        for (path, why) in [
            (dir.path().join("nope"), "could not read"),
            (big, "over the"),
            (reserved, "uses for its own files"),
            (dir.path().to_path_buf(), "not a file"),
        ] {
            let error = read_notice(&path).err().unwrap();
            assert!(
                error.contains("bundle.license_file") && error.contains(why),
                "{error}"
            );
        }
    }

    #[test]
    fn the_report_says_what_became_of_each_field_and_where_it_came_from() {
        let bundle = BundleConfig {
            publisher: Some("Floregreen".into()),
            copyright: Some("Copyright 2026".into()),
            license: Some("MIT".into()),
            license_file: Some("/p/LICENSE".into()),
            category: None,
            homepage: Some("https://example.com".into()),
        };
        let notice = Notice {
            file_name: "LICENSE".into(),
            bytes: Vec::new(),
        };
        let provenance = [
            provenance("bundle.publisher", Provenance::ConfigFile(None)),
            provenance("bundle.copyright", Provenance::ConfigFile(None)),
            provenance("bundle.license", Provenance::CargoManifest),
            provenance("bundle.homepage", Provenance::CargoManifest),
        ];

        let report = report(&bundle, &provenance, Some(&notice));

        let get = |field: &str| report.fields.iter().find(|f| f.field == field).unwrap();
        assert_eq!(get("bundle.copyright").mapping, "embedded");
        assert_eq!(get("bundle.copyright").source, "config");
        assert_eq!(get("bundle.license").mapping, "not_represented");
        assert_eq!(get("bundle.license").source, "cargo");
        assert_eq!(get("bundle.license_file").mapping, "staged");
        assert_eq!(get("bundle.license_file").value.as_deref(), Some("LICENSE"));
        assert_eq!(get("bundle.homepage").mapping, "not_represented");
        assert_eq!(get("bundle.category").mapping, "absent");
        assert_eq!(get("bundle.category").source, "none");
        assert_eq!(get("bundle.category").note, None);
    }
}
