//! Real, per-OS platform code — one directory per target OS, mirroring
//! `std::os`. `florui-platform`'s own OS-agnostic modules dispatch into
//! these; nothing here should be imported directly outside this crate.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "windows")]
pub mod windows;
