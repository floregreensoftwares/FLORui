//! Registering an application with a Linux desktop, and taking the
//! registration away again.
//!
//! Like the Windows registration, this only ever happens in `florui register`
//! and nothing here sets a default handler: registering makes the application
//! a candidate ("Open with", the Default apps list), and the user chooses.
//! Everything is written per user, under `$XDG_DATA_HOME`:
//!
//! - the desktop entry `applications/<identifier>.desktop`, which names the
//!   staged executable, the URL schemes (`x-scheme-handler/<scheme>`) and the
//!   MIME types of the file types it handles;
//! - for a file type no installed MIME type claims, `mime/packages/
//!   <identifier>.xml` defines one. An extension another type already claims
//!   keeps that type, and the application only becomes a candidate for it;
//! - the icon, as `icons/hicolor/<size>x<size>/apps/<identifier>.png`;
//! - a record of every file written, `florui/registrations/<identifier>.json`,
//!   so removing it removes exactly those files, and only while they are still
//!   what was written.
//!
//! A desktop entry or MIME definition that exists without this identifier's
//! owner marker belongs to someone else and is a conflict, reported before
//! anything is written.

use std::fs;
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::declaration::{Conflict, Declaration, is_reserved_scheme, safe_segment};

/// The key in a desktop entry that names the Florui application that wrote it.
pub const OWNER_KEY: &str = "X-Florui-Owner";
const OWNER_COMMENT: &str = "florui-owner:";
const RECORD_VERSION: u32 = 1;

/// The per-user data directory and what lives under it.
#[derive(Debug, Clone)]
pub struct Dirs {
    data_home: PathBuf,
    /// Where installed MIME types are looked up, besides `data_home`.
    system_dirs: Vec<PathBuf>,
}

impl Dirs {
    pub fn new(data_home: PathBuf, system_dirs: Vec<PathBuf>) -> Self {
        Self {
            data_home,
            system_dirs,
        }
    }

    #[cfg(target_os = "linux")]
    pub fn from_env() -> Result<Self, String> {
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
            })
            .ok_or_else(|| "neither XDG_DATA_HOME nor HOME is set".to_string())?;
        let system = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
        Ok(Self::new(
            data_home,
            system.split(':').map(PathBuf::from).collect(),
        ))
    }

    fn entry(&self, identifier: &str) -> PathBuf {
        self.data_home
            .join("applications")
            .join(florui_platform::desktop_entry_file(identifier))
    }

    fn mime_package(&self, identifier: &str) -> PathBuf {
        self.data_home
            .join("mime/packages")
            .join(format!("{identifier}.xml"))
    }

    fn icon(&self, identifier: &str, size: u32) -> PathBuf {
        self.data_home
            .join(format!("icons/hicolor/{size}x{size}/apps"))
            .join(format!("{identifier}.png"))
    }

    fn record(&self, identifier: &str) -> PathBuf {
        self.data_home
            .join("florui/registrations")
            .join(format!("{identifier}.json"))
    }

    /// The MIME type an installed database maps `*.<extension>` to.
    fn known_type(&self, extension: &str) -> Option<String> {
        std::iter::once(&self.data_home)
            .chain(&self.system_dirs)
            .filter_map(|dir| fs::read_to_string(dir.join("mime/globs2")).ok())
            .find_map(|globs| type_of_glob(&globs, extension))
    }
}

/// The type `globs2` (lines of `weight:type:glob[:flags]`) gives `*.<ext>`.
fn type_of_glob(globs: &str, extension: &str) -> Option<String> {
    let wanted = format!("*.{}", extension.to_ascii_lowercase());
    globs.lines().find_map(|line| {
        let mut parts = line.split(':');
        let _weight = parts.next()?;
        let mime = parts.next()?;
        let glob = parts.next()?;
        (glob.to_ascii_lowercase() == wanted).then(|| mime.to_owned())
    })
}

