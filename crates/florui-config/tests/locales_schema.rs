//! The editor schema of `florui.locales.toml` against the parser it is
//! derived from, the same way `json_schema.rs` checks the configuration's.

use std::path::{Path, PathBuf};

use florui_config::{
    CargoProjectFacts, ConfigError, LOCALES_SCHEMA_FILE_NAME, locales_json_schema,
    locales_json_schema_pretty, resolve,
};
use serde_json::Value;

fn committed_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("schema")
        .join(LOCALES_SCHEMA_FILE_NAME)
}

#[test]
fn the_committed_schema_is_the_generated_one() {
    let generated = locales_json_schema_pretty();
    if std::env::var_os("UPDATE_SCHEMA").is_some() {
        std::fs::create_dir_all(committed_path().parent().unwrap()).unwrap();
        std::fs::write(committed_path(), &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(committed_path())
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        committed == generated,
        "crates/florui-config/schema/{LOCALES_SCHEMA_FILE_NAME} is not what the typed definition \
         generates; regenerate it with `UPDATE_SCHEMA=1 cargo test -p florui-config --test \
         locales_schema` and commit the result"
    );
}

#[test]
fn the_file_is_a_table_of_locales_each_described_and_closed() {
    let schema = locales_json_schema();

    assert_eq!(schema["type"], "object");
    assert_eq!(
        schema["additionalProperties"]["$ref"], "#/definitions/Locale",
        "{schema}"
    );
    assert!(
        schema["description"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
    let locale = &schema["definitions"]["Locale"];
    assert_eq!(
        locale["additionalProperties"],
        Value::Bool(false),
        "{locale}"
    );
    for (key, property) in locale["properties"].as_object().unwrap() {
        assert!(
            property["description"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "{key} has no description"
        );
    }
    assert!(
        locale["description"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
    assert!(schema.get("$id").is_none(), "no URL is published");
}

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

fn schema_accepts(document: &str) -> bool {
    let validator = jsonschema::validator_for(&locales_json_schema()).unwrap();
    match toml::from_str::<toml::Value>(document) {
        Ok(value) => validator.is_valid(&serde_json::to_value(value).unwrap()),
        Err(_) => false,
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Parser {
    Accepted,
    RejectedByShape,
    RejectedLater,
}

/// What the real parser makes of `document` as the conventional
/// `florui.locales.toml` next to a minimal configuration.
fn parser_verdict(document: &str) -> Parser {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("florui.config.toml"),
        "schema_version = 1\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("florui.locales.toml"), document).unwrap();
    match resolve(&facts(dir.path()), None, None) {
        Ok(_) => Parser::Accepted,
        Err(ConfigError::Toml { .. }) => Parser::RejectedByShape,
        Err(_) => Parser::RejectedLater,
    }
}

const FULL: &str = r#"
[en]
name = "Garden"
description = "A workspace for ideas"

[pt-BR]
name = "Jardim"
description = "Um espaco para ideias"
"#;

/// A document and whether the schema must accept it.
const CASES: &[(&str, &str, bool)] = &[
    ("empty file", "", true),
    ("full", FULL, true),
    (
        "a locale with only a name",
        "[en]\nname = \"Garden\"\n",
        true,
    ),
    ("an empty locale", "[en]\n", true),
    ("unknown key in a locale", "[en]\nbogus = 1\n", false),
    ("name of the wrong type", "[en]\nname = 1\n", false),
    (
        "description of the wrong type",
        "[en]\ndescription = [\"a\"]\n",
        false,
    ),
    ("a locale that is not a table", "en = \"Garden\"\n", false),
    (
        "a key at the top that is not a table",
        "name = \"Garden\"\n",
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
        if matches!(
            (&parser, schema),
            (Parser::RejectedByShape, true) | (Parser::Accepted, false)
        ) {
            disagreements.push(format!(
                "{name}: schema={schema} but the parser says {parser:?}"
            ));
        }
    }
    assert!(disagreements.is_empty(), "{disagreements:#?}");
}

#[test]
fn a_tag_the_resolver_rejects_later_is_still_a_table_the_schema_accepts() {
    // Whether `not a tag` is a locale tag is a rule that needs code, so the
    // schema leaves it to the resolver, which reports it with a location.
    let document = "[\"not a tag\"]\nname = \"x\"\n";
    assert!(schema_accepts(document));
    assert_ne!(parser_verdict(document), Parser::RejectedByShape);
}

#[test]
fn the_example_uses_every_key_the_schema_defines() {
    let schema = locales_json_schema();
    let defined: Vec<String> = schema["definitions"]["Locale"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let value: toml::Value = toml::from_str(FULL).unwrap();
    let used: Vec<String> = value
        .as_table()
        .unwrap()
        .values()
        .flat_map(|locale| locale.as_table().unwrap().keys().cloned())
        .collect();
    let unused: Vec<&String> = defined.iter().filter(|key| !used.contains(key)).collect();
    assert!(unused.is_empty(), "no example uses: {unused:?}");
}
