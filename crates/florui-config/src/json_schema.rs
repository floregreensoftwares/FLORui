//! The JSON Schema editors read for `florui.config.toml`, generated from the
//! same typed definition the parser deserializes into (`schema.rs`), so the
//! two cannot describe different files.
//!
//! The schema says what the parser itself enforces: the keys that exist, the
//! type of each, the enum values, that `schema_version` is present and
//! supported, and that no unknown key is accepted. Rules that need code
//! (SPDX syntax, URL shape, locale tags, window-size consistency) stay in
//! the resolver, where they are reported with a line and column, and are
//! described in each field's text instead.
//!
//! There is no `$id`: no URL for the schema is published, and none is
//! invented.

use schemars::generate::SchemaSettings;
use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::schema::{RawConfig, RawLocale, SUPPORTED_SCHEMA_VERSION};

/// The conventional file name for a project's local copy of the schema.
pub const SCHEMA_FILE_NAME: &str = "florui.config.schema.json";

/// The same for the locales file (`florui.locales.toml`), whose entries are
/// the `[app.locales]` of the configuration kept in a file of their own.
pub const LOCALES_SCHEMA_FILE_NAME: &str = "florui.locales.schema.json";

fn generate<T: schemars::JsonSchema>(title: &str, description: &str) -> Value {
    let generator = SchemaSettings::draft07().into_generator();
    let schema = generator.into_root_schema_for::<T>();
    let mut value = serde_json::to_value(&schema).expect("a generated schema is valid JSON");
    simplify(&mut value);
    if let Value::Object(root) = &mut value {
        root.insert("title".to_string(), Value::String(title.to_string()));
        root.insert(
            "description".to_string(),
            Value::String(description.to_string()),
        );
    }
    value
}

/// The schema as a JSON value (draft-07, which editors with TOML support
/// read).
pub fn json_schema() -> Value {
    let mut value = generate::<RawConfig>(
        "florui.config.toml",
        "Configuration of one Florui application: its identity, window defaults, distribution \
         metadata and per-environment overrides.",
    );
    if let Value::Object(root) = &mut value {
        root.insert(
            "x-florui-schema-version".to_string(),
            Value::from(SUPPORTED_SCHEMA_VERSION),
        );
    }
    value
}

/// The schema of `florui.locales.toml`: a table per locale tag, such as `en`
/// or `pt-BR`, each with the application's name and description in that
/// language.
pub fn locales_json_schema() -> Value {
    generate::<BTreeMap<String, RawLocale>>(
        "florui.locales.toml",
        "The application's name and description in other languages, one table per locale tag \
         such as `en` or `pt-BR`. Used instead of `[app.locales]` in florui.config.toml; \
         declaring both is an error.",
    )
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("a generated schema is valid JSON");
    text.push('\n');
    text
}

/// The schema as the text of the committed file: two-space indentation and a
/// trailing newline.
pub fn json_schema_pretty() -> String {
    pretty(&json_schema())
}

/// The locales schema as the text of its committed file.
pub fn locales_json_schema_pretty() -> String {
    pretty(&locales_json_schema())
}

/// Rewrites what `schemars` emits into what a TOML file and an editor mean:
/// there is no `null` (an absent key is how a value is omitted), numeric
/// `format` hints are noise, hard-wrapped documentation is reflowed into
/// paragraphs, a documented enum becomes a plain enum, and the internal `Raw`
/// prefix of the table names is dropped.
///
/// A `$ref` keeps the description written next to it. Wrapping it in an
/// `allOf` so strict draft-07 readers keep that text was tried and removed:
/// Taplo's key completion overflows its stack on that shape and the language
/// server dies, while the referenced table carries its own description anyway.
fn simplify(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for child in map.values_mut() {
                simplify(child);
            }
            drop_null_from_type(map);
            drop_numeric_format(map);
            collapse_optional_any_of(map);
            reflow_description(map);
            strip_raw_prefix(map);
            flatten_documented_enum(map);
        }
        Value::Array(items) => {
            for item in items {
                simplify(item);
            }
        }
        _ => {}
    }
}

/// Doc comments are wrapped by hand; a newline inside a paragraph is a space,
/// a blank line stays a paragraph break.
fn reflow_description(map: &mut Map<String, Value>) {
    let Some(Value::String(text)) = map.get_mut("description") else {
        return;
    };
    let mut reflowed = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            if chars.peek() == Some(&'\n') {
                reflowed.push_str("\n\n");
                while chars.peek() == Some(&'\n') {
                    chars.next();
                }
            } else {
                reflowed.push(' ');
            }
        } else {
            reflowed.push(c);
        }
    }
    *text = reflowed;
}

