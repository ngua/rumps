//! Build script for rumps-storage.
//!
//! Reads `RUMPS_PAGE_SIZE` environment variable at compile time to configure
//! the page size. Defaults to `4096` if not set.

fn main() {
    println!("cargo::rerun-if-env-changed=RUMPS_PAGE_SIZE");

    let page_size: usize = std::env::var("RUMPS_PAGE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4096);

    assert!(
        page_size.is_power_of_two(),
        "RUMPS_PAGE_SIZE must be a power of two, got {page_size}"
    );

    println!("cargo::rustc-env=RUMPS_PAGE_SIZE={page_size}");
}
