//! Reference-comparison harness: loads a fixture pairing an HTML/CSS
//! reference with Florui's own minimal render path, drives Chromium to
//! capture the reference, and compares pixels and geometry between the two.
//!
//! Development/test tooling only — never a dependency of a public Florui
//! runtime crate.

pub mod driver;
pub mod engine;
pub mod geometry;
pub mod pin;
pub mod pixels;
pub mod reference_fixture;
pub mod regions;
pub mod report;
pub mod run_history;