/// One staged icon, as the build wrote it.
#[derive(Debug, Clone, PartialEq)]
pub struct StagedIcon {
    pub size: u32,
    pub png: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WrittenFile {
    pub path: String,
    pub sha256: String,
}

/// Everything a registration wrote, kept so it can be taken away exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub version: u32,
    pub identifier: String,
    pub exe: String,
    pub files: Vec<WrittenFile>,
    pub schemes: Vec<String>,
    pub extensions: Vec<String>,
    pub mime_types: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub declaration: Declaration,
    pub exe: PathBuf,
    pub writes: Vec<(PathBuf, Vec<u8>)>,
    pub mime_types: Vec<String>,
    pub conflicts: Vec<Conflict>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A value of a desktop entry, which has no line breaks and doubles its
/// backslashes.
fn entry_value(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .replace('\\', "\\\\")
}

/// `exe` as the one quoted program argument of an `Exec` line: quoted, with
/// the characters the quoting rule escapes, then the file-level backslash
/// escape, and `%` doubled so it is not a field code.
fn exec_program(exe: &Path) -> Result<String, String> {
    let text = exe
        .to_str()
        .ok_or_else(|| "the executable's path is not valid text".to_string())?;
    if text.chars().any(char::is_control) {
        return Err("the executable's path contains a control character".to_string());
    }
    let mut quoted = String::from("\"");
    for c in text.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(c),
        }
    }
    quoted.push('"');
    Ok(quoted.replace('\\', "\\\\"))
}

fn xml_escape(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .map(|c| match c {
            '&' => "&amp;".to_owned(),
            '<' => "&lt;".to_owned(),
            '>' => "&gt;".to_owned(),
            '"' => "&quot;".to_owned(),
            '\'' => "&apos;".to_owned(),
            other => other.to_string(),
        })
        .collect()
}

/// The MIME type a file type is registered under, and whether this
/// registration has to define it.
fn mime_for(
    identifier: &str,
    association: &crate::declaration::Association,
    dirs: &Dirs,
) -> (String, bool) {
    if let Some(declared) = &association.mime_type {
        return (declared.clone(), true);
    }
    match dirs.known_type(&association.extension) {
        Some(known) => (known, false),
        None => {
            let identity = association
                .identity
                .strip_prefix(&format!("{identifier}."))
                .unwrap_or(&association.identity)
                .to_ascii_lowercase();
            (format!("application/x-{identifier}-{identity}"), true)
        }
    }
}

fn desktop_entry(
    declaration: &Declaration,
    exec: &str,
    mime_types: &[String],
    icon: bool,
) -> String {
    let field_code = match (
        declaration.schemes.is_empty(),
        declaration.associations.is_empty(),
    ) {
        (false, false) => "%U",
        (false, true) => "%u",
        _ => "%F",
    };
    let mut entry = String::from("[Desktop Entry]\nType=Application\nVersion=1.0\n");
    entry.push_str(&format!(
        "Name={}\n",
        entry_value(&declaration.display_name)
    ));
    if let Some(description) = &declaration.description {
        entry.push_str(&format!("Comment={}\n", entry_value(description)));
    }
    entry.push_str(&format!("Exec={exec} {field_code}\n"));
    if icon {
        entry.push_str(&format!("Icon={}\n", declaration.identifier));
    }
    entry.push_str("Terminal=false\n");
    entry.push_str(&format!("StartupWMClass={}\n", declaration.identifier));
    let mut types: Vec<String> = declaration
        .schemes
        .iter()
        .map(|scheme| format!("x-scheme-handler/{}", scheme.to_ascii_lowercase()))
        .collect();
    types.extend(mime_types.iter().cloned());
    if !types.is_empty() {
        entry.push_str(&format!("MimeType={};\n", types.join(";")));
    }
    entry.push_str(&format!("{OWNER_KEY}={}\n", declaration.identifier));
    entry
}

fn mime_package(identifier: &str, definitions: &[(String, String, String)]) -> String {
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- {OWNER_COMMENT} {identifier} -->\n<mime-info xmlns=\"http://www.freedesktop.org/standards/shared-mime-info\">\n"
    );
    for (mime, comment, extension) in definitions {
        xml.push_str(&format!("  <mime-type type=\"{}\">\n", xml_escape(mime)));
        xml.push_str(&format!("    <comment>{}</comment>\n", xml_escape(comment)));
        xml.push_str(&format!(
            "    <glob pattern=\"*.{}\"/>\n  </mime-type>\n",
            xml_escape(extension)
        ));
    }
    xml.push_str("</mime-info>\n");
    xml
}

