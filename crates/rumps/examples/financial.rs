//! Financial database example: loads CSV data into a RUMPS database.
//!
//! This example demonstrates the `open_override` API for batch imports.
//! It creates a database with default settings, closes it, then re-opens
//! with `SyncMode::Relaxed` for faster bulk inserts.
//!
//! Run with: `cargo run --example financial --features examples`

use std::path::Path;

use futures::stream::{self, StreamExt, TryStreamExt};
use rumps::{Database, Key, Name, Result, Subscript, SyncMode, Value};

/// A parsed CSV row ready for insertion.
struct Entry {
    global: Name,
    key: Key,
    val: Value,
}

/// Parses a path string like `"ACCT000001,BAL"` or `"ACCT000001"` into subscripts.
fn parse_path(path: &str) -> Key {
    path.split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<i64>()
                .map(Subscript::from)
                .unwrap_or_else(|_| Subscript::from(s))
        })
        .collect()
}

/// Parses a CSV row into an `Entry`.
fn parse_row(rec: &csv::StringRecord) -> Option<Entry> {
    let global_name = rec.get(0)?;
    let path = rec.get(1)?;
    let val = rec.get(2)?;

    Some(Entry {
        global: Name::global(global_name),
        key: parse_path(path),
        val: Value::from(val),
    })
}

/// Loads entries from the CSV file.
fn load_csv(path: &Path) -> Result<Vec<Entry>> {
    let mut rdr = csv::Reader::from_path(path).map_err(|e| {
        rumps::StorageError::InvalidConfiguration(e.to_string())
    })?;

    rdr.records()
        .filter_map(|r| r.ok())
        .filter_map(|rec| parse_row(&rec))
        .map(Ok)
        .collect()
}

// This is somewhat contrived, but demonstrates:
//   - Creating a DB with default settings; in this case, the important
//     setting is WAL sync mode, which defaults to `SyncMode::OnCommit`
//   - Re=opening the DB with overridden settings for this session only; here
//     it's using `SyncMode::Relaxed` for a batch import
#[tokio::main]
async fn main() -> Result<()> {
    let csv_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("financial-db.csv");

    let db_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("financial.db");

    println!("Loading CSV from: {}", csv_path.display());
    let entries = load_csv(&csv_path)?;
    println!("Parsed {} entries", entries.len());

    // Remove existing DB if present (for idempotent runs)
    if db_path.exists() {
        std::fs::remove_dir_all(&db_path)
            .map_err(rumps::StorageError::IoGeneric)?;
    }

    // Step 1: Create DB with default settings
    println!("\nCreating database at: {}", db_path.display());
    let db = Database::create(&db_path).await?;
    db.close().await?;
    println!("Database created and closed.");

    // Step 2: Re-open with `SyncMode::Relaxed` for batch import
    println!("\nRe-opening with `SyncMode::Relaxed` for batch import...");
    let db = Database::open_override(&db_path)
        .sync_mode(SyncMode::Relaxed)
        .open()
        .await?;

    // Step 3: Insert all entries in a single transaction
    db.transaction(|txn| async move {
        stream::iter(entries.iter())
            .map(Ok)
            .try_for_each(|e| async {
                txn.set(&e.global, &e.key, e.val.clone()).await
            })
            .await
    })
    .await?;

    // Print summary
    let globals = db.list_globals().await;
    println!("\nInserted data into {} globals:", globals.len());
    globals.iter().for_each(|g| println!("  ^{}", g));

    db.close().await?;
    println!("\nOverride database closed successfully.");

    Ok(())
}
