//! Medical database example: loads CSV data into a RUMPS database.
//!
//! This example demonstrates populating a RUMPS database from CSV data.
//! The CSV format is `global,path,value` where `path` can be comma-separated.
//!
//! Run with: `cargo run --example medical`

use std::path::Path;

use rumps::{Database, Key, Name, Result, Subscript, Value};

/// A parsed CSV row ready for insertion.
struct Entry {
    global: Name,
    key: Key,
    val: Value,
}

/// Parses a path string like `"1,VISIT,2"` or `"1"` into subscripts.
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

#[tokio::main]
async fn main() -> Result<()> {
    let csv_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("medical-db.csv");

    let db_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("medical.db");

    println!("Loading CSV from: {}", csv_path.display());
    let entries = load_csv(&csv_path)?;
    println!("Parsed {} entries", entries.len());

    // Remove existing DB if present (for idempotent runs)
    if db_path.exists() {
        std::fs::remove_dir_all(&db_path)
            .map_err(rumps::StorageError::IoGeneric)?;
    }

    println!("Creating database at: {}", db_path.display());
    let db = Database::create(&db_path).await?;

    // Insert all entries in a single transaction
    db.transaction(|txn| {
        async fn insert_all(
            txn: &rumps::Transaction,
            entries: &[Entry],
        ) -> Result<()> {
            match entries.split_first() {
                None => Ok(()),
                Some((e, rest)) => {
                    txn.set(&e.global, &e.key, e.val.clone()).await?;
                    Box::pin(insert_all(txn, rest)).await
                }
            }
        }
        async move { insert_all(&txn, &entries).await }
    })
    .await?;

    // Print summary
    let globals = db.list_globals().await;
    println!("\nInserted data into {} globals:", globals.len());
    globals.iter().for_each(|g| println!("  ^{}", g));

    db.close().await?;
    println!("\nDatabase closed successfully.");

    Ok(())
}