/// Why `path` is not this application's own, when it exists and `is_ours`
/// does not recognize its content.
fn foreign(path: &Path, is_ours: impl Fn(&str) -> bool) -> Option<String> {
    let content = fs::read_to_string(path).ok()?;
    (!is_ours(&content)).then(|| {
        format!(
            "{} already exists and was not written by this application",
            path.display()
        )
    })
}

/// What registering `declaration` with `exe` would write, and what stands in
/// its way. Reads the file system and writes nothing.
pub fn plan(declaration: &Declaration, exe: &Path, icons: &[StagedIcon], dirs: &Dirs) -> Plan {
    let identifier = &declaration.identifier;
    let mut conflicts = Vec::new();
    let mut writes: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut mime_types = Vec::new();

    let exec = match exec_program(exe) {
        Ok(exec) => exec,
        Err(reason) => {
            conflicts.push(Conflict {
                what: "the executable".to_string(),
                reason,
            });
            String::new()
        }
    };
    if !safe_segment(identifier) {
        conflicts.push(Conflict {
            what: format!("the identifier \"{identifier}\""),
            reason: "it must be letters, digits and `-_.+` to be part of a file name".to_string(),
        });
    }
    for scheme in &declaration.schemes {
        if is_reserved_scheme(scheme) {
            conflicts.push(Conflict {
                what: format!("the URL scheme \"{scheme}\""),
                reason: "it is a reserved scheme that the system or the web relies on".to_string(),
            });
        } else if !safe_segment(&scheme.to_ascii_lowercase()) {
            conflicts.push(Conflict {
                what: format!("the URL scheme \"{scheme}\""),
                reason: "it cannot be part of a MIME type".to_string(),
            });
        }
    }
    let mut definitions = Vec::new();
    for association in &declaration.associations {
        if !safe_segment(&association.extension) || !safe_segment(&association.identity) {
            conflicts.push(Conflict {
                what: format!(
                    "the file type \".{}\" (identity \"{}\")",
                    association.extension, association.identity
                ),
                reason: "the extension and the identity must be letters, digits and `-_.+`"
                    .to_string(),
            });
            continue;
        }
        let (mime, define) = mime_for(identifier, association, dirs);
        if define {
            let comment = association
                .description
                .clone()
                .unwrap_or_else(|| format!("{} file", association.extension));
            definitions.push((mime.clone(), comment, association.extension.clone()));
        }
        mime_types.push(mime);
    }
    mime_types.dedup();

    let entry_marker = format!("{OWNER_KEY}={identifier}");
    if let Some(reason) = foreign(&dirs.entry(identifier), |text| {
        text.lines().any(|line| line.trim() == entry_marker)
    }) {
        conflicts.push(Conflict {
            what: "the desktop entry".to_string(),
            reason,
        });
    }
    let mime_marker = format!("{OWNER_COMMENT} {identifier} -->");
    if !definitions.is_empty()
        && let Some(reason) = foreign(&dirs.mime_package(identifier), |text| {
            text.contains(&mime_marker)
        })
    {
        conflicts.push(Conflict {
            what: "the MIME definition".to_string(),
            reason,
        });
    }
    let previous = read_record(dirs, identifier);
    for icon in icons {
        let path = dirs.icon(identifier, icon.size);
        let ours = previous
            .as_ref()
            .is_some_and(|record| record.files.iter().any(|f| Path::new(&f.path) == path));
        if path.exists() && !ours {
            conflicts.push(Conflict {
                what: format!("the {0}x{0} icon", icon.size),
                reason: format!(
                    "{} already exists and was not written by this application",
                    path.display()
                ),
            });
        }
        writes.push((path, icon.png.clone()));
    }
    writes.push((
        dirs.entry(identifier),
        desktop_entry(declaration, &exec, &mime_types, !icons.is_empty()).into_bytes(),
    ));
    if !definitions.is_empty() {
        writes.push((
            dirs.mime_package(identifier),
            mime_package(identifier, &definitions).into_bytes(),
        ));
    }
    Plan {
        declaration: declaration.clone(),
        exe: exe.to_path_buf(),
        writes,
        mime_types,
        conflicts,
    }
}

