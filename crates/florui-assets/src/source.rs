//! Where an asset's bytes come from, and the identity used to dedupe and
//! cache it. A packaged app never resolves [`AssetSource::Embedded`]
//! against the filesystem at all -- its bytes are compiled in, the same
//! `include_bytes!` idiom `florui-icon`'s own build-time embedding already
//! uses -- so a shipped binary has no dependency on a development
//! checkout's directory layout.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use crate::AssetError;
use crate::limits::check_source_len;

/// An asset's origin. `Path` is resolved relative to whatever base
/// directory the caller already uses for its own asset lookups (this
/// crate has no opinion on that) -- typically a project root in
/// development. `Embedded` carries bytes compiled directly into the
/// binary plus a stable `id` assigned at build time (e.g. the constant
/// name a build script generated), standing in for a filesystem path.
#[derive(Debug, Clone, Copy)]
pub enum AssetSource<'a> {
    Path(&'a Path),
    Embedded {
        id: &'static str,
        bytes: &'static [u8],
    },
}

/// A resolved asset's dedup/cache identity. Two [`AssetSource`] values
/// that refer to the same underlying file (even via different relative
/// paths, symlinks, or `.`/`..` segments) resolve to the same `Path`
/// variant, since resolution canonicalizes the path first.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AssetId {
    Path(PathBuf),
    Embedded(&'static str),
}

impl<'a> AssetSource<'a> {
    /// Resolves this source's dedup identity. For a `Path` source this
    /// requires the file to exist (canonicalization needs to read the
    /// filesystem), so a missing file is reported here rather than only
    /// at read time.
    pub fn identity(&self) -> Result<AssetId, AssetError> {
        match self {
            AssetSource::Path(path) => {
                let canonical = std::fs::canonicalize(path).map_err(|source| AssetError::Io {
                    path: (*path).to_owned(),
                    source,
                })?;
                Ok(AssetId::Path(canonical))
            }
            AssetSource::Embedded { id, .. } => Ok(AssetId::Embedded(id)),
        }
    }

    /// Reads this source's raw bytes, rejecting anything over
    /// [`crate::limits::MAX_SOURCE_BYTES`] before it is copied into
    /// memory. Borrows rather than copies for an already-in-memory
    /// embedded source.
    pub fn read_bytes(&self) -> Result<Cow<'a, [u8]>, AssetError> {
        match self {
            AssetSource::Path(path) => {
                let metadata = std::fs::metadata(path).map_err(|source| AssetError::Io {
                    path: (*path).to_owned(),
                    source,
                })?;
                check_source_len(metadata.len(), Some(path))?;
                let bytes = std::fs::read(path).map_err(|source| AssetError::Io {
                    path: (*path).to_owned(),
                    source,
                })?;
                Ok(Cow::Owned(bytes))
            }
            AssetSource::Embedded { bytes, .. } => {
                check_source_len(bytes.len() as u64, None)?;
                Ok(Cow::Borrowed(bytes))
            }
        }
    }

    /// Reads this source's bytes as UTF-8 text (SVG's own encoding).
    pub fn read_text(&self) -> Result<String, AssetError> {
        let bytes = self.read_bytes()?;
        String::from_utf8(bytes.into_owned()).map_err(|_| AssetError::InvalidUtf8 {
            path: self.path().map(Path::to_owned),
        })
    }

    fn path(&self) -> Option<&Path> {
        match self {
            AssetSource::Path(path) => Some(path),
            AssetSource::Embedded { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_of_a_path_source_canonicalizes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.svg");
        std::fs::write(&path, "<svg/>").unwrap();
        let id = AssetSource::Path(&path).identity().unwrap();
        assert_eq!(id, AssetId::Path(std::fs::canonicalize(&path).unwrap()));
    }

    #[test]
    fn identity_of_two_relative_paths_to_the_same_file_matches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("icon.svg");
        std::fs::write(&path, "<svg/>").unwrap();
        let via_dot = dir.path().join(".").join("icon.svg");
        assert_eq!(
            AssetSource::Path(&path).identity().unwrap(),
            AssetSource::Path(&via_dot).identity().unwrap()
        );
    }

    #[test]
    fn identity_of_a_missing_path_is_an_io_error() {
        let err = AssetSource::Path(Path::new("does/not/exist.svg"))
            .identity()
            .unwrap_err();
        assert!(matches!(err, AssetError::Io { .. }));
    }

    #[test]
    fn identity_of_an_embedded_source_is_its_id() {
        let id = AssetSource::Embedded {
            id: "some_icon",
            bytes: b"<svg/>",
        }
        .identity()
        .unwrap();
        assert_eq!(id, AssetId::Embedded("some_icon"));
    }

    #[test]
    fn read_bytes_of_a_path_source_returns_its_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        std::fs::write(&path, [1u8, 2, 3, 4]).unwrap();
        let bytes = AssetSource::Path(&path).read_bytes().unwrap();
        assert_eq!(&*bytes, &[1, 2, 3, 4]);
    }

    #[test]
    fn read_bytes_of_an_embedded_source_borrows_without_copying() {
        static DATA: &[u8] = &[9, 9, 9];
        let source = AssetSource::Embedded {
            id: "x",
            bytes: DATA,
        };
        match source.read_bytes().unwrap() {
            Cow::Borrowed(bytes) => assert_eq!(bytes.as_ptr(), DATA.as_ptr()),
            Cow::Owned(_) => panic!("embedded bytes should be borrowed, not copied"),
        }
    }

    #[test]
    fn read_bytes_rejects_a_path_source_over_the_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.bin");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(crate::limits::MAX_SOURCE_BYTES + 1).unwrap();
        let err = AssetSource::Path(&path).read_bytes().unwrap_err();
        assert!(matches!(err, AssetError::SourceTooLarge { .. }));
    }

    #[test]
    fn read_bytes_rejects_an_embedded_source_over_the_size_limit() {
        // Built, not literal, so the binary doesn't actually carry a
        // 64MiB+1 static array just to exercise this check.
        let data: Vec<u8> = vec![0u8; (crate::limits::MAX_SOURCE_BYTES + 1) as usize];
        let leaked: &'static [u8] = data.leak();
        let source = AssetSource::Embedded {
            id: "huge",
            bytes: leaked,
        };
        let err = source.read_bytes().unwrap_err();
        assert!(matches!(err, AssetError::SourceTooLarge { .. }));
    }

    #[test]
    fn read_text_rejects_invalid_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.svg");
        std::fs::write(&path, [0xff, 0xfe, 0xfd]).unwrap();
        let err = AssetSource::Path(&path).read_text().unwrap_err();
        assert!(matches!(err, AssetError::InvalidUtf8 { .. }));
    }

    #[test]
    fn read_text_of_valid_utf8_returns_the_string() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.svg");
        std::fs::write(&path, "<svg/>").unwrap();
        assert_eq!(AssetSource::Path(&path).read_text().unwrap(), "<svg/>");
    }
}
