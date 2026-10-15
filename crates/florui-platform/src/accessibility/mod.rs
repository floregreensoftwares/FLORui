//! Everything accessibility: real OS preference reads (this file) and the
//! real AccessKit tree bridge ([`tree`], desktop-only).

#[cfg(target_os = "windows")]
pub use crate::os::windows::accessibility::prefers_reduced_motion;

#[cfg(target_os = "linux")]
pub use crate::os::linux::accessibility::prefers_reduced_motion;

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub fn prefers_reduced_motion() -> bool {
    false
}

#[cfg(feature = "desktop")]
pub(crate) mod tree;
