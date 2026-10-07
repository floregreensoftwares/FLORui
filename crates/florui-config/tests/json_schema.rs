//! The editor schema against the parser it is derived from: the committed
//! file is current, every field documents itself, and the schema and the
//! parser agree on what a configuration file may contain.

use std::path::{Path, PathBuf};

use florui_config::{
    CargoProjectFacts, ConfigError, SCHEMA_FILE_NAME, json_schema, json_schema_pretty, resolve,
};
use serde_json::Value;

fn committed_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schema")
        .join(SCHEMA_FILE_NAME)
}

#[test]
fn the_committed_schema_is_the_generated_one() {
    let generated = json_schema_pretty();
    if std::env::var_os("UPDATE_SCHEMA").is_some() {
        std::fs::create_dir_all(committed_path().parent().unwrap()).unwrap();
        std::fs::write(committed_path(), &generated).unwrap();
        return;
    }
    // A checkout may have converted line endings; the content is what counts.
    let committed = std::fs::read_to_string(committed_path())
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        committed == generated,
        "crates/florui-config/schema/{SCHEMA_FILE_NAME} is not what the typed definition generates; \
         regenerate it with `UPDATE_SCHEMA=1 cargo test -p florui-config --test json_schema` and \
         commit the result"
    );
}

/// Every property of every definition, by a readable path.
fn properties(schema: &Value) -> Vec<(String, &Value)> {
    let mut owners: Vec<(String, &Value)> = vec![("(root)".to_string(), schema)];
    if let Some(definitions) = schema.get("definitions").and_then(Value::as_object) {
        for (name, definition) in definitions {
            owners.push((name.clone(), definition));
        }
    }
    let mut found = Vec::new();
    for (owner, object) in owners {
        if let Some(props) = object.get("properties").and_then(Value::as_object) {
            for (name, property) in props {
                found.push((format!("{owner}.{name}"), property));
            }
        }
    }
    found
}

#[test]
fn every_property_and_every_table_describes_itself() {
    let schema = json_schema();
    let mut missing = Vec::new();
    for (path, property) in properties(&schema) {
        let described = property
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty());
        if !described {
            missing.push(path);
        }
    }
    for (name, definition) in schema["definitions"].as_object().unwrap() {
        let described = definition
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty());
        if !described {
            missing.push(format!("{name} (definition)"));
        }
    }
    assert!(
        missing.is_empty(),
        "these have no description, so an editor shows nothing for them: {missing:?}"
    );
}

#[test]
fn no_description_leaks_a_maintainer_note() {
    let schema = json_schema().to_string();
    for note in [
        "Spanned",
        "span",
        "deny_unknown_fields",
        "SchemaVersionProbe",
    ] {
        assert!(
            !schema.contains(note),
            "the schema text mentions {note:?}, which is an implementation detail"
        );
    }
}

#[test]
fn every_table_rejects_a_key_it_does_not_define() {
    let schema = json_schema();
    let mut open = Vec::new();
    if schema.get("additionalProperties") != Some(&Value::Bool(false)) {
        open.push("(root)".to_string());
    }
    for (name, definition) in schema["definitions"].as_object().unwrap() {
        let is_table = definition.get("properties").is_some();
        if is_table && definition.get("additionalProperties") != Some(&Value::Bool(false)) {
            open.push(name.clone());
        }
    }
    assert!(open.is_empty(), "these accept unknown keys: {open:?}");
}

// ----------------------------------------------------------- schema vs parser

fn facts(dir: &Path) -> CargoProjectFacts {
    CargoProjectFacts {
        workspace_root: dir.to_owned(),
        package_root: dir.to_owned(),
        manifest_path: dir.join("Cargo.toml"),
        manifest_text: String::new(),
        target_dir: dir.join("target"),
        package_name: "app".to_owned(),
        package_version: "0.1.0".to_owned(),
        example_targets: Vec::new(),
        legacy_dev_example: None,
        license: None,
        license_file: None,
        homepage: None,
    }
}

/// Whether the schema accepts `document`.
fn schema_accepts(document: &str) -> bool {
    let validator = jsonschema::validator_for(&json_schema()).unwrap();
    let value: toml::Value = match toml::from_str(document) {
        Ok(value) => value,
        // Not even TOML: nothing to validate, and the parser rejects it too.
        Err(_) => return false,
    };
    validator.is_valid(&serde_json::to_value(value).unwrap())
}