/// Runs the programs that refresh the desktop's caches. A missing program is
/// not an error: the entries are found without the cache, only later.
pub trait Refresh {
    fn run(&self, program: &str, args: &[&Path]) -> Result<(), String>;
}

#[cfg(target_os = "linux")]
pub struct Programs;

#[cfg(target_os = "linux")]
impl Refresh for Programs {
    fn run(&self, program: &str, args: &[&Path]) -> Result<(), String> {
        match Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(format!("{program} exited with {status}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(format!("{program} is not installed"))
            }
            Err(error) => Err(format!("{program}: {error}")),
        }
    }
}

fn refresh(dirs: &Dirs, refresher: &dyn Refresh) -> Vec<String> {
    let applications = dirs.data_home.join("applications");
    let mime = dirs.data_home.join("mime");
    let icons = dirs.data_home.join("icons/hicolor");
    let mut notes = Vec::new();
    let mut attempt = |program: &str, args: &[&Path]| {
        if let Err(reason) = refresher.run(program, args) {
            notes.push(reason);
        }
    };
    attempt("update-desktop-database", &[applications.as_path()]);
    if mime.is_dir() {
        attempt("update-mime-database", &[mime.as_path()]);
    }
    if icons.is_dir() {
        attempt(
            "gtk-update-icon-cache",
            &[Path::new("-f"), Path::new("-t"), icons.as_path()],
        );
    }
    notes
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    let temporary = path.with_extension("florui-tmp");
    fs::write(&temporary, bytes)
        .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}

/// What applying a plan did.
pub struct Applied {
    pub record: Record,
    /// Cache refreshes that did not happen; the registration is still in place.
    pub notes: Vec<String>,
}

/// Writes the plan, then its record. Stops at the first file it cannot write.
pub fn apply(plan: &Plan, dirs: &Dirs, refresher: &dyn Refresh) -> Result<Applied, String> {
    let mut files = Vec::new();
    for (path, bytes) in &plan.writes {
        write_atomically(path, bytes)?;
        files.push(WrittenFile {
            path: path.display().to_string(),
            sha256: sha256_hex(bytes),
        });
    }
    let record = Record {
        version: RECORD_VERSION,
        identifier: plan.declaration.identifier.clone(),
        exe: plan.exe.display().to_string(),
        files,
        schemes: plan.declaration.schemes.clone(),
        extensions: plan
            .declaration
            .associations
            .iter()
            .map(|association| association.extension.clone())
            .collect(),
        mime_types: plan.mime_types.clone(),
    };
    let json = serde_json::to_vec_pretty(&record)
        .map_err(|error| format!("could not serialize the record: {error}"))?;
    write_atomically(&dirs.record(&plan.declaration.identifier), &json)?;
    let notes = refresh(dirs, refresher);
    Ok(Applied { record, notes })
}

pub fn read_record(dirs: &Dirs, identifier: &str) -> Option<Record> {
    let bytes = fs::read(dirs.record(identifier)).ok()?;
    let record: Record = serde_json::from_slice(&bytes).ok()?;
    (record.version == RECORD_VERSION && record.identifier == identifier).then_some(record)
}

/// Whether a file a registration wrote is still as it was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Intact,
    Changed,
    Missing,
}

pub fn file_state(file: &WrittenFile) -> FileState {
    match fs::read(&file.path) {
        Err(_) => FileState::Missing,
        Ok(bytes) if sha256_hex(&bytes) == file.sha256 => FileState::Intact,
        Ok(_) => FileState::Changed,
    }
}

/// What removing a registration did.
#[derive(Debug, Default, PartialEq)]
pub struct Removal {
    pub removed: Vec<String>,
    /// Files that changed since they were written, which are not ours to remove.
    pub left: Vec<String>,
    pub notes: Vec<String>,
}

