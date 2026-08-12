mod assets;
mod auth;
mod config;
mod handlers;
mod posts;
mod site;
mod state;
mod tools;
mod zola;

use std::{future::IntoFuture, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};

use config::Config;
use state::AppState;

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    if let Err(error) = run().await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let config = Config::load()?;
    let search_root = search_root()?;
    let zola_root = site::discover(&search_root)?;
    site::validate(&zola_root)?;

    let (ready_tx, ready_rx) = tokio::sync::watch::channel(false);
    let zola = zola::spawn(&zola_root, ready_tx).await?;
    let state = Arc::new(AppState {
        config,
        http: reqwest::Client::new(),
        zola_root,
        ready: ready_rx,
        coordinator: tokio::sync::Mutex::new(()),
        zola,
    });

    let api = Router::new()
        .route("/health", get(handlers::health))
        .route("/ready", get(handlers::ready))
        .route("/chat", post(handlers::chat))
        .route("/web_search", post(handlers::web_search))
        .route("/web_fetch", post(handlers::web_fetch))
        .route("/transcribe", post(handlers::transcribe))
        .route("/upload-image", post(handlers::upload_image))
        .route("/rename-image", post(handlers::rename_image))
        .route("/delete-image", post(handlers::delete_image))
        .route("/posts", get(posts::list))
        .route("/post", get(posts::load))
        .route("/post/save", post(posts::save))
        .route("/post/create", post(posts::create))
        .route("/post/rename-preview", get(posts::rename_preview))
        .route("/post/rename", post(posts::rename))
        .route("/post/delete", post(posts::delete))
        .route("/post/recover", post(posts::recover))
        .route("/preview-check", get(posts::preview_check));

    let app = Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/preview-site", get(handlers::preview_site))
        .route("/preview-site/{*path}", get(handlers::preview_site_path))
        .nest("/api", api)
        .fallback(assets::static_handler)
        .layer(DefaultBodyLimit::max(25 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ))
        .with_state(state.clone());

    let listener = match tokio::net::TcpListener::bind("0.0.0.0:3000").await {
        Ok(listener) => listener,
        Err(error) => {
            state.zola.shutdown().await?;
            return Err(match error.kind() {
                std::io::ErrorKind::AddrInUse => {
                    "Blogger cannot start because port 3000 is already in use".to_string()
                }
                _ => format!("failed to bind Blogger web UI to port 3000: {error}"),
            });
        }
    };

    println!("listening on 0.0.0.0:3000");
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(wait_for_shutdown(shutdown_tx));

    let mut server_shutdown = shutdown_rx.clone();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = server_shutdown.changed().await;
        })
        .into_future();
    tokio::pin!(server);

    let mut shutdown_rx = shutdown_rx;
    let server_result = tokio::select! {
        result = &mut server => result.map_err(|e| format!("server error: {e}")),
        _ = shutdown_rx.changed() => {
            println!("shutting down...");
            if tokio::time::timeout(Duration::from_secs(5), &mut server).await.is_err() {
                eprintln!("warning: HTTP shutdown grace period expired");
            }
            Ok(())
        }
    };

    state.zola.shutdown().await?;
    server_result
}

fn search_root() -> Result<PathBuf, String> {
    let mut args = std::env::args_os().skip(1);
    let root = match args.next() {
        Some(root) => PathBuf::from(root),
        None => std::env::current_dir()
            .map_err(|e| format!("failed to determine current directory: {e}"))?,
    };
    if args.next().is_some() {
        return Err("usage: blogger [SEARCH_ROOT]".to_string());
    }
    Ok(root)
}

async fn wait_for_shutdown(shutdown: tokio::sync::watch::Sender<bool>) {
    #[cfg(unix)]
    {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(error) => {
                    eprintln!("warning: failed to install SIGTERM handler: {error}");
                    let _ = tokio::signal::ctrl_c().await;
                    let _ = shutdown.send(true);
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }

    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;

    let _ = shutdown.send(true);
}
