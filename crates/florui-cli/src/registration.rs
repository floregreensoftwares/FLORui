//! Registering an application's URL schemes and file types with the
//! operating system, and taking the registration away again.
//!
//! This only ever happens in `florui register`, an explicit step: `dev`,
//! `build` and `doctor` never change an association. Everything is written
//! per user (`HKCU`, no administrator), as keys the application owns. Nothing
//! here sets a default handler: registering makes the application a candidate
//! ("Open with", the Default apps list), and the user chooses.
//!
//! # What is written
//!
//! - A URL scheme `S`: the key `Software\\Classes\\S` (a protocol whose
//!   command is the staged executable with the URL as its one argument), and
//!   the ProgId `<identifier>.url.S` the capabilities point at.
//! - A file type with extension `E` and identity `I`: the ProgId
//!   `<identifier>.I`, and the value `<identifier>.I` in
//!   `Software\\Classes\\.E\\OpenWithProgids`. The extension's own default
//!   value and content type are never touched.
//! - The capabilities `Software\\Florui\\<identifier>\\Capabilities` and the
//!   value in `Software\\RegisteredApplications` that list the application in
//!   Settings.
//! - A record of every key and value written, `Software\\Florui\\<identifier>`,
//!   so removing it removes exactly that.
//!
//! Every key the application owns under \`Software\\Classes\` carries the value
//! \`FloruiOwner\` naming the identifier. A scheme or ProgId that exists
//! without that marker, or with another identifier's, belongs to someone else
//! and is a conflict: it is reported before anything is written, and removing
//! a registration leaves anything that is no longer ours alone.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub const OWNER_VALUE: &str = "FloruiOwner";
const CLASSES: &str = "Software\\Classes";
const FLORUI: &str = "Software\\Florui";
const REGISTERED_APPLICATIONS: &str = "Software\\RegisteredApplications";

