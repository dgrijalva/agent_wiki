use std::path::PathBuf;

use agent_wiki::{IndexStorage, Wiki};
use clap::Parser;

/// Serve a folder of markdown as a website.
#[derive(Parser)]
struct Args {
    /// Folder of markdown files to serve.
    folder: PathBuf,
    /// Port to listen on (127.0.0.1).
    #[arg(long, default_value_t = 3000)]
    port: u16,
    /// Database file to keep the index in across runs; in memory when omitted.
    #[arg(long, value_name = "PATH")]
    index: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let storage = match args.index {
        Some(path) => IndexStorage::FilePath(path),
        None => IndexStorage::InMemory,
    };
    let wiki = Wiki::builder(&args.folder)
        .index_storage(storage)
        .build()
        .await?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", args.port)).await?;
    println!(
        "Serving {} at http://127.0.0.1:{}/",
        args.folder.display(),
        args.port
    );
    axum::serve(listener, wiki.router())
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

/// Resolves on the first Ctrl-C. A second Ctrl-C exits at once, since open
/// pages hold live-update connections that don't close on their own.
async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    println!("Shutting down; waiting for open connections to close (Ctrl-C again to quit)");
    tokio::spawn(async {
        let _ = tokio::signal::ctrl_c().await;
        std::process::exit(130);
    });
}