pub fn remove(record: &Record, dirs: &Dirs, refresher: &dyn Refresh) -> Removal {
    let mut removal = Removal::default();
    for file in &record.files {
        match file_state(file) {
            FileState::Missing => {} // Already gone.
            FileState::Intact => match fs::remove_file(&file.path) {
                Ok(()) => removal.removed.push(file.path.clone()),
                Err(_) => removal.left.push(file.path.clone()),
            },
            FileState::Changed => removal.left.push(file.path.clone()),
        }
    }
    let _ = fs::remove_file(dirs.record(&record.identifier));
    removal.notes = refresh(dirs, refresher);
    removal
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::declaration::Association;

    struct Recorder(RefCell<Vec<String>>);

    impl Refresh for Recorder {
        fn run(&self, program: &str, _args: &[&Path]) -> Result<(), String> {
            self.0.borrow_mut().push(program.to_owned());
            Ok(())
        }
    }

    fn declaration() -> Declaration {
        Declaration {
            identifier: "com.example.garden".to_string(),
            display_name: "Garden".to_string(),
            description: Some("A garden planner".to_string()),
            schemes: vec!["garden".to_string()],
            associations: vec![Association {
                extension: "garden".to_string(),
                identity: "document".to_string(),
                description: Some("A garden file".to_string()),
                mime_type: None,
            }],
        }
    }

    fn dirs(root: &Path) -> Dirs {
        Dirs::new(root.join("data"), vec![root.join("system")])
    }

    fn written(plan: &Plan, name: &str) -> String {
        let (_, bytes) = plan
            .writes
            .iter()
            .find(|(path, _)| path.to_string_lossy().ends_with(name))
            .expect("planned file");
        String::from_utf8(bytes.clone()).expect("text")
    }

    #[test]
    fn the_entry_lists_the_scheme_and_the_file_type_and_names_its_owner() {
        let tmp = tempfile::tempdir().expect("tmp");
        let plan = plan(
            &declaration(),
            Path::new("/opt/garden/garden"),
            &[],
            &dirs(tmp.path()),
        );
        assert!(plan.conflicts.is_empty(), "{:?}", plan.conflicts);
        let entry = written(&plan, "com.example.garden.desktop");
        assert!(
            entry.contains("Exec=\"/opt/garden/garden\" %U\n"),
            "{entry}"
        );
        assert!(
            entry.contains(
                "MimeType=x-scheme-handler/garden;application/x-com.example.garden-document;\n"
            ),
            "{entry}"
        );
        assert!(entry.contains("X-Florui-Owner=com.example.garden\n"));
        assert!(entry.contains("StartupWMClass=com.example.garden\n"));
        assert!(!entry.contains("Icon="), "no icon was staged");
    }

    #[test]
    fn a_type_the_system_already_knows_is_not_redefined() {
        let tmp = tempfile::tempdir().expect("tmp");
        let system = tmp.path().join("system/mime");
        fs::create_dir_all(&system).expect("dir");
        fs::write(system.join("globs2"), "50:text/x-garden-known:*.garden\n").expect("globs");
        let plan = plan(&declaration(), Path::new("/opt/g"), &[], &dirs(tmp.path()));
        assert_eq!(plan.mime_types, vec!["text/x-garden-known".to_string()]);
        assert!(
            !plan
                .writes
                .iter()
                .any(|(path, _)| path.to_string_lossy().ends_with(".xml")),
            "nothing to define"
        );
    }

    #[test]
    fn an_unknown_type_is_defined_with_a_glob_and_the_owner_comment() {
        let tmp = tempfile::tempdir().expect("tmp");
        let plan = plan(&declaration(), Path::new("/opt/g"), &[], &dirs(tmp.path()));
        let xml = written(&plan, "com.example.garden.xml");
        assert!(xml.contains("<!-- florui-owner: com.example.garden -->"));
        assert!(xml.contains("<mime-type type=\"application/x-com.example.garden-document\">"));
        assert!(xml.contains("<glob pattern=\"*.garden\"/>"));
    }

    #[test]
    fn a_declared_mime_type_is_used_and_defined() {
        let tmp = tempfile::tempdir().expect("tmp");
        let mut declared = declaration();
        declared.associations[0].mime_type = Some("application/x-garden".to_string());
        let plan = plan(&declared, Path::new("/opt/g"), &[], &dirs(tmp.path()));
        assert_eq!(plan.mime_types, vec!["application/x-garden".to_string()]);
        assert!(written(&plan, ".xml").contains("type=\"application/x-garden\""));
    }

    #[test]
    fn reserved_schemes_and_foreign_entries_are_conflicts() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dirs = dirs(tmp.path());
        let mut reserved = declaration();
        reserved.schemes = vec!["https".to_string()];
        let found = plan(&reserved, Path::new("/opt/g"), &[], &dirs).conflicts;
        assert!(found.iter().any(|c| c.what.contains("https")), "{found:?}");

        let entry = dirs.entry("com.example.garden");
        fs::create_dir_all(entry.parent().expect("parent")).expect("dir");
        fs::write(&entry, "[Desktop Entry]\nName=Someone else\n").expect("write");
        let found = plan(&declaration(), Path::new("/opt/g"), &[], &dirs).conflicts;
        assert!(
            found.iter().any(|c| c.what == "the desktop entry"),
            "{found:?}"
        );
    }

    #[test]
    fn the_exec_line_quotes_a_path_the_way_the_specification_asks() {
        assert_eq!(
            exec_program(Path::new("/opt/My App/g")).as_deref(),
            Ok("\"/opt/My App/g\"")
        );
        assert_eq!(
            exec_program(Path::new("/opt/a$b`c\"d\\e%f")).as_deref(),
            Ok("\"/opt/a\\\\$b\\\\`c\\\\\"d\\\\\\\\e%%f\"")
        );
        assert!(exec_program(Path::new("/opt/a\nb")).is_err());
    }

    #[test]
    fn registering_writes_everything_and_removing_takes_exactly_that_away() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dirs = dirs(tmp.path());
        let icons = [StagedIcon {
            size: 48,
            png: vec![1, 2, 3],
        }];
        let recorder = Recorder(RefCell::new(Vec::new()));
        let plan = plan(&declaration(), Path::new("/opt/g"), &icons, &dirs);
        let applied = apply(&plan, &dirs, &recorder).expect("applied");
        assert_eq!(applied.record.files.len(), 3);
        assert!(dirs.entry("com.example.garden").is_file());
        assert!(dirs.icon("com.example.garden", 48).is_file());
        assert_eq!(
            read_record(&dirs, "com.example.garden"),
            Some(applied.record.clone())
        );
        assert_eq!(
            *recorder.0.borrow(),
            [
                "update-desktop-database",
                "update-mime-database",
                "gtk-update-icon-cache"
            ]
        );

        // A file someone changed since is left alone.
        fs::write(dirs.icon("com.example.garden", 48), [9, 9]).expect("edit");
        let removal = remove(&applied.record, &dirs, &recorder);
        assert_eq!(removal.left.len(), 1, "{removal:?}");
        assert_eq!(removal.removed.len(), 2, "{removal:?}");
        assert!(!dirs.entry("com.example.garden").exists());
        assert!(read_record(&dirs, "com.example.garden").is_none());
    }

    #[test]
    fn registering_again_replaces_this_applications_own_files() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dirs = dirs(tmp.path());
        let icons = [StagedIcon {
            size: 32,
            png: vec![1],
        }];
        let recorder = Recorder(RefCell::new(Vec::new()));
        let first = plan(&declaration(), Path::new("/opt/a"), &icons, &dirs);
        apply(&first, &dirs, &recorder).expect("first");
        let second = plan(&declaration(), Path::new("/opt/b"), &icons, &dirs);
        assert!(second.conflicts.is_empty(), "{:?}", second.conflicts);
        apply(&second, &dirs, &recorder).expect("second");
        let entry = fs::read_to_string(dirs.entry("com.example.garden")).expect("entry");
        assert!(entry.contains("Exec=\"/opt/b\""));
    }

    #[test]
    fn a_missing_cache_program_is_a_note_not_a_failure() {
        struct Missing;
        impl Refresh for Missing {
            fn run(&self, program: &str, _args: &[&Path]) -> Result<(), String> {
                Err(format!("{program} is not installed"))
            }
        }
        let tmp = tempfile::tempdir().expect("tmp");
        let dirs = dirs(tmp.path());
        let plan = plan(&declaration(), Path::new("/opt/g"), &[], &dirs);
        let applied = apply(&plan, &dirs, &Missing).expect("still registered");
        assert!(!applied.notes.is_empty());
        assert!(dirs.entry("com.example.garden").is_file());
    }
}