/// What an application declares to be registered.
#[derive(Debug, Clone, PartialEq)]
pub struct Declaration {
    pub identifier: String,
    pub display_name: String,
    pub description: Option<String>,
    pub schemes: Vec<String>,
    pub associations: Vec<Association>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Association {
    /// Without the dot.
    pub extension: String,
    pub identity: String,
    pub description: Option<String>,
}

/// The per-user registry, as far as registration needs it. Paths are relative
/// to the current user's root and use `\\`.
pub trait Registry {
    fn key_exists(&self, key: &str) -> bool;
    /// The string value `name` of `key`; the empty name is the default value.
    fn get(&self, key: &str, name: &str) -> Option<String>;
    /// Sets a string value, creating the key (and its parents) when needed.
    fn set(&mut self, key: &str, name: &str, data: &str) -> Result<(), String>;
    fn create_key(&mut self, key: &str) -> Result<(), String>;
    fn delete_value(&mut self, key: &str, name: &str) -> Result<(), String>;
    fn delete_tree(&mut self, key: &str) -> Result<(), String>;
    /// No values and no subkeys.
    fn is_empty(&self, key: &str) -> bool;
    /// The same value of the machine-wide registry, which registration only
    /// ever reads.
    fn machine_get(&self, key: &str, name: &str) -> Option<String>;
}

/// Why a registration cannot be written as asked.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Conflict {
    pub what: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Key(String),
    Value {
        key: String,
        name: String,
        data: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub declaration: Declaration,
    pub command: String,
    pub steps: Vec<Step>,
    pub conflicts: Vec<Conflict>,
}

/// Everything a registration wrote, kept so it can be taken away exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub version: u32,
    pub identifier: String,
    pub exe: String,
    /// Keys the registration owns entirely, removed with everything under
    /// them while their owner marker is still ours.
    pub trees: Vec<String>,
    /// Values added to keys that were already there.
    pub values: Vec<ValueRef>,
    /// Keys this registration created that other applications may share
    /// (`.ext\\OpenWithProgids`): removed only if they end up empty.
    pub shared: Vec<String>,
    pub schemes: Vec<String>,
    pub extensions: Vec<String>,
    pub progids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValueRef {
    pub key: String,
    pub name: String,
}

pub fn record_key(identifier: &str) -> String {
    format!("{FLORUI}\\{identifier}")
}

/// A scheme Florui will never register: the ones the web and the system rely
/// on, and every `ms-`/`microsoft-` one.
pub fn is_reserved_scheme(scheme: &str) -> bool {
    const RESERVED: &[&str] = &[
        "http",
        "https",
        "ftp",
        "ftps",
        "file",
        "mailto",
        "tel",
        "sms",
        "data",
        "javascript",
        "about",
        "blob",
        "ws",
        "wss",
        "ssh",
        "ldap",
        "ldaps",
        "news",
        "nntp",
        "telnet",
        "irc",
        "search-ms",
        "shell",
        "res",
        "mk",
        "its",
        "vbscript",
        "callto",
        "magnet",
        "webcal",
        "steam",
        "zoommtg",
        "slack",
        "discord",
        "spotify",
    ];
    let scheme = scheme.to_ascii_lowercase();
    RESERVED.contains(&scheme.as_str())
        || scheme.starts_with("ms-")
        || scheme.starts_with("microsoft-")
}

fn safe_segment(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
}

/// The command a launch runs: the executable and the one argument.
pub fn command_for(exe: &Path) -> Result<String, String> {
    let text = exe
        .to_str()
        .ok_or_else(|| "the executable's path is not valid text".to_string())?;
    if text.contains('"') {
        return Err("the executable's path contains a quote".to_string());
    }
    Ok(format!("\"{text}\" \"%1\""))
}

fn progid_for_scheme(identifier: &str, scheme: &str) -> String {
    format!("{identifier}.url.{}", scheme.to_ascii_lowercase())
}

/// `<identifier>.<identity>`, unless the identity already begins with the
/// identifier (a stable, fully qualified identity), which is used as it is.
fn progid_for_association(identifier: &str, association: &Association) -> String {
    let identity = &association.identity;
    if identity.starts_with(&format!("{identifier}.")) {
        identity.clone()
    } else {
        format!("{identifier}.{identity}")
    }
}

/// What registering `declaration` with `exe` would write, and what stands in
/// its way. Reads the registry and writes nothing.
pub fn plan(declaration: &Declaration, exe: &Path, registry: &dyn Registry) -> Plan {
    let identifier = &declaration.identifier;
    let mut conflicts = Vec::new();
    let mut steps: Vec<Step> = Vec::new();
    let command = match command_for(exe) {
        Ok(command) => command,
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
            reason: "it must be letters, digits and `-_.+` to be part of a registry key"
                .to_string(),
        });
    }
    let icon = format!("\"{}\",0", exe.display());
    let value = |key: &str, name: &str, data: &str| Step::Value {
        key: key.to_string(),
        name: name.to_string(),
        data: data.to_string(),
    };

    // Whether `key` belongs to this identifier, or to someone else and how.
    let foreign_owner = |key: &str| -> Option<String> {
        if !registry.key_exists(key) {
            return None;
        }
        match registry.get(key, OWNER_VALUE) {
            Some(owner) if owner == *identifier => None,
            Some(owner) => Some(format!("it belongs to the Florui application {owner}")),
            None => Some(
                registry
                    .get(&format!("{key}\\shell\\open\\command"), "")
                    .map_or_else(
                        || "it is already registered by another application".to_string(),
                        |command| format!("it is already registered by {command}"),
                    ),
            ),
        }
    };

    for scheme in &declaration.schemes {
        let lower = scheme.to_ascii_lowercase();
        if is_reserved_scheme(&lower) {
            conflicts.push(Conflict {
                what: format!("the URL scheme \"{scheme}\""),
                reason: "it is a reserved scheme that the system or the web relies on".to_string(),
            });
            continue;
        }
        if !safe_segment(&lower) {
            conflicts.push(Conflict {
                what: format!("the URL scheme \"{scheme}\""),
                reason: "it cannot be a registry key".to_string(),
            });
            continue;
        }
        let key = format!("{CLASSES}\\{lower}");
        if let Some(reason) = foreign_owner(&key) {
            conflicts.push(Conflict {
                what: format!("the URL scheme \"{scheme}\""),
                reason,
            });
            continue;
        }
        // Registered machine-wide by something else: a per-user entry would
        // shadow it, which is a takeover.
        if !registry.key_exists(&key)
            && let Some(machine) = registry.machine_get(&format!("{key}\\shell\\open\\command"), "")
        {
            conflicts.push(Conflict {
                what: format!("the URL scheme \"{scheme}\""),
                reason: format!("it is registered for every user by {machine}"),
            });
            continue;
        }
        let progid = progid_for_scheme(identifier, scheme);
        if let Some(reason) = foreign_owner(&format!("{CLASSES}\\{progid}")) {
            conflicts.push(Conflict {
                what: format!("the ProgId \"{progid}\""),
                reason,
            });
            continue;
        }
        for class in [lower.clone(), progid.clone()] {
            let key = format!("{CLASSES}\\{class}");
            steps.push(Step::Key(key.clone()));
            // The protocol key follows the `URL:` convention; the ProgId is
            // what Settings shows the user, so it is plain.
            let shown = if class == lower {
                format!("URL:{} ({lower})", declaration.display_name)
            } else {
                format!("{} ({lower})", declaration.display_name)
            };
            steps.push(value(&key, "", &shown));
            steps.push(value(&key, "URL Protocol", ""));
            steps.push(value(&key, OWNER_VALUE, identifier));
            steps.push(value(&format!("{key}\\DefaultIcon"), "", &icon));
            steps.push(value(&format!("{key}\\shell\\open\\command"), "", &command));
        }
    }

    for association in &declaration.associations {
        let extension = &association.extension;
        if !safe_segment(extension) || !safe_segment(&association.identity) {
            conflicts.push(Conflict {
                what: format!(
                    "the file type \".{extension}\" (identity \"{}\")",
                    association.identity
                ),
                reason: "the extension and the identity must be letters, digits and `-_.+`"
                    .to_string(),
            });
            continue;
        }
        let progid = progid_for_association(identifier, association);
        let key = format!("{CLASSES}\\{progid}");
        if let Some(reason) = foreign_owner(&key) {
            conflicts.push(Conflict {
                what: format!("the ProgId \"{progid}\""),
                reason,
            });
            continue;
        }
        steps.push(Step::Key(key.clone()));
        steps.push(value(
            &key,
            "",
            association
                .description
                .as_deref()
                .unwrap_or(&format!("{} .{extension} file", declaration.display_name)),
        ));
        steps.push(value(&key, OWNER_VALUE, identifier));
        steps.push(value(&format!("{key}\\DefaultIcon"), "", &icon));
        steps.push(value(&format!("{key}\\shell\\open\\command"), "", &command));
        let with = format!("{CLASSES}\\.{extension}\\OpenWithProgids");
        steps.push(Step::Key(with.clone()));
        steps.push(value(&with, &progid, ""));
    }

    // The capabilities, which list the application in Settings, and the
    // record. Both live in the application's own namespace.
    let capabilities = format!("{}\\Capabilities", record_key(identifier));
    steps.push(Step::Key(capabilities.clone()));
    steps.push(value(
        &capabilities,
        "ApplicationName",
        &declaration.display_name,
    ));
    steps.push(value(
        &capabilities,
        "ApplicationDescription",
        declaration
            .description
            .as_deref()
            .unwrap_or(&declaration.display_name),
    ));
    if !declaration.associations.is_empty() {
        let key = format!("{capabilities}\\FileAssociations");
        steps.push(Step::Key(key.clone()));
        for association in &declaration.associations {
            steps.push(value(
                &key,
                &format!(".{}", association.extension),
                &progid_for_association(identifier, association),
            ));
        }
    }
    if !declaration.schemes.is_empty() {
        let key = format!("{capabilities}\\UrlAssociations");
        steps.push(Step::Key(key.clone()));
        for scheme in &declaration.schemes {
            steps.push(value(
                &key,
                &scheme.to_ascii_lowercase(),
                &progid_for_scheme(identifier, scheme),
            ));
        }
    }
    steps.push(Step::Key(REGISTERED_APPLICATIONS.to_string()));
    steps.push(value(REGISTERED_APPLICATIONS, identifier, &capabilities));

    if let Some(existing) = registry.get(REGISTERED_APPLICATIONS, identifier)
        && existing != capabilities
    {
        conflicts.push(Conflict {
            what: format!("the registered application \"{identifier}\""),
            reason: format!("it already points at {existing}"),
        });
    }

    Plan {
        declaration: declaration.clone(),
        command,
        steps,
        conflicts,
    }
}

