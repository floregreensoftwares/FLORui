//! What an application declares to be registered, and the rules every
//! operating system's registration shares: which schemes are never taken, and
//! which names are safe to put in a file or key name.

use serde::Serialize;

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
    /// The declared MIME type, when there is one.
    pub mime_type: Option<String>,
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

pub fn safe_segment(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
}

/// Why a registration cannot be written as asked.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Conflict {
    pub what: String,
    pub reason: String,
}
