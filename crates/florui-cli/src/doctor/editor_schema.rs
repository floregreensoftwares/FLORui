//! `config.editor_schema`: whether the editor schema a project's
//! `florui.config.toml` points at is the one this `florui` generates. The
//! schema is a local copy (no URL is published), so it goes stale when Florui
//! is upgraded; this says so, offline, without changing anything.

use std::path::Path;

use super::{Check, Status, display_redacted};

const ID: &str = "config.editor_schema";

fn check(status: Status, evidence: String, remediation: Option<&str>) -> Check {
    Check {
        id: ID,
        category: "config",
        status,
        required: false,
        observed: None,
        expected: None,
        evidence,
        reason: None,
        remediation: remediation.map(str::to_string),
    }
}

/// The target of a `#:schema <target>` directive among the comment lines at
/// the top of `text`, the way Taplo and Even Better TOML read it.
pub(super) fn schema_directive(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .take_while(|line| line.is_empty() || line.starts_with('#'))
        .find_map(|line| line.strip_prefix("#:schema"))
        .map(str::trim)
        .filter(|target| !target.is_empty())
}

fn normalized(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// `config_dir` is where `florui.config.toml` is; a relative target is
/// relative to it, as an editor resolves it.
pub(super) fn editor_schema_check(config_exists: bool, config_dir: &Path) -> Check {
    if !config_exists {
        return check(
            Status::NotApplicable,
            "no florui.config.toml, so no editor schema to associate".to_string(),
            None,
        );
    }
    let config_path = florui_config::config_file_path(config_dir);
    let Ok(config) = std::fs::read_to_string(&config_path) else {
        return check(
            Status::Unknown,
            "florui.config.toml could not be read".to_string(),
            None,
        );
    };
    let how_to = "run `florui schema --write` and start florui.config.toml with \
                  `#:schema ./florui.config.schema.json`";
    let Some(target) = schema_directive(&config) else {
        return check(
            Status::NotApplicable,
            "florui.config.toml has no `#:schema` line, so an editor reads no schema for it"
                .to_string(),
            Some(how_to),
        );
    };
    if target.contains("://") && !target.starts_with("file://") {
        return check(
            Status::NotApplicable,
            format!(
                "the `#:schema` line points at {target}, which is not read: doctor makes no \
                 network requests"
            ),
            None,
        );
    }
    let local = target.strip_prefix("file://").unwrap_or(target);
    let path = config_dir.join(local.strip_prefix("./").unwrap_or(local));
    let shown = display_redacted(&path);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) => {
            return check(
                Status::Warning,
                format!("the `#:schema` line points at {shown}, which could not be read: {error}"),
                Some("run `florui schema --write` to create it"),
            );
        }
    };
    if normalized(&text) == normalized(&florui_config::json_schema_pretty()) {
        check(
            Status::Pass,
            format!("{shown} is the schema this florui generates"),
            None,
        )
    } else {
        check(
            Status::Warning,
            format!(
                "{shown} differs from the schema this florui generates: it was written by \
                 another version of florui, or edited, so an editor may flag valid keys or miss \
                 new ones"
            ),
            Some("run `florui schema --write` to refresh it"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_directive_is_read_from_the_comment_lines_at_the_top() {
        assert_eq!(
            schema_directive("#:schema ./florui.config.schema.json\nschema_version = 1\n"),
            Some("./florui.config.schema.json")
        );
        assert_eq!(
            schema_directive("# Garden\n\n#:schema   ./s.json  \nschema_version = 1\n"),
            Some("./s.json")
        );
        assert_eq!(
            schema_directive("#:schema https://example.com/s.json\n"),
            Some("https://example.com/s.json")
        );
    }

    #[test]
    fn a_directive_after_the_first_key_or_without_a_target_is_not_one() {
        assert_eq!(
            schema_directive("schema_version = 1\n#:schema ./s.json\n"),
            None
        );
        assert_eq!(schema_directive("#:schema\nschema_version = 1\n"), None);
        assert_eq!(
            schema_directive("# just a comment\nschema_version = 1\n"),
            None
        );
        assert_eq!(schema_directive(""), None);
    }

    #[test]
    fn a_missing_config_is_not_applicable() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            editor_schema_check(false, dir.path()).status,
            Status::NotApplicable
        );
    }
}