/// The record of an earlier registration of `identifier`, if there is one.
pub fn read_record(registry: &dyn Registry, identifier: &str) -> Option<Record> {
    let text = registry.get(&record_key(identifier), "Record")?;
    serde_json::from_str(&text).ok()
}

/// The keys a registration owns entirely: one per scheme and ProgId under
/// the classes, and the application's own namespace.
fn tree_roots(declaration: &Declaration) -> Vec<String> {
    let identifier = &declaration.identifier;
    let mut roots = Vec::new();
    for scheme in &declaration.schemes {
        roots.push(format!("{CLASSES}\\{}", scheme.to_ascii_lowercase()));
        roots.push(format!(
            "{CLASSES}\\{}",
            progid_for_scheme(identifier, scheme)
        ));
    }
    for association in &declaration.associations {
        roots.push(format!(
            "{CLASSES}\\{}",
            progid_for_association(identifier, association)
        ));
    }
    roots.push(record_key(identifier));
    roots
}

/// Creates `key` and every parent that is missing, noting which were made.
fn create_tracked(
    registry: &mut dyn Registry,
    key: &str,
    created: &mut Vec<String>,
) -> Result<(), String> {
    let mut path = String::new();
    for part in key.split('\\') {
        if !path.is_empty() {
            path.push('\\');
        }
        path.push_str(part);
        if !registry.key_exists(&path) {
            registry.create_key(&path)?;
            created.push(path.clone());
        }
    }
    Ok(())
}

