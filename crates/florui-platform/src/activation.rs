//! Opt-in single-instance detection plus the bounded queue of typed
//! activation events (ordinary launch, URL open, file open) a second
//! launch hands to the already-running process. Pure/testable types and
//! logic only -- see `crate::os::windows::single_instance` for the real
//! named-mutex/named-pipe mechanics this builds on.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use florui_reactive::use_context;
use serde::{Deserialize, Serialize};

/// The plain, already-resolved values an app passes in to opt into
/// single-instance behavior -- deliberately not `florui-config`'s own
/// `ActivationConfig`, the same reasoning as `WindowPersistence`: this
/// crate never gains a `florui-config` dependency, so the app resolves it
/// and hands over a plain string.
#[derive(Debug, Clone, PartialEq)]
pub struct SingleInstance {
    pub app_identifier: String,
    /// How long a second launch waits for the primary to acknowledge a
    /// handed-off activation event before giving up.
    pub handoff_timeout: Duration,
}

impl Default for SingleInstance {
    fn default() -> Self {
        Self {
            app_identifier: String::new(),
            handoff_timeout: Duration::from_secs(2),
        }
    }
}

/// A typed activation payload -- never a shell-joined string. `Launch`
/// carries the second process's own `argv` (its first element still the
/// executable path, matching `std::env::args()`) so the primary can
/// re-parse it exactly as it would have parsed its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ActivationEvent {
    Launch { args: Vec<String> },
    OpenUrl { url: String },
    OpenFiles { paths: Vec<PathBuf> },
}

/// Classifies a process's own `argv` (`args[0]` still the executable path)
/// into a typed [`ActivationEvent`], against the app's own declared
/// `url_schemes`/`file_extensions` -- already-resolved plain values, the
/// same "app resolves `florui-config` itself, hands over plain data"
/// pattern [`SingleInstance`] uses, so this crate still never depends on
/// `florui-config`. A single trailing argument whose scheme matches a
/// declared one becomes [`ActivationEvent::OpenUrl`]; one or more trailing
/// arguments that *all* match a declared extension become
/// [`ActivationEvent::OpenFiles`]. Anything else -- no trailing arguments,
/// an unrecognized scheme/extension, or a mix of the two -- stays
/// [`ActivationEvent::Launch`], since it's an ordinary argument this crate
/// has no business reinterpreting. Matching is case-insensitive on both
/// sides, mirroring how `florui-config` itself dedupes a declared scheme
/// or extension.
///
/// Never touches the filesystem or a URL's own validity beyond its
/// scheme -- classification only says what *kind* of activation this is,
/// not that the payload is safe to open; the association itself grants no
/// filesystem permission, per the same rule `florui-config`'s own schema
/// documents.
pub fn classify_launch(
    args: Vec<String>,
    url_schemes: &[String],
    file_extensions: &[String],
) -> ActivationEvent {
    let trailing = &args[1.min(args.len())..];
    if let [single] = trailing
        && let Some(scheme) = url_scheme(single)
        && url_schemes
            .iter()
            .any(|declared| declared.eq_ignore_ascii_case(scheme))
    {
        return ActivationEvent::OpenUrl {
            url: single.clone(),
        };
    }
    if !trailing.is_empty()
        && trailing
            .iter()
            .all(|arg| matches_known_extension(arg, file_extensions))
    {
        return ActivationEvent::OpenFiles {
            paths: trailing.iter().map(PathBuf::from).collect(),
        };
    }
    ActivationEvent::Launch { args }
}

/// The scheme portion of `value` (before `://`), only if that scheme is
/// well-formed per RFC 3986 (a letter, then letters/digits/`+`/`-`/`.`) --
/// the exact rule `florui-config` itself validates a declared scheme
/// against, so a runtime value is held to the same standard a config
/// author already is.
fn url_scheme(value: &str) -> Option<&str> {
    let (scheme, _rest) = value.split_once("://")?;
    let mut chars = scheme.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return None,
    }
    chars
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        .then_some(scheme)
}

