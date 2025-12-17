//! RUMPS CLI; runs scripts against a database.
//!
//! # Usage
//!
//! ```bash
//! # In-memory database
//! rumps --script examples/hello.rumps
//!
//! # Persistent database
//! rumps --db ./data --script examples/hello.rumps
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use miette::{NamedSource, Report};
use rumps_query::run;
use rumps_storage::Database;

/// RUMPS query language interpreter.
#[derive(Parser)]
#[command(name = "rumps")]
#[command(about = "Run RUMPS scripts against a database")]
struct Args {
    /// Path to database directory; uses in-memory if omitted.
    #[arg(long)]
    db: Option<PathBuf>,

    /// Path to script file to execute.
    #[arg(long)]
    script: PathBuf,
}

#[tokio::main]
async fn main() -> ExitCode {
    install_miette();
    let args = Args::parse();

    match run(args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:?}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> miette::Result<()> {
    let src = std::fs::read_to_string(&args.script).map_err(|e| {
        miette::miette!("failed to read script {:?}: {e}", args.script)
    })?;

    let db = match &args.db {
        Some(path) => Database::open(path).await,
        None => Database::in_memory(),
    }
    .map_err(|e| miette::miette!("failed to open database: {e}"))?;

    run(&src, db).await.map_err(|e| {
        let name = args.script.display().to_string();
        Report::new(e).with_source_code(NamedSource::new(name, src.clone()))
    })
}

fn install_miette() {
    miette::set_hook(Box::new(|_| {
        Box::new(
            miette::MietteHandlerOpts::new()
                .terminal_links(true)
                .context_lines(2)
                .build(),
        )
    }))
    .ok();
}
