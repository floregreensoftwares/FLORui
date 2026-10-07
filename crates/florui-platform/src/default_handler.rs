//! Asking to be the default handler of a URL scheme or a file type.
//!
//! The application states the intent in its configuration
//! (`[app.activation] request_default`) and, at run time, asks. Asking never
//! sets anything: it opens the operating system's own settings page for this
//! application, where the user decides. An application that did not declare
//! the request, or that is not registered with the system (`florui register`),
//! is refused before anything is opened.
//!
//! Like [`crate::SingleInstance`], this takes plain, already-resolved values,
//! so this crate never depends on `florui-config`.
//!
//! The registry layout of a registration is a contract between `florui
//! register`, which writes it, and this module, which looks it up; the names
//! of those keys live here, once.

/// Where Windows lists the applications that offer themselves as handlers
/// (value name: the application's identifier, data: its capabilities key).
pub const REGISTERED_APPLICATIONS_KEY: &str = "Software\\RegisteredApplications";

/// The application's own key, which also holds the record of a registration.
pub fn application_key(identifier: &str) -> String {
    format!("Software\\Florui\\{identifier}")
}

/// The capabilities key (`ApplicationName`, `FileAssociations`, ...) Windows
/// shows the application from.
pub fn capabilities_key(identifier: &str) -> String {
    format!("{}\\Capabilities", application_key(identifier))
}

/// A scheme or file type an application may ask to be the default of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultTarget {
    /// A declared URL scheme, as written there.
    UrlScheme(String),
    /// A declared file extension, without the dot.
    FileExtension(String),
}

impl DefaultTarget {
    fn matches(&self, other: &DefaultTarget) -> bool {
        match (self, other) {
            (DefaultTarget::UrlScheme(a), DefaultTarget::UrlScheme(b))
            | (DefaultTarget::FileExtension(a), DefaultTarget::FileExtension(b)) => {
                a.eq_ignore_ascii_case(b)
            }
            _ => false,
        }
    }
}

/// What the application declared it may ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultHandlerPermission {
    /// The application's identifier, which names its registration.
    pub identifier: String,
    pub allowed: Vec<DefaultTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultHandlerOutcome {
    /// The system's settings page for this application is open; the user
    /// chooses there. This does not say they chose this application.
    Opened,
    /// The application did not declare this request.
    NotDeclared,
    /// The application is not registered with the system, so there is
    /// nothing for the user to choose.
    NotRegistered,
    /// This platform has no such settings page to open.
    Unsupported,
    Failed(String),
}

/// Asks the user, through the system's own settings, to make this application
/// the default for `wanted`. Never sets a default itself.
pub fn request_default_handler(
    permission: &DefaultHandlerPermission,
    wanted: &DefaultTarget,
) -> DefaultHandlerOutcome {
    if !permission
        .allowed
        .iter()
        .any(|allowed| allowed.matches(wanted))
    {
        return DefaultHandlerOutcome::NotDeclared;
    }
    os::open_default_apps(&permission.identifier)
}

/// The settings page that lists `application_name`'s defaults.
pub(crate) fn settings_uri(application_name: &str) -> String {
    let mut encoded = String::new();
    for byte in application_name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("ms-settings:defaultapps?registeredAppUser={encoded}")
}