/// Writes `plan`, replacing an earlier registration of the same identifier,
/// and records what it wrote. Refuses a plan with conflicts.
pub fn apply(plan: &Plan, exe: &Path, registry: &mut dyn Registry) -> Result<Record, String> {
    if !plan.conflicts.is_empty() {
        return Err("the plan has conflicts, so nothing is written".to_string());
    }
    let identifier = &plan.declaration.identifier;
    // An earlier registration is taken away first, so what it wrote that this
    // one no longer asks for does not linger.
    if let Some(old) = read_record(registry, identifier) {
        remove(&old, registry);
    }

    let roots = tree_roots(&plan.declaration);
    let in_a_tree = |key: &str| {
        roots
            .iter()
            .any(|root| key == root || key.starts_with(&format!("{root}\\")))
    };
    let mut created: Vec<String> = Vec::new();
    let mut values: Vec<ValueRef> = Vec::new();
    for step in &plan.steps {
        match step {
            Step::Key(key) => create_tracked(registry, key, &mut created)?,
            Step::Value { key, name, data } => {
                create_tracked(registry, key, &mut created)?;
                // A value in a key the registration does not own is recorded,
                // so it can be taken out without touching its neighbours.
                if !in_a_tree(key) && registry.get(key, name).is_none() {
                    values.push(ValueRef {
                        key: key.clone(),
                        name: name.clone(),
                    });
                }
                registry.set(key, name, data)?;
            }
        }
    }
    let record = Record {
        version: 1,
        identifier: identifier.clone(),
        exe: exe.display().to_string(),
        trees: roots.clone(),
        values,
        // What was made outside the trees, and may be shared, is only removed
        // again if it ends up empty.
        shared: created.into_iter().filter(|key| !in_a_tree(key)).collect(),
        schemes: plan
            .declaration
            .schemes
            .iter()
            .map(|s| s.to_ascii_lowercase())
            .collect(),
        extensions: plan
            .declaration
            .associations
            .iter()
            .map(|a| a.extension.clone())
            .collect(),
        progids: plan
            .declaration
            .schemes
            .iter()
            .map(|s| progid_for_scheme(identifier, s))
            .chain(
                plan.declaration
                    .associations
                    .iter()
                    .map(|a| progid_for_association(identifier, a)),
            )
            .collect(),
    };
    let text = serde_json::to_string(&record).map_err(|error| error.to_string())?;
    registry.set(&record_key(identifier), "Record", &text)?;
    Ok(record)
}

/// What a removal did, so the report can say what it left alone.
#[derive(Debug, Default, PartialEq, Serialize)]
pub struct Removal {
    pub removed: Vec<String>,
    pub left: Vec<String>,
}

/// Takes away exactly what `record` lists: a tree only while it still names
/// this identifier as its owner, a value only if it is still there, a shared
/// key only if it is empty. Anything else is left alone and reported.
pub fn remove(record: &Record, registry: &mut dyn Registry) -> Removal {
    let mut removal = Removal::default();
    for value in record.values.iter().rev() {
        if registry.get(&value.key, &value.name).is_some()
            && registry.delete_value(&value.key, &value.name).is_ok()
        {
            removal
                .removed
                .push(format!("{} / {}", value.key, value.name));
        }
    }
    let root = record_key(&record.identifier);
    for key in &record.trees {
        if !registry.key_exists(key) {
            continue;
        }
        let ours = *key == root
            || registry.get(key, OWNER_VALUE).as_deref() == Some(record.identifier.as_str());
        if !ours {
            removal.left.push(format!("{key} (no longer ours)"));
            continue;
        }
        if registry.delete_tree(key).is_ok() {
            removal.removed.push(key.clone());
        }
    }
    for key in record.shared.iter().rev() {
        if registry.key_exists(key) && registry.is_empty(key) && registry.delete_tree(key).is_ok() {
            removal.removed.push(key.clone());
        }
    }
    removal
}

#[cfg(windows)]
pub use real::CurrentUser;