/// What the real parser makes of `document`.
fn parser_verdict(document: &str) -> ParserVerdict {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("florui.config.toml"), document).unwrap();
    match resolve(&facts(dir.path()), None, None) {
        Ok(_) => ParserVerdict::Accepted,
        Err(ConfigError::Toml { .. }) => ParserVerdict::RejectedByShape,
        Err(_) => ParserVerdict::RejectedLater,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ParserVerdict {
    Accepted,
    /// Unknown key, wrong type, bad enum value: what the schema describes.
    RejectedByShape,
    /// The shape parsed and a rule that needs code failed (reported with a
    /// location by the resolver).
    RejectedLater,
}

const FULL: &str = r#"
schema_version = 1

[app]
identifier = "com.example.garden"
name = "Garden"
description = "A workspace for ideas"
version = "1.2.3"
default_locale = "en"

[app.icons]
source = "icon.svg"
windows = "icon.svg"
macos = "icon.svg"
linux = "icon.svg"

[app.activation]
single_instance = true
url_schemes = ["garden"]
request_default = ["garden", ".garden"]

[[app.activation.file_associations]]
extension = "garden"
mime_type = "application/x-garden"
description = "A garden file"
identity = "garden-file"

[app.locales.en]
name = "Garden"
description = "A workspace for ideas"

[app.locales.pt-BR]
name = "Jardim"
description = "Um espaco para ideias"

[window]
title = "Garden"
width = 1200.0
height = 800.0
min_width = 800.0
min_height = 600.0
decorations = "custom"
transparent = false

[window.persistence]
enabled = true
key = "main"

[bundle]
publisher = "Floregreen"
copyright = "Copyright 2026 Garden"
license = "MIT OR Apache-2.0"
license_file = "LICENSE"
category = "productivity"
homepage = "https://example.com/garden"

[dev]
example = "dev"

[web]
title = "Garden Web"
description = "In the browser"
base_path = "/garden/"

[web.icons]
favicon = "favicon.svg"
apple_touch_icon = "touch.png"

[environments.development.app]
identifier = "com.example.garden.dev"
name = "Garden Dev"
description = "Development build"

[environments.development.app.icons]
source = "icon-dev.svg"

[environments.development.app.activation]
single_instance = true
url_schemes = ["garden-dev"]
request_default = ["garden-dev", ".garden-dev"]

[[environments.development.app.activation.file_associations]]
extension = "garden-dev"
mime_type = "application/x-garden-dev"
description = "A garden file (development)"
identity = "garden-dev-file"
"#;

/// A document and whether the schema must accept it.
const CASES: &[(&str, &str, bool)] = &[
    ("minimal", "schema_version = 1\n", true),
    ("full", FULL, true),
    (
        "workspace version",
        "schema_version = 1\n[app]\nversion = { workspace = true }\n",
        true,
    ),
    (
        "a default request for something not declared",
        "schema_version = 1
[app.activation]
url_schemes = [\"garden\"]
request_default = [\"other\"]
",
        true,
    ),
    (
        "a default request of the wrong type",
        "schema_version = 1
[app.activation]
request_default = \"garden\"
",
        false,
    ),
    (
        "unknown top-level key",
        "schema_version = 1\nbogus = true\n",
        false,
    ),
    (
        "unknown key in [app]",
        "schema_version = 1\n[app]\nbogus = 1\n",
        false,
    ),
    (
        "unknown key in [window]",
        "schema_version = 1\n[window]\nbogus = 1\n",
        false,
    ),
    (
        "unknown key in [bundle]",
        "schema_version = 1\n[bundle]\nrepository = \"x\"\n",
        false,
    ),
    (
        "unknown key in a locale",
        "schema_version = 1\n[app.locales.en]\nbogus = 1\n",
        false,
    ),
    (
        "unknown key in an environment",
        "schema_version = 1\n[environments.dev]\nbogus = 1\n",
        false,
    ),
    (
        "window key in an environment",
        "schema_version = 1\n[environments.dev.window]\ntitle = \"x\"\n",
        false,
    ),
    (
        "wrong type: width",
        "schema_version = 1\n[window]\nwidth = \"wide\"\n",
        false,
    ),
    (
        "wrong type: transparent",
        "schema_version = 1\n[window]\ntransparent = 1\n",
        false,
    ),
    (
        "wrong type: url_schemes",
        "schema_version = 1\n[app.activation]\nurl_schemes = \"garden\"\n",
        false,
    ),
    (
        "bad enum: decorations",
        "schema_version = 1\n[window]\ndecorations = \"diagonal\"\n",
        false,
    ),
    ("missing schema_version", "[app]\nname = \"x\"\n", false),
    ("unsupported schema_version", "schema_version = 2\n", false),
    ("schema_version as text", "schema_version = \"1\"\n", false),
    (
        "file association without identity",
        "schema_version = 1\n[[app.activation.file_associations]]\nextension = \"g\"\n",
        false,
    ),
];

#[test]
fn the_schema_and_the_parser_agree_on_every_document() {
    let mut disagreements = Vec::new();
    for (name, document, expected) in CASES {
        let schema = schema_accepts(document);
        let parser = parser_verdict(document);
        if schema != *expected {
            disagreements.push(format!(
                "{name}: the schema says {schema}, expected {expected}"
            ));
        }
        // What the schema accepts must never fail on shape in the parser, and
        // what fails on shape in the parser must never be accepted by the
        // schema. A document the parser accepts but the schema rejects, or
        // one that fails only a rule that needs code, is listed below.
        let consistent = !matches!(
            (&parser, schema),
            (ParserVerdict::RejectedByShape, true) | (ParserVerdict::Accepted, false)
        );
        if !consistent {
            disagreements.push(format!(
                "{name}: schema={schema} but the parser says {parser:?}"
            ));
        }
    }
    assert!(disagreements.is_empty(), "{disagreements:#?}");
}

#[test]
fn a_version_the_parser_reports_later_is_not_offered_by_the_schema() {
    // The parser reads these and reports a located error afterwards; the
    // schema does not offer them, so an editor flags them first.
    for document in [
        "schema_version = 1\n[app]\nversion = 123\n",
        "schema_version = 1\n[app]\nversion = { workspace = false }\n",
    ] {
        assert!(!schema_accepts(document), "{document}");
        assert_eq!(
            parser_verdict(document),
            ParserVerdict::RejectedLater,
            "{document}"
        );
    }
}

/// The keys `[app].locales_file` and an environment's per-platform icons
/// cannot appear in `FULL` (the first excludes `locales`), so they have their
/// own example.
const REST: &str = r#"
schema_version = 1

[app]
locales_file = "locales.toml"

[environments.development.app.icons]
windows = "w.svg"
macos = "m.svg"
linux = "l.svg"
"#;

/// Every key path the schema defines, map keys written as `*`.
fn schema_paths(schema: &Value, root: &Value, path: &str, found: &mut Vec<String>) {
    let schema = deref(schema, root);
    if let Some(props) = schema.get("properties").and_then(Value::as_object) {
        for (key, property) in props {
            let at = format!("{path}.{key}");
            found.push(at.clone());
            schema_paths(property, root, &at, found);
        }
    }
    if let Some(extra) = schema.get("additionalProperties").filter(|e| e.is_object()) {
        schema_paths(extra, root, &format!("{path}.*"), found);
    }
    if let Some(items) = schema.get("items") {
        schema_paths(items, root, path, found);
    }
}

/// Every key path a document uses, in the same notation. A table with
/// arbitrary keys is told from a fixed one by the schema.
fn document_paths(
    value: &Value,
    schema: &Value,
    root: &Value,
    path: &str,
    found: &mut Vec<String>,
) {
    let schema = deref(schema, root);
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if let Some(property) = schema.get("properties").and_then(|p| p.get(key)) {
                    let at = format!("{path}.{key}");
                    found.push(at.clone());
                    document_paths(child, property, root, &at, found);
                } else if let Some(extra) =
                    schema.get("additionalProperties").filter(|e| e.is_object())
                {
                    let at = format!("{path}.*");
                    document_paths(child, extra, root, &at, found);
                }
            }
        }
        Value::Array(items) => {
            if let Some(item_schema) = schema.get("items") {
                for item in items {
                    document_paths(item, item_schema, root, path, found);
                }
            }
        }
        _ => {}
    }
}

