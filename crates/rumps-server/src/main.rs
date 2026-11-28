use std::path::PathBuf;
use std::process;

use clap::Parser;
use rumps_server::config::Config;
use rumps_server::Server;

/// RUMPS database server; HTTP RPC access to a RUMPS database.
#[derive(Parser)]
#[command(name = "rumps-server")]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Bind address (overrides config).
    #[arg(long)]
    bind: Option<String>,

    /// Port (overrides config).
    #[arg(long)]
    port: Option<u16>,

    /// Database path (overrides config).
    #[arg(long)]
    db: Option<PathBuf>,

    /// Disable authentication. The server will accept all requests
    /// regardless of credentials.
    #[arg(long)]
    no_auth: bool,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rumps_server=info".into()),
        )
        .init();

    let cli = Cli::parse();

    let mut cfg = match &cli.config {
        Some(path) => match Config::from_file(path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("error: {e}");
                process::exit(1);
            }
        },
        None => Config::default(),
    };

    if let Some(bind) = cli.bind {
        cfg.server.bind = bind;
    }
    if let Some(port) = cli.port {
        cfg.server.port = port;
    }
    if let Some(db) = cli.db {
        cfg.database.path = db;
    }

    let mut server = Server::new(cfg);
    if cli.no_auth {
        server = server.no_auth();
    }

    if let Err(e) = server.run().await {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