/// An enum whose variants carry documentation is emitted as a `oneOf` of
/// constants, which an editor reports as "not valid under any of the
/// schemas" and hovers as a pile of unlabeled sentences. A plain `enum` with
/// each variant's text in `x-taplo` (which Taplo shows per value) reads
/// better and validates identically.
fn flatten_documented_enum(map: &mut Map<String, Value>) {
    let Some(Value::Array(options)) = map.get("oneOf") else {
        return;
    };
    let mut values = Vec::new();
    let mut docs = Vec::new();
    for option in options {
        let Some(object) = option.as_object() else {
            return;
        };
        let only_const_text = object
            .keys()
            .all(|key| matches!(key.as_str(), "const" | "type" | "description"));
        let Some(Value::String(value)) = object.get("const") else {
            return;
        };
        if !only_const_text || object.get("type").and_then(Value::as_str) != Some("string") {
            return;
        }
        values.push(Value::String(value.clone()));
        docs.push(
            object
                .get("description")
                .cloned()
                .unwrap_or_else(|| Value::String(String::new())),
        );
    }
    map.remove("oneOf");
    map.insert("type".to_string(), Value::String("string".to_string()));
    map.insert("enum".to_string(), Value::Array(values));
    map.insert(
        "x-taplo".to_string(),
        serde_json::json!({ "docs": { "enumValues": docs } }),
    );
}

/// Table names come from the Rust types, which carry a `Raw` prefix that
/// means nothing to someone editing the file.
fn strip_raw_prefix(map: &mut Map<String, Value>) {
    if let Some(Value::String(reference)) = map.get_mut("$ref")
        && let Some(name) = reference.strip_prefix("#/definitions/Raw")
    {
        *reference = format!("#/definitions/{name}");
    }
    if let Some(Value::Object(definitions)) = map.get_mut("definitions") {
        let renamed: Map<String, Value> = std::mem::take(definitions)
            .into_iter()
            .map(|(name, definition)| {
                let name = name
                    .strip_prefix("Raw")
                    .map_or(name.clone(), str::to_string);
                (name, definition)
            })
            .collect();
        *definitions = renamed;
    }
}

fn drop_null_from_type(map: &mut Map<String, Value>) {
    let Some(Value::Array(types)) = map.get_mut("type") else {
        return;
    };
    types.retain(|kind| kind != "null");
    if types.len() == 1 {
        let only = types.remove(0);
        map.insert("type".to_string(), only);
    }
}

fn drop_numeric_format(map: &mut Map<String, Value>) {
    let numeric = matches!(
        map.get("format").and_then(Value::as_str),
        Some("double" | "float" | "int32" | "int64" | "uint" | "uint32" | "uint64" | "uint8")
    );
    if numeric {
        map.remove("format");
    }
}

/// `anyOf: [X, { "type": "null" }]` is an optional `X`: the property is
/// simply `X`, keeping the description written next to the `anyOf`.
fn collapse_optional_any_of(map: &mut Map<String, Value>) {
    let Some(Value::Array(options)) = map.get("anyOf") else {
        return;
    };
    let is_null = |option: &Value| option.get("type").and_then(Value::as_str) == Some("null");
    if options.len() != 2 || !options.iter().any(is_null) {
        return;
    }
    let Some(kept) = options.iter().find(|option| !is_null(option)).cloned() else {
        return;
    };
    map.remove("anyOf");
    if let Value::Object(kept) = kept {
        for (key, entry) in kept {
            map.entry(key).or_insert(entry);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_optional_field_is_the_plain_type_and_the_numeric_format_is_dropped() {
        let schema = json_schema();
        let width = &schema["definitions"]["Window"]["properties"]["width"];
        assert_eq!(width["type"], "number", "{width}");
        assert!(width.get("format").is_none(), "{width}");
        let title = &schema["definitions"]["Window"]["properties"]["title"];
        assert_eq!(title["type"], "string", "{title}");
    }

    #[test]
    fn an_optional_reference_is_the_reference_with_its_description_beside_it() {
        let schema = json_schema();
        let window = &schema["properties"]["window"];
        assert_eq!(window["$ref"], "#/definitions/Window", "{window}");
        assert!(window["description"].as_str().is_some(), "{window}");
        assert!(window.get("anyOf").is_none(), "{window}");
    }

    #[test]
    fn no_reference_is_wrapped_in_an_all_of() {
        // Taplo's key completion overflows its stack on `allOf: [{ "$ref" }]`.
        fn walk(value: &Value, found: &mut Vec<String>) {
            match value {
                Value::Object(map) => {
                    if let Some(Value::Array(all)) = map.get("allOf")
                        && all.iter().any(|entry| entry.get("$ref").is_some())
                    {
                        found.push(value.to_string());
                    }
                    map.values().for_each(|child| walk(child, found));
                }
                Value::Array(items) => items.iter().for_each(|child| walk(child, found)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        walk(&json_schema(), &mut found);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn a_documented_enum_is_a_plain_enum_with_its_texts_beside_it() {
        let schema = json_schema();
        let decorations = &schema["definitions"]["Decorations"];
        assert_eq!(decorations["enum"], serde_json::json!(["system", "custom"]));
        assert!(decorations.get("oneOf").is_none(), "{decorations}");
        let docs = decorations["x-taplo"]["docs"]["enumValues"]
            .as_array()
            .unwrap();
        assert_eq!(docs.len(), 2);
        assert!(
            docs.iter()
                .all(|doc| doc.as_str().is_some_and(|text| !text.is_empty()))
        );
    }

    #[test]
    fn the_schema_names_the_file_format_version_it_describes() {
        let schema = json_schema();
        assert_eq!(schema["x-florui-schema-version"], SUPPORTED_SCHEMA_VERSION);
        assert_eq!(
            schema["properties"]["schema_version"]["const"],
            SUPPORTED_SCHEMA_VERSION
        );
        assert!(schema.get("$id").is_none(), "no URL is published");
    }
}