#[test]
fn the_examples_use_every_key_the_schema_defines() {
    // A key added to a struct without an example here has nothing the other
    // tests validate; this fails until it has one.
    let schema = json_schema();
    let mut defined = Vec::new();
    schema_paths(&schema, &schema, "", &mut defined);
    let mut used = Vec::new();
    for document in [FULL, REST] {
        let value: toml::Value = toml::from_str(document).unwrap();
        let value = serde_json::to_value(value).unwrap();
        document_paths(&value, &schema, &schema, "", &mut used);
    }
    let unused: Vec<&String> = defined.iter().filter(|path| !used.contains(path)).collect();
    assert!(unused.is_empty(), "no example uses: {unused:?}");
}

#[test]
fn the_extra_example_is_valid_for_the_schema() {
    assert!(schema_accepts(REST));
}

#[test]
fn the_real_configurations_in_the_repository_validate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for relative in [
        "examples/florui-example-app/florui.config.toml",
        "crates/florui-cli/templates/new/florui.config.toml.tmpl",
    ] {
        let text = std::fs::read_to_string(root.join(relative)).unwrap();
        // The scaffold's placeholder is a valid TOML string either way.
        let document = text.replace("{{name}}", "demo-app");
        assert!(schema_accepts(&document), "{relative} does not validate");
    }
}

/// The table a schema refers to, through a `$ref` or an `allOf` that carries
/// one next to a description.
fn deref<'a>(schema: &'a Value, root: &'a Value) -> &'a Value {
    let reference = schema
        .get("$ref")
        .or_else(|| {
            schema
                .get("allOf")
                .and_then(|all| all.get(0))
                .and_then(|first| first.get("$ref"))
        })
        .and_then(Value::as_str);
    match reference {
        Some(reference) => &root["definitions"][reference.trim_start_matches("#/definitions/")],
        None => schema,
    }
}
