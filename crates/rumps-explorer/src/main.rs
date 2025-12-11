mod app;
mod ui;

use std::io;
use std::path::PathBuf;

use clap::Parser;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use miette::{IntoDiagnostic, Result};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use rumps_storage::Database;

#[derive(Parser)]
#[command(name = "rumps-explorer")]
#[command(about = "TUI explorer for RUMPS databases")]
struct Cli {
    /// Path to the database directory
    path: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let db = Database::open(&cli.path).await.into_diagnostic()?;
    let db_path = cli.path.display().to_string();

    // Set up terminal
    enable_raw_mode().into_diagnostic()?;
    io::stdout()
        .execute(EnterAlternateScreen)
        .into_diagnostic()?;

    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend).into_diagnostic()?;

    // Run the app
    let result = app::run(db, db_path, terminal).await;

    // Restore terminal
    disable_raw_mode().into_diagnostic()?;
    io::stdout()
        .execute(LeaveAlternateScreen)
        .into_diagnostic()?;

    result
}
