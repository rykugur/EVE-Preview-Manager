#![deny(unsafe_code)]

mod common;
mod config;
mod daemon;
mod input;
mod manager;
mod x11;

use anyhow::Result;
use clap::Parser;
use tracing::Level as TraceLevel;
use tracing_subscriber::FmtSubscriber;

#[derive(Parser)]
#[command(name = "eve-preview-manager")]
#[command(version)]
#[command(about = "EVE Online window preview manager", long_about = None)]
struct Cli {
    /// Run in preview daemon mode (background process showing thumbnails)
    #[arg(long)]
    preview: bool,

    /// Name of the IPC server to connect to for configuration and status updates
    #[arg(long)]
    ipc_server: Option<String>,
}

fn main() -> Result<()> {
    // Initialize logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(TraceLevel::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Failed to set tracing subscriber");

    let cli = Cli::parse();

    if cli.preview {
        // Start the dedicated preview process to isolate X11 rendering and overlay management
        // Initialize Tokio runtime for the daemon
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to build Tokio runtime");

        rt.block_on(async {
            if let Some(server_name) = cli.ipc_server {
                daemon::run_preview_daemon(server_name).await
            } else {
                eprintln!("Error: --ipc-server is required for preview daemon mode");
                std::process::exit(1);
            }
        })
    } else {
        // Default mode: launch the configuration GUI which manages the daemon lifecycle
        manager::run_gui()
    }
}