fn matches_known_extension(arg: &str, file_extensions: &[String]) -> bool {
    let Some(extension) = Path::new(arg).extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    file_extensions
        .iter()
        .any(|declared| declared.eq_ignore_ascii_case(extension))
}

/// Named-object charset for a Windows mutex/pipe name is broad, but kept
/// deliberately narrow here (matches what an `app.identifier` already
/// looks like -- reverse-DNS-style, see `florui-config`'s own schema) so
/// a stray character in a user-supplied identifier can never produce an
/// invalid or surprising object name.
#[cfg(any(windows, test))]
fn sanitize_identifier(app_identifier: &str) -> String {
    app_identifier
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `Local\` already scopes a named kernel object to the caller's own
/// logon session (Windows transparently redirects an unprefixed or
/// `Local\`-prefixed name through `\Sessions\<n>\BaseNamedObjects`; only
/// `Global\` escapes that) -- so this needs no separate session-id
/// component, the same "already per-user by construction" shape
/// `window_state.rs` documents for `LOCALAPPDATA`. Distinct dev/production
/// identifiers (already how `WindowPersistence` avoids collisions) produce
/// distinct names for free.
#[cfg(any(windows, test))]
pub(crate) fn mutex_name(app_identifier: &str) -> String {
    format!(
        "Local\\florui-single-instance-{}",
        sanitize_identifier(app_identifier)
    )
}

/// Named pipes resolve through the same per-session object namespace as a
/// named mutex, so this needs no separate session scoping either.
#[cfg(any(windows, test))]
pub(crate) fn pipe_name(app_identifier: &str) -> String {
    format!(
        "\\\\.\\pipe\\florui-single-instance-{}",
        sanitize_identifier(app_identifier)
    )
}

/// How many not-yet-delivered activation events [`ActivationQueue`] holds
/// before it starts dropping the oldest -- bounds memory if a primary is
/// slow to mount its first window while several launches race in.
const MAX_QUEUED_EVENTS: usize = 16;

/// Activation events that arrived (from [`crate::os::windows::single_instance`]'s
/// pipe listener) before -- or between renders of -- the window that reads
/// them. Bounded: a `push` past [`MAX_QUEUED_EVENTS`] drops the oldest
/// queued event and logs a diagnostic, the same non-fatal-on-overflow
/// treatment this crate already gives corrupted window state.
#[derive(Debug, Default)]
pub(crate) struct ActivationQueue {
    events: VecDeque<ActivationEvent>,
}

impl ActivationQueue {
    pub(crate) fn push(&mut self, event: ActivationEvent) {
        if self.events.len() >= MAX_QUEUED_EVENTS {
            self.events.pop_front();
            eprintln!(
                "florui-platform: activation event queue full ({MAX_QUEUED_EVENTS}), dropping the oldest pending event"
            );
        }
        self.events.push_back(event);
    }

    pub(crate) fn take_pending(&mut self) -> Vec<ActivationEvent> {
        self.events.drain(..).collect()
    }
}

/// Reachable from the primary window's component tree via
/// [`use_activation_events`] from that window's very first render onward
/// -- see [`crate::desktop::DesktopHost`]'s own doc for why only the
/// primary window gets this context.
#[derive(Debug, Clone)]
pub struct ActivationEvents(pub(crate) Rc<RefCell<ActivationQueue>>);

impl ActivationEvents {
    /// Every event queued since the last call, oldest first. Draining
    /// (not peeking) is deliberate: this is the one window that owns this
    /// queue, so nothing else is left to read the same event twice.
    pub fn take_pending(&self) -> Vec<ActivationEvent> {
        self.0.borrow_mut().take_pending()
    }
}

/// Reads the [`ActivationEvents`] [`crate::desktop::DesktopHost`] provides
/// to the primary window every render. `None` outside a real desktop host,
/// or in any window other than the primary one, or when single-instance
/// activation was never opted into.
pub fn use_activation_events() -> Option<ActivationEvents> {
    use_context::<ActivationEvents>()
}

/// Real, observed evidence -- not an assumption from the OS merely being
/// Windows -- that the named-mutex and named-pipe mechanisms
/// [`crate::run_single_instance`] depends on actually work on this
/// machine. `false` on any other platform, since no equivalent mechanism
/// exists there yet. Used by `florui doctor` to report actual capability
/// rather than assuming it; leaves nothing behind either way.
pub fn probe_single_instance_capability() -> bool {
    crate::single_instance::probe_capability()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(rest: &[&str]) -> Vec<String> {
        std::iter::once("app.exe".to_owned())
            .chain(rest.iter().map(|s| s.to_string()))
            .collect()
    }

    #[test]
    fn no_trailing_arguments_is_a_plain_launch() {
        let event = classify_launch(args(&[]), &["florui".to_owned()], &["garden".to_owned()]);
        assert_eq!(event, ActivationEvent::Launch { args: args(&[]) });
    }

    #[test]
    fn ordinary_flags_stay_a_plain_launch() {
        let event = classify_launch(
            args(&["--flag", "value"]),
            &["florui".to_owned()],
            &["garden".to_owned()],
        );
        assert_eq!(
            event,
            ActivationEvent::Launch {
                args: args(&["--flag", "value"])
            }
        );
    }

    #[test]
    fn a_declared_scheme_becomes_open_url() {
        let event = classify_launch(args(&["florui://open?id=42"]), &["florui".to_owned()], &[]);
        assert_eq!(
            event,
            ActivationEvent::OpenUrl {
                url: "florui://open?id=42".to_owned()
            }
        );
    }

    #[test]
    fn scheme_matching_is_case_insensitive_on_both_sides() {
        let event = classify_launch(args(&["FLORUI://open?id=42"]), &["Florui".to_owned()], &[]);
        assert_eq!(
            event,
            ActivationEvent::OpenUrl {
                url: "FLORUI://open?id=42".to_owned()
            }
        );
    }

    #[test]
    fn an_undeclared_scheme_stays_a_plain_launch() {
        let event = classify_launch(args(&["other://open?id=42"]), &["florui".to_owned()], &[]);
        assert_eq!(
            event,
            ActivationEvent::Launch {
                args: args(&["other://open?id=42"])
            }
        );
    }

    #[test]
    fn malformed_uri_like_text_stays_a_plain_launch() {
        let event = classify_launch(args(&["not a url"]), &["florui".to_owned()], &[]);
        assert_eq!(
            event,
            ActivationEvent::Launch {
                args: args(&["not a url"])
            }
        );
    }

    #[test]
    fn a_single_matching_file_becomes_open_files() {
        let event = classify_launch(
            args(&["C:\\docs\\plan.garden"]),
            &[],
            &["garden".to_owned()],
        );
        assert_eq!(
            event,
            ActivationEvent::OpenFiles {
                paths: vec![PathBuf::from("C:\\docs\\plan.garden")]
            }
        );
    }

    #[test]
    fn multiple_matching_files_all_become_open_files() {
        let event = classify_launch(args(&["a.garden", "b.garden"]), &[], &["garden".to_owned()]);
        assert_eq!(
            event,
            ActivationEvent::OpenFiles {
                paths: vec![PathBuf::from("a.garden"), PathBuf::from("b.garden")]
            }
        );
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        let event = classify_launch(args(&["plan.GARDEN"]), &[], &["garden".to_owned()]);
        assert_eq!(
            event,
            ActivationEvent::OpenFiles {
                paths: vec![PathBuf::from("plan.GARDEN")]
            }
        );
    }

    #[test]
    fn a_mix_of_matching_and_unmatching_files_stays_a_plain_launch() {
        let event = classify_launch(args(&["a.garden", "b.txt"]), &[], &["garden".to_owned()]);
        assert_eq!(
            event,
            ActivationEvent::Launch {
                args: args(&["a.garden", "b.txt"])
            }
        );
    }

    #[test]
    fn a_file_argument_with_no_extension_stays_a_plain_launch() {
        let event = classify_launch(args(&["README"]), &[], &["garden".to_owned()]);
        assert_eq!(
            event,
            ActivationEvent::Launch {
                args: args(&["README"])
            }
        );
    }

    #[test]
    fn no_declared_schemes_or_extensions_never_classifies_anything() {
        let event = classify_launch(args(&["florui://open", "a.garden"]), &[], &[]);
        assert_eq!(
            event,
            ActivationEvent::Launch {
                args: args(&["florui://open", "a.garden"])
            }
        );
    }

    #[test]
    fn mutex_and_pipe_names_are_scoped_to_local_session_namespace() {
        assert!(mutex_name("com.floregreen.garden").starts_with("Local\\"));
        assert!(pipe_name("com.floregreen.garden").starts_with("\\\\.\\pipe\\"));
    }

    #[test]
    fn dev_and_production_identifiers_never_collide() {
        let prod = mutex_name("com.floregreen.garden");
        let dev = mutex_name("com.floregreen.garden.dev");
        assert_ne!(prod, dev);
        let prod_pipe = pipe_name("com.floregreen.garden");
        let dev_pipe = pipe_name("com.floregreen.garden.dev");
        assert_ne!(prod_pipe, dev_pipe);
    }

    #[test]
    fn sanitize_identifier_replaces_disallowed_characters() {
        assert_eq!(
            sanitize_identifier("com.floregreen.garden"),
            "com.floregreen.garden"
        );
        assert_eq!(
            sanitize_identifier("weird/name\\with:chars"),
            "weird_name_with_chars"
        );
    }

    #[test]
    fn activation_event_round_trips_through_json() {
        let events = [
            ActivationEvent::Launch {
                args: vec!["app.exe".to_owned(), "--flag".to_owned()],
            },
            ActivationEvent::OpenUrl {
                url: "florui://open?id=42".to_owned(),
            },
            ActivationEvent::OpenFiles {
                paths: vec![PathBuf::from("C:\\a b\\c.txt")],
            },
        ];
        for event in events {
            let json = serde_json::to_vec(&event).unwrap();
            let decoded: ActivationEvent = serde_json::from_slice(&json).unwrap();
            assert_eq!(decoded, event);
        }
    }

    #[test]
    fn queue_drains_in_fifo_order() {
        let mut queue = ActivationQueue::default();
        queue.push(ActivationEvent::OpenUrl {
            url: "a".to_owned(),
        });
        queue.push(ActivationEvent::OpenUrl {
            url: "b".to_owned(),
        });
        let drained = queue.take_pending();
        assert_eq!(
            drained,
            vec![
                ActivationEvent::OpenUrl {
                    url: "a".to_owned()
                },
                ActivationEvent::OpenUrl {
                    url: "b".to_owned()
                },
            ]
        );
        assert!(queue.take_pending().is_empty());
    }

    #[test]
    fn queue_drops_oldest_once_it_is_full() {
        let mut queue = ActivationQueue::default();
        for i in 0..MAX_QUEUED_EVENTS + 3 {
            queue.push(ActivationEvent::OpenUrl { url: i.to_string() });
        }
        let drained = queue.take_pending();
        assert_eq!(drained.len(), MAX_QUEUED_EVENTS);
        assert_eq!(
            drained.first(),
            Some(&ActivationEvent::OpenUrl {
                url: "3".to_owned()
            })
        );
        assert_eq!(
            drained.last(),
            Some(&ActivationEvent::OpenUrl {
                url: (MAX_QUEUED_EVENTS + 2).to_string()
            })
        );
    }
}
