//! Private diagnostic output of a native build: the debug symbols that make a
//! crash in the shipped executable readable, kept apart from what is shipped.
//!
//! The deployable directory (`target/florui-build/<package>/native`) holds the
//! executables and nothing that explains them. Their symbols (a `.pdb` on
//! Windows) are copied to a sibling directory, `diagnostics`, bound to the
//! executable they belong to by the identity the linker wrote into both (a
//! GUID and an age) and by the final executable's SHA-256, and never
//! referenced from the deployable files. Nothing here uploads anything.
//!
//! "Bound" is checked, not assumed: the retained symbols are loaded with the
//! system's own debug-help library against the executable alone, with the
//! retained directory as the only place to look, and the build only records
//! them when they match the executable and resolve a known function. A build
//! made where no symbols are produced says so instead of claiming any.

use std::path::Path;

use serde::Serialize;

/// What the symbol loader found for one executable.
#[derive(Debug, Clone, PartialEq)]
pub struct SymbolInfo {
    /// A `.pdb` was loaded for the executable (not only its export table).
    pub matched: bool,
    /// The identity both files carry, as a GUID.
    pub guid: String,
    pub age: u32,
    /// The file name (no directory) of the symbols that were loaded.
    pub loaded_file: String,
    /// `main` resolved to an address through those symbols.
    pub resolves_main: bool,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct RetainedFile {
    pub file: String,
    pub kind: &'static str,
    pub bytes: u64,
    pub sha256: String,
    /// The executable these symbols explain, as staged.
    pub executable: String,
    pub executable_sha256: String,
    pub guid: String,
    pub age: u32,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct DiagnosticsReport {
    /// `retained`, `none` (the build produced no symbols to keep) or
    /// `unsupported_host`.
    pub status: &'static str,
    /// Where the files are, relative to the deployable directory; they are
    /// outside it on purpose.
    pub directory: &'static str,
    pub files: Vec<RetainedFile>,
    pub warnings: Vec<String>,
    pub notes: Vec<&'static str>,
}

pub const DIRECTORY: &str = "../diagnostics";

#[cfg(not(windows))]
pub fn unsupported_host() -> DiagnosticsReport {
    DiagnosticsReport {
        status: "unsupported_host",
        directory: DIRECTORY,
        files: Vec::new(),
        warnings: Vec::new(),
        notes: vec!["symbols are only retained for Windows builds so far"],
    }
}

#[cfg(windows)]
pub fn inspect(executable: &Path, search: &Path) -> Result<SymbolInfo, String> {
    use std::mem::{size_of, zeroed};

    use windows_sys::Win32::Foundation::{FALSE, TRUE};
    use windows_sys::Win32::System::Diagnostics::Debug::{
        IMAGEHLP_MODULEW64, SYMBOL_INFOW, SYMOPT_DEFERRED_LOADS, SYMOPT_FAIL_CRITICAL_ERRORS,
        SYMOPT_NO_PROMPTS, SYMOPT_UNDNAME, SymCleanup, SymFromNameW, SymGetModuleInfoW64,
        SymInitializeW, SymLoadModuleExW, SymSetOptions,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    use std::os::windows::ffi::OsStrExt;

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }
    fn wide_str(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }
    // The module's own `SymPdb` kind, which is what a loaded `.pdb` reports.
    const SYM_PDB: i32 = 3;

    // SAFETY: the debug-help library is single-threaded per process and this
    // is the only user of it here; every buffer passed outlives its call, the
    // structures are sized as the API requires, and the session is closed
    // before returning.
    unsafe {
        let process = GetCurrentProcess();
        SymSetOptions(
            SYMOPT_UNDNAME
                | SYMOPT_DEFERRED_LOADS
                | SYMOPT_FAIL_CRITICAL_ERRORS
                | SYMOPT_NO_PROMPTS,
        );
        if SymInitializeW(process, wide(search).as_ptr(), FALSE) != TRUE {
            return Err("could not start the symbol loader".to_string());
        }
        let base = SymLoadModuleExW(
            process,
            std::ptr::null_mut(),
            wide(executable).as_ptr(),
            std::ptr::null(),
            0x1000_0000,
            0,
            std::ptr::null_mut(),
            0,
        );
        if base == 0 {
            SymCleanup(process);
            return Err(format!("could not read {}", executable.display()));
        }
        // Resolving a name is what makes the loader read the symbols, so the
        // module is described after it.
        let mut storage = vec![0u8; size_of::<SYMBOL_INFOW>() + 512 * 2];
        let symbol = storage.as_mut_ptr().cast::<SYMBOL_INFOW>();
        (*symbol).SizeOfStruct = size_of::<SYMBOL_INFOW>() as u32;
        (*symbol).MaxNameLen = 512;
        let resolves_main = SymFromNameW(process, wide_str("main").as_ptr(), symbol) == TRUE;
        let mut module: IMAGEHLP_MODULEW64 = zeroed();
        module.SizeOfStruct = size_of::<IMAGEHLP_MODULEW64>() as u32;
        let described = SymGetModuleInfoW64(process, base, &mut module) == TRUE;
        SymCleanup(process);
        if !described {
            return Err("the symbol loader could not describe the module".to_string());
        }
        let g = module.PdbSig70;
        let loaded: Vec<u16> = module
            .LoadedPdbName
            .iter()
            .copied()
            .take_while(|unit| *unit != 0)
            .collect();
        let loaded = String::from_utf16_lossy(&loaded);
        Ok(SymbolInfo {
            matched: module.SymType == SYM_PDB,
            guid: format!(
                "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                g.data1,
                g.data2,
                g.data3,
                g.data4[0],
                g.data4[1],
                g.data4[2],
                g.data4[3],
                g.data4[4],
                g.data4[5],
                g.data4[6],
                g.data4[7]
            ),
            age: module.PdbAge,
            loaded_file: Path::new(&loaded.replace('/', "\\"))
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            resolves_main,
        })
    }
}

#[cfg(not(windows))]
pub fn inspect(_executable: &Path, _search: &Path) -> Result<SymbolInfo, String> {
    Err("symbols are only inspected on Windows hosts".to_string())
}

/// Keeps the symbols of each staged executable in `directory`, outside the
/// deployable one, and records what they are bound to. `staged_hashes` is the
/// final SHA-256 of each staged executable by file name. A symbol file that
/// does not belong to its executable, or does not resolve a known function when
/// it alone is searched, is not kept, and the build says so.
#[cfg(windows)]
pub fn retain(
    executables: &[std::path::PathBuf],
    staging: &Path,
    directory: &Path,
    staged_hashes: &std::collections::BTreeMap<String, String>,
) -> DiagnosticsReport {
    let mut report = DiagnosticsReport {
        status: "none",
        directory: DIRECTORY,
        files: Vec::new(),
        warnings: Vec::new(),
        notes: vec![
            "private: these files explain the executables and are not part of what is shipped; nothing uploads them",
        ],
    };
    for built in executables {
        let Some(name) = built.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let staged = staging.join(&name);
        let pdb = built.with_extension("pdb");
        let Some(pdb_name) = pdb.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if !pdb.is_file() {
            report.warnings.push(format!(
                "no symbols were produced for {name}, so a crash in it cannot be symbolized"
            ));
            continue;
        }
        let Some(search) = built.parent() else {
            continue;
        };
        let linked = match inspect(&staged, search) {
            Ok(info) if info.matched && info.loaded_file.eq_ignore_ascii_case(&pdb_name) => info,
            Ok(_) => {
                report.warnings.push(format!(
                    "{pdb_name} is not the symbol file {name} was linked with, so it was not kept"
                ));
                continue;
            }
            Err(error) => {
                report
                    .warnings
                    .push(format!("could not read the symbols of {name}: {error}"));
                continue;
            }
        };
        if let Err(error) = std::fs::create_dir_all(directory)
            .and_then(|()| std::fs::copy(&pdb, directory.join(&pdb_name)).map(|_| ()))
        {
            report
                .warnings
                .push(format!("could not keep {pdb_name}: {error}"));
            continue;
        }
        // What is kept, searched alone, must be what explains the executable.
        let kept = directory.join(&pdb_name);
        match inspect(&staged, directory) {
            Ok(info)
                if info.matched
                    && info.guid == linked.guid
                    && info.age == linked.age
                    && info.resolves_main => {}
            _ => {
                let _ = std::fs::remove_file(&kept);
                report.warnings.push(format!(
                    "the copy of {pdb_name} does not resolve {name} by itself, so it was not kept"
                ));
                continue;
            }
        }
        let Ok(hashes) = crate::build::hash_file(&kept) else {
            report.warnings.push(format!("could not hash {pdb_name}"));
            continue;
        };
        report.files.push(RetainedFile {
            file: pdb_name,
            kind: "pdb",
            bytes: hashes.bytes,
            sha256: hashes.sha256,
            executable: name.clone(),
            executable_sha256: staged_hashes.get(&name).cloned().unwrap_or_default(),
            guid: linked.guid,
            age: linked.age,
        });
    }
    if !report.files.is_empty() {
        report.status = "retained";
    }
    report
}

#[cfg(not(windows))]
pub fn retain(
    _executables: &[std::path::PathBuf],
    _staging: &Path,
    _directory: &Path,
    _staged_hashes: &std::collections::BTreeMap<String, String>,
) -> DiagnosticsReport {
    unsupported_host()
}

/// Whether `name` is a file that explains an executable and so must never be
/// in a deployable directory.
pub fn is_private_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".pdb", ".map", ".dwp", ".dsym", ".dbg", ".debug"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_and_map_files_are_private_and_executables_are_not() {
        for name in [
            "garden.pdb",
            "GARDEN.PDB",
            "app.js.map",
            "garden.dwp",
            "x.debug",
        ] {
            assert!(is_private_file(name), "{name}");
        }
        for name in [
            "garden.exe",
            "report.json",
            "icon.ico",
            "LICENSE-MIT",
            "pdb.txt",
        ] {
            assert!(!is_private_file(name), "{name}");
        }
    }
}
