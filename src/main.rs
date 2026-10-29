use std::path::PathBuf;

use agent_wiki::{AllowedHosts, IndexStorage, Wiki, WikiBuilder};
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
    /// Rebuild the index from scratch at startup, in the background.
    #[arg(long)]
    reindex: bool,
    /// Run the scripts in notes (`<script>` elements, event handlers such as
    /// `onclick=`, and `javascript:` links) as written. Without it none run.
    #[arg(long)]
    allow_scripts: bool,
}

fn builder(args: &Args) -> WikiBuilder {
    let storage = match &args.index {
        Some(path) => IndexStorage::FilePath(path.clone()),
        None => IndexStorage::InMemory,
    };
    let builder = Wiki::builder(&args.folder)
        .index_storage(storage)
        .allowed_hosts(AllowedHosts::loopback());
    if args.allow_scripts {
        builder
    } else {
        builder.scripts(|_| false)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let wiki = builder(&args).build().await?;
    if args.reindex {
        let wiki = wiki.clone();
        tokio::spawn(async move {
            if let Err(e) = wiki.reindex().await {
                eprintln!("agent_wiki: reindex: {e:#}");
            }
        });
    }
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", args.port)).await?;
    println!(
        "Serving {} at http://127.0.0.1:{}/",
        args.folder.display(),
        args.port
    );
    println!("MCP at http://127.0.0.1:{}/_wiki/mcp", args.port);
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

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    const INLINE: &str = "<script>document.title = 'inline ran'</script>";

    async fn probe_page(flags: &[&str]) -> String {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Probe.md"),
            format!("# Probe\n\n{INLINE}\n"),
        )
        .unwrap();
        let folder = dir.path().to_str().unwrap();
        let args = Args::parse_from(["agent_wiki", folder].iter().chain(flags));
        let wiki = builder(&args).build().await.unwrap();
        wiki.indexed().await;
        let response = wiki
            .router()
            .oneshot(
                Request::get("/Probe")
                    .header("host", "127.0.0.1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn the_default_blocks_scripts_in_notes() {
        let body = probe_page(&[]).await;
        assert!(
            body.contains("http-equiv=\"Content-Security-Policy\""),
            "{body}"
        );
        assert!(!body.contains("inline ran"), "{body}");
        assert!(body.contains("<!-- script blocked -->"), "{body}");
    }

    #[tokio::test]
    async fn allow_scripts_runs_them_as_written() {
        let body = probe_page(&["--allow-scripts"]).await;
        assert!(!body.contains("Content-Security-Policy"), "{body}");
        assert!(
            body.contains("document.title = 'inline ran'</script>"),
            "{body}"
        );
    }
}
