use std::io;

use miette::Result;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use rumps_storage::Database;

pub async fn run(
    _db: Database,
    _db_path: String,
    mut _term: Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    Ok(())
}