#[cfg(all(windows, feature = "desktop"))]
mod os {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::HSTRING;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, REG_SZ, RegCloseKey, RegOpenKeyExW, RegQueryValueExW,
    };

    use super::{DefaultHandlerOutcome, capabilities_key, settings_uri};

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    /// A string value of the current user's registry.
    fn read(key: &str, name: &str) -> Option<String> {
        let mut handle: HKEY = std::ptr::null_mut();
        // SAFETY: `key` is a NUL-terminated buffer that outlives the call and
        // `handle` is a valid out pointer; the handle is closed below.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(key).as_ptr(),
                0,
                KEY_READ,
                &mut handle,
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let name = wide(name);
        let mut kind = 0u32;
        let mut size = 0u32;
        // SAFETY: the handle is open for reading; the first call asks only
        // for the size, the second fills a buffer of exactly that many bytes;
        // the handle is closed before returning.
        let value = unsafe {
            let first = RegQueryValueExW(
                handle,
                name.as_ptr(),
                std::ptr::null(),
                &mut kind,
                std::ptr::null_mut(),
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
                std::ptr::null(),
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

    pub(super) fn open_default_apps(identifier: &str) -> DefaultHandlerOutcome {
        if read(super::REGISTERED_APPLICATIONS_KEY, identifier).is_none() {
            return DefaultHandlerOutcome::NotRegistered;
        }
        let name = read(&capabilities_key(identifier), "ApplicationName")
            .unwrap_or(identifier.to_string());
        let operation = HSTRING::from("open");
        let uri = HSTRING::from(settings_uri(&name));
        // SAFETY: asks the shell to open a settings page; `operation` and
        // `uri` are live `HSTRING`s for the whole call, and `ShellExecuteW`
        // does not retain either pointer past it.
        let result = unsafe { ShellExecuteW(None, &operation, &uri, None, None, SW_SHOWNORMAL) };
        // `ShellExecute`'s own contract: a value above 32 is success; the type
        // is a repurposed `HINSTANCE`, so anything at or below it is an error code.
        let code = result.0 as isize;
        if code > 32 {
            DefaultHandlerOutcome::Opened
        } else {
            DefaultHandlerOutcome::Failed(format!("ShellExecuteW returned {code}"))
        }
    }
}

#[cfg(not(all(windows, feature = "desktop")))]
mod os {
    use super::DefaultHandlerOutcome;

    pub(super) fn open_default_apps(_identifier: &str) -> DefaultHandlerOutcome {
        DefaultHandlerOutcome::Unsupported
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permission() -> DefaultHandlerPermission {
        DefaultHandlerPermission {
            identifier: "com.example.garden".to_string(),
            allowed: vec![
                DefaultTarget::UrlScheme("garden".to_string()),
                DefaultTarget::FileExtension("garden".to_string()),
            ],
        }
    }

    #[test]
    fn a_request_that_was_not_declared_is_refused_before_anything_is_opened() {
        let outcome = |target| request_default_handler(&permission(), &target);
        assert_eq!(
            outcome(DefaultTarget::UrlScheme("other".to_string())),
            DefaultHandlerOutcome::NotDeclared
        );
        assert_eq!(
            outcome(DefaultTarget::FileExtension("other".to_string())),
            DefaultHandlerOutcome::NotDeclared
        );
        // A scheme and a file type with the same name are different requests.
        let only_scheme = DefaultHandlerPermission {
            allowed: vec![DefaultTarget::UrlScheme("garden".to_string())],
            ..permission()
        };
        assert_eq!(
            request_default_handler(
                &only_scheme,
                &DefaultTarget::FileExtension("garden".to_string())
            ),
            DefaultHandlerOutcome::NotDeclared
        );
    }

    #[test]
    fn matching_a_declared_request_ignores_case_and_asks_the_system() {
        // An application that is not registered cannot be chosen, so asking
        // never reaches the settings page.
        let unregistered = DefaultHandlerPermission {
            identifier: "com.floruitest.never-registered".to_string(),
            ..permission()
        };
        let outcome = request_default_handler(
            &unregistered,
            &DefaultTarget::UrlScheme("GARDEN".to_string()),
        );
        assert!(
            matches!(
                outcome,
                DefaultHandlerOutcome::NotRegistered | DefaultHandlerOutcome::Unsupported
            ),
            "{outcome:?}"
        );
    }

    #[test]
    fn the_settings_page_names_the_application_encoded_as_a_query_value() {
        assert_eq!(
            settings_uri("Garden"),
            "ms-settings:defaultapps?registeredAppUser=Garden"
        );
        assert_eq!(
            settings_uri("Meu Jardim & Cia ação"),
            "ms-settings:defaultapps?registeredAppUser=Meu%20Jardim%20%26%20Cia%20a%C3%A7%C3%A3o"
        );
    }

    #[test]
    fn the_registry_layout_is_named_once() {
        assert_eq!(
            application_key("com.example.garden"),
            "Software\\Florui\\com.example.garden"
        );
        assert_eq!(
            capabilities_key("com.example.garden"),
            "Software\\Florui\\com.example.garden\\Capabilities"
        );
    }
}
