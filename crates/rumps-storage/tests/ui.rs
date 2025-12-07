//! Compile-fail tests for derive macro error messages.
//!
//! These tests verify that invalid derive macro usage produces helpful
//! compile-time errors.
//!
//! # Environment Variables
//!
//! ## `RUMPS_SKIP_UI_TESTS`
//!
//! Set to any value (e.g., `RUMPS_SKIP_UI_TESTS=1`) to skip these tests.
//!
//! This is useful for Nix sandbox builds where `trybuild` may not work
//! correctly due to restricted filesystem access and subprocess spawning.
//! In such environments, the `trybuild` crate may not be able to invoke
//! `cargo`/`rustc` as it normally could. However, we don't want this to block
//! using `doCheck` in package nuilds
//!
//! Example Naersk usage:
//! ```nix
//! naersk.buildPackage {
//!   src = ./.;
//!   doCheck = true;
//!   RUMPS_SKIP_UI_TESTS = "1";
//! }
//! ```

#![cfg(feature = "derive")]

#[test]
fn ui_tests() {
    if std::env::var("RUMPS_SKIP_UI_TESTS").is_ok() {
        eprintln!("Skipping UI tests (RUMPS_SKIP_UI_TESTS is set)");
    } else {
        trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
    }
}