#[cfg(windows)]
mod real {
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS, KEY_READ,
        REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW,
        RegDeleteValueW, RegEnumKeyExW, RegEnumValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW,
    };

    use super::Registry;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    /// The current user's registry (`HKEY_CURRENT_USER`).
    pub struct CurrentUser;

    fn open(root: HKEY, key: &str, access: u32) -> Option<HKEY> {
        let mut handle: HKEY = null_mut();
        // SAFETY: `key` is a NUL-terminated buffer that outlives the call and
        // `handle` is a valid out pointer; the handle is closed by the caller.
        let status = unsafe { RegOpenKeyExW(root, wide(key).as_ptr(), 0, access, &mut handle) };
        (status == ERROR_SUCCESS).then_some(handle)
    }

    fn read(root: HKEY, key: &str, name: &str) -> Option<String> {
        let handle = open(root, key, KEY_READ)?;
        let name = wide(name);
        let mut kind = 0u32;
        let mut size = 0u32;
        // SAFETY: the handle is open for reading; the first call only asks
        // for the size, the second fills a buffer of exactly that many bytes;
        // the handle is closed before returning.
        let value = unsafe {
            let first = RegQueryValueExW(
                handle,
                name.as_ptr(),
                null(),
                &mut kind,
                null_mut(),
                &mut size,
            );
            if first != ERROR_SUCCESS || kind != REG_SZ {
                RegCloseKey(handle);
                return None;
            }
            let mut buffer = vec![0u16; (size as usize).div_ceil(2)];
            let mut length = size;
            let second = RegQueryValueExW(
                handle,
                name.as_ptr(),
                null(),
                &mut kind,
                buffer.as_mut_ptr().cast(),
                &mut length,
            );
            RegCloseKey(handle);
            if second != ERROR_SUCCESS {
                return None;
            }
            buffer.truncate(length as usize / 2);
            while buffer.last() == Some(&0) {
                buffer.pop();
            }
            String::from_utf16_lossy(&buffer)
        };
        Some(value)
    }

    impl Registry for CurrentUser {
        fn key_exists(&self, key: &str) -> bool {
            open(HKEY_CURRENT_USER, key, KEY_READ).is_some_and(|handle| {
                // SAFETY: the handle was just opened.
                unsafe { RegCloseKey(handle) };
                true
            })
        }

        fn get(&self, key: &str, name: &str) -> Option<String> {
            read(HKEY_CURRENT_USER, key, name)
        }

        fn create_key(&mut self, key: &str) -> Result<(), String> {
            let mut handle: HKEY = null_mut();
            // SAFETY: `key` is NUL-terminated and outlives the call; `handle`
            // is a valid out pointer closed right after.
            let status = unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    wide(key).as_ptr(),
                    0,
                    null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_ALL_ACCESS,
                    null(),
                    &mut handle,
                    null_mut(),
                )
            };
            if status != ERROR_SUCCESS {
                return Err(format!("could not create HKCU\\{key} (error {status})"));
            }
            // SAFETY: the handle was just created.
            unsafe { RegCloseKey(handle) };
            Ok(())
        }

        fn set(&mut self, key: &str, name: &str, data: &str) -> Result<(), String> {
            self.create_key(key)?;
            let handle = open(HKEY_CURRENT_USER, key, KEY_ALL_ACCESS)
                .ok_or_else(|| format!("could not open HKCU\\{key}"))?;
            let bytes: Vec<u8> = wide(data)
                .iter()
                .flat_map(|unit| unit.to_le_bytes())
                .collect();
            // SAFETY: the handle is open for writing; `bytes` holds the
            // NUL-terminated UTF-16 data and its length is passed with it.
            let status = unsafe {
                let status = RegSetValueExW(
                    handle,
                    wide(name).as_ptr(),
                    0,
                    REG_SZ,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                );
                RegCloseKey(handle);
                status
            };
            if status != ERROR_SUCCESS {
                return Err(format!("could not write HKCU\\{key} (error {status})"));
            }
            Ok(())
        }

        fn delete_value(&mut self, key: &str, name: &str) -> Result<(), String> {
            let handle = open(HKEY_CURRENT_USER, key, KEY_ALL_ACCESS)
                .ok_or_else(|| format!("could not open HKCU\\{key}"))?;
            // SAFETY: the handle is open for writing and `name` is NUL-terminated.
            let status = unsafe {
                let status = RegDeleteValueW(handle, wide(name).as_ptr());
                RegCloseKey(handle);
                status
            };
            if status != ERROR_SUCCESS {
                return Err(format!(
                    "could not delete a value of HKCU\\{key} (error {status})"
                ));
            }
            Ok(())
        }

        fn delete_tree(&mut self, key: &str) -> Result<(), String> {
            // SAFETY: `key` is NUL-terminated and outlives the call.
            let status = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide(key).as_ptr()) };
            if status != ERROR_SUCCESS {
                return Err(format!("could not delete HKCU\\{key} (error {status})"));
            }
            Ok(())
        }

        fn is_empty(&self, key: &str) -> bool {
            let Some(handle) = open(HKEY_CURRENT_USER, key, KEY_READ) else {
                return false;
            };
            let mut name = [0u16; 1];
            // SAFETY: the handle is open for reading; the buffers are valid for
            // the lengths passed; asking for index 0 only tells whether any
            // value or subkey exists (the answer `ERROR_MORE_DATA` or success
            // means one does), and the handle is closed before returning.
            let (values, keys) = unsafe {
                let mut length = 1u32;
                let values = RegEnumValueW(
                    handle,
                    0,
                    name.as_mut_ptr(),
                    &mut length,
                    null(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                );
                length = 1;
                let keys = RegEnumKeyExW(
                    handle,
                    0,
                    name.as_mut_ptr(),
                    &mut length,
                    null(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                );
                RegCloseKey(handle);
                (values, keys)
            };
            const NO_MORE_ITEMS: u32 = 259;
            values == NO_MORE_ITEMS && keys == NO_MORE_ITEMS
        }

        fn machine_get(&self, key: &str, name: &str) -> Option<String> {
            read(HKEY_LOCAL_MACHINE, key, name)
        }
    }
}

