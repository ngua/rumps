//! Integration tests for `.rumps` scripts.
//!
//! Uses `datatest-stable` to discover scripts and `insta` for snapshot testing.

use std::path::Path;

use rumps_query::run_capturing;
use rumps_storage::Database;

fn run_script(path: &Path) -> datatest_stable::Result<()> {
    let abs_path = path.canonicalize()?;
    let src = std::fs::read_to_string(&abs_path)?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let output = rt.block_on(async {
        let db = match Database::in_memory() {
            Ok(db) => db,
            Err(e) => return format!("ERROR: {e}"),
        };
        match run_capturing(&src, &abs_path, db).await {
            Ok(out) => out,
            Err(e) => format!("ERROR: {e}"),
        }
    });

    insta::with_settings!({
        description => &src,
        omit_expression => true,
        snapshot_path => "../snapshots",
    }, {
        insta::assert_snapshot!(name, output);
    });

    Ok(())
}

datatest_stable::harness!(run_script, "scripts", r".*\.rumps$");