#[cfg(test)]
pub mod memory {
    use std::collections::BTreeMap;

    use super::Registry;

    /// A registry in memory, for tests. Keys are matched without regard to
    /// case, as the real one does.
    #[derive(Default, Clone)]
    pub struct Memory {
        keys: BTreeMap<String, BTreeMap<String, String>>,
        machine: BTreeMap<String, BTreeMap<String, String>>,
    }

    fn norm(key: &str) -> String {
        key.to_ascii_lowercase()
    }

    impl Memory {
        pub fn machine_set(&mut self, key: &str, name: &str, data: &str) {
            self.machine
                .entry(norm(key))
                .or_default()
                .insert(name.to_string(), data.to_string());
        }

        /// Every key and value, to compare two states.
        pub fn snapshot(&self) -> Vec<(String, String, String)> {
            self.keys
                .iter()
                .flat_map(|(key, values)| {
                    let mut rows: Vec<_> = values
                        .iter()
                        .map(|(name, data)| (key.clone(), name.clone(), data.clone()))
                        .collect();
                    if rows.is_empty() {
                        rows.push((key.clone(), String::new(), "<key>".to_string()));
                    }
                    rows
                })
                .collect()
        }
    }

    impl Registry for Memory {
        fn key_exists(&self, key: &str) -> bool {
            self.keys.contains_key(&norm(key))
        }
        fn get(&self, key: &str, name: &str) -> Option<String> {
            self.keys.get(&norm(key))?.get(name).cloned()
        }
        fn create_key(&mut self, key: &str) -> Result<(), String> {
            let mut path = String::new();
            for part in key.split('\\') {
                if !path.is_empty() {
                    path.push('\\');
                }
                path.push_str(part);
                self.keys.entry(norm(&path)).or_default();
            }
            Ok(())
        }
        fn set(&mut self, key: &str, name: &str, data: &str) -> Result<(), String> {
            self.create_key(key)?;
            self.keys
                .get_mut(&norm(key))
                .expect("just created")
                .insert(name.to_string(), data.to_string());
            Ok(())
        }
        fn delete_value(&mut self, key: &str, name: &str) -> Result<(), String> {
            self.keys
                .get_mut(&norm(key))
                .and_then(|values| values.remove(name))
                .map(|_| ())
                .ok_or_else(|| "no such value".to_string())
        }
        fn delete_tree(&mut self, key: &str) -> Result<(), String> {
            let key = norm(key);
            let prefix = format!("{key}\\");
            self.keys
                .retain(|existing, _| *existing != key && !existing.starts_with(&prefix));
            Ok(())
        }
        fn is_empty(&self, key: &str) -> bool {
            let key = norm(key);
            let prefix = format!("{key}\\");
            self.keys.get(&key).is_some_and(|values| values.is_empty())
                && !self.keys.keys().any(|other| other.starts_with(&prefix))
        }
        fn machine_get(&self, key: &str, name: &str) -> Option<String> {
            self.machine.get(&norm(key))?.get(name).cloned()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::memory::Memory;
    use super::*;

    fn exe() -> PathBuf {
        PathBuf::from("C:\\Users\\Ana Maria\\Jardim ação\\garden.exe")
    }

    fn declaration() -> Declaration {
        Declaration {
            identifier: "com.example.garden".to_string(),
            display_name: "Garden".to_string(),
            description: Some("A workspace".to_string()),
            schemes: vec!["garden".to_string()],
            associations: vec![Association {
                extension: "garden".to_string(),
                identity: "document".to_string(),
                description: Some("A garden file".to_string()),
            }],
        }
    }

    fn registered() -> (Memory, Record) {
        let mut registry = Memory::default();
        let planned = plan(&declaration(), &exe(), &registry);
        assert_eq!(planned.conflicts, vec![]);
        let record = apply(&planned, &exe(), &mut registry).unwrap();
        (registry, record)
    }

    #[test]
    fn the_command_quotes_the_path_and_takes_one_argument() {
        assert_eq!(
            command_for(&exe()).unwrap(),
            "\"C:\\Users\\Ana Maria\\Jardim ação\\garden.exe\" \"%1\""
        );
        assert!(
            command_for(Path::new("C:\\a\"b\\x.exe")).is_err(),
            "a quote in the path"
        );
    }

    #[test]
    fn registering_writes_the_scheme_the_file_type_and_the_capabilities() {
        let (registry, record) = registered();
        let command = "\"C:\\Users\\Ana Maria\\Jardim ação\\garden.exe\" \"%1\"";

        let scheme = "Software\\Classes\\garden";
        assert_eq!(registry.get(scheme, "URL Protocol").as_deref(), Some(""));
        assert_eq!(
            registry.get(scheme, OWNER_VALUE).as_deref(),
            Some("com.example.garden")
        );
        assert_eq!(
            registry
                .get(&format!("{scheme}\\shell\\open\\command"), "")
                .as_deref(),
            Some(command)
        );

        let progid = "Software\\Classes\\com.example.garden.document";
        assert_eq!(registry.get(progid, "").as_deref(), Some("A garden file"));
        assert_eq!(
            registry
                .get(&format!("{progid}\\shell\\open\\command"), "")
                .as_deref(),
            Some(command)
        );
        assert_eq!(
            registry
                .get(
                    "Software\\Classes\\.garden\\OpenWithProgids",
                    "com.example.garden.document"
                )
                .as_deref(),
            Some("")
        );
        let capabilities = "Software\\Florui\\com.example.garden\\Capabilities";
        assert_eq!(
            registry.get(capabilities, "ApplicationName").as_deref(),
            Some("Garden")
        );
        assert_eq!(
            registry
                .get(&format!("{capabilities}\\FileAssociations"), ".garden")
                .as_deref(),
            Some("com.example.garden.document")
        );
        assert_eq!(
            registry
                .get(&format!("{capabilities}\\UrlAssociations"), "garden")
                .as_deref(),
            Some("com.example.garden.url.garden")
        );
        assert_eq!(
            registry
                .get("Software\\RegisteredApplications", "com.example.garden")
                .as_deref(),
            Some(capabilities)
        );
        assert_eq!(record.schemes, vec!["garden"]);
        assert_eq!(
            read_record(&registry, "com.example.garden").unwrap(),
            record,
            "the record is kept in the registry"
        );
        // The extension's own default and content type are never written.
        assert_eq!(registry.get("Software\\Classes\\.garden", ""), None);
        assert_eq!(
            registry.get("Software\\Classes\\.garden", "Content Type"),
            None
        );
    }

    #[test]
    fn an_identity_that_already_carries_the_identifier_is_not_prefixed_twice() {
        let mut declared = declaration();
        declared.associations[0].identity = "com.example.garden.document".to_string();
        let mut registry = Memory::default();
        let planned = plan(&declared, &exe(), &registry);
        let record = apply(&planned, &exe(), &mut registry).unwrap();
        assert_eq!(record.progids[1], "com.example.garden.document");
        assert!(registry.key_exists("Software\\Classes\\com.example.garden.document"));
        assert!(
            !registry
                .key_exists("Software\\Classes\\com.example.garden.com.example.garden.document")
        );
        // An identity that is only a name is prefixed.
        assert_eq!(
            progid_for_association("com.example.garden", &declaration().associations[0]),
            "com.example.garden.document"
        );
    }

    #[test]
    fn registering_again_is_idempotent_and_drops_what_is_no_longer_asked_for() {
        let (mut registry, _) = registered();
        let before = registry.snapshot();
        let again = plan(&declaration(), &exe(), &registry);
        assert_eq!(
            again.conflicts,
            vec![],
            "its own registration is not a conflict"
        );
        apply(&again, &exe(), &mut registry).unwrap();
        assert_eq!(registry.snapshot(), before);

        let mut smaller = declaration();
        smaller.associations.clear();
        let planned = plan(&smaller, &exe(), &registry);
        apply(&planned, &exe(), &mut registry).unwrap();
        assert!(!registry.key_exists("Software\\Classes\\com.example.garden.document"));
        assert!(
            !registry.key_exists("Software\\Classes\\.garden"),
            "the shared key it created is gone"
        );
        assert!(registry.key_exists("Software\\Classes\\garden"));
    }

    #[test]
    fn removing_restores_the_registry_exactly() {
        let mut registry = Memory::default();
        // Something else already uses the extension.
        registry
            .set("Software\\Classes\\.garden", "", "gardenfile")
            .unwrap();
        registry
            .set(
                "Software\\Classes\\.garden\\OpenWithProgids",
                "other.app",
                "",
            )
            .unwrap();
        registry
            .set(
                "Software\\Classes\\gardenfile\\shell\\open\\command",
                "",
                "\"C:\\other.exe\" \"%1\"",
            )
            .unwrap();
        let before = registry.snapshot();

        let planned = plan(&declaration(), &exe(), &registry);
        let record = apply(&planned, &exe(), &mut registry).unwrap();
        assert_ne!(registry.snapshot(), before);
        let removal = remove(&record, &mut registry);

        assert_eq!(registry.snapshot(), before, "{removal:?}");
        assert_eq!(removal.left, Vec::<String>::new());
    }

    #[test]
    fn a_scheme_someone_else_owns_is_a_conflict_and_nothing_is_written() {
        let mut registry = Memory::default();
        registry
            .set("Software\\Classes\\garden", "URL Protocol", "")
            .unwrap();
        registry
            .set(
                "Software\\Classes\\garden\\shell\\open\\command",
                "",
                "\"C:\\other.exe\" \"%1\"",
            )
            .unwrap();
        let before = registry.snapshot();

        let planned = plan(&declaration(), &exe(), &registry);

        assert_eq!(planned.conflicts.len(), 1);
        assert!(
            planned.conflicts[0].reason.contains("C:\\other.exe"),
            "{:?}",
            planned.conflicts
        );
        assert!(apply(&planned, &exe(), &mut registry).is_err());
        assert_eq!(registry.snapshot(), before);
    }

    #[test]
    fn a_scheme_registered_for_every_user_is_not_shadowed() {
        let mut registry = Memory::default();
        registry.machine_set(
            "Software\\Classes\\garden\\shell\\open\\command",
            "",
            "\"C:\\Program Files\\Other\\o.exe\" \"%1\"",
        );
        let planned = plan(&declaration(), &exe(), &registry);
        assert!(
            planned.conflicts[0].reason.contains("every user"),
            "{:?}",
            planned.conflicts
        );
    }

    #[test]
    fn another_environments_registration_is_a_conflict() {
        let (mut registry, _) = registered();
        let mut other = declaration();
        other.identifier = "com.example.garden.dev".to_string();

        let planned = plan(&other, &exe(), &registry);

        assert!(
            planned
                .conflicts
                .iter()
                .any(|c| c.reason.contains("com.example.garden")),
            "{:?}",
            planned.conflicts
        );
        // A distinct scheme and identity do not contend.
        other.schemes = vec!["garden-dev".to_string()];
        other.associations[0].extension = "gardendev".to_string();
        let separate = plan(&other, &exe(), &registry);
        assert_eq!(separate.conflicts, vec![]);
        apply(&separate, &exe(), &mut registry).unwrap();
        assert!(registry.key_exists("Software\\Classes\\garden-dev"));
        assert!(
            registry.key_exists("Software\\Classes\\garden"),
            "the first is untouched"
        );
    }

    #[test]
    fn reserved_schemes_are_refused() {
        for scheme in [
            "http",
            "HTTPS",
            "mailto",
            "file",
            "ms-settings",
            "microsoft-edge",
        ] {
            let mut declared = declaration();
            declared.schemes = vec![scheme.to_string()];
            let planned = plan(&declared, &exe(), &Memory::default());
            assert!(
                planned
                    .conflicts
                    .iter()
                    .any(|c| c.reason.contains("reserved")),
                "{scheme}: {:?}",
                planned.conflicts
            );
        }
        assert!(!is_reserved_scheme("garden"));
    }

    #[test]
    fn removing_leaves_a_key_that_is_no_longer_ours_and_says_so() {
        let (mut registry, record) = registered();
        // Another application took the scheme over after the registration.
        registry
            .set("Software\\Classes\\garden", OWNER_VALUE, "com.someone.else")
            .unwrap();

        let removal = remove(&record, &mut registry);

        assert!(registry.key_exists("Software\\Classes\\garden"));
        assert!(
            removal.left.iter().any(|l| l.contains("no longer ours")),
            "{removal:?}"
        );
        assert!(!registry.key_exists("Software\\Classes\\com.example.garden.document"));
        assert!(
            !registry.key_exists("Software\\Florui\\com.example.garden"),
            "the record goes"
        );
    }

    #[test]
    fn a_missing_or_unreadable_record_reads_as_none() {
        let mut registry = Memory::default();
        assert_eq!(read_record(&registry, "com.example.garden"), None);
        registry
            .set(&record_key("com.example.garden"), "Record", "not json")
            .unwrap();
        assert_eq!(read_record(&registry, "com.example.garden"), None);
    }

    #[test]
    fn an_identity_or_extension_that_cannot_be_a_key_is_refused() {
        let mut declared = declaration();
        declared.associations[0].identity = "a\\b".to_string();
        let planned = plan(&declared, &exe(), &Memory::default());
        assert!(!planned.conflicts.is_empty());
        let mut declared = declaration();
        declared.identifier = "bad id".to_string();
        assert!(
            !plan(&declared, &exe(), &Memory::default())
                .conflicts
                .is_empty()
        );
    }
}
