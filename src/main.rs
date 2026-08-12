mod assets;
mod auth;
mod config;
mod git;
mod handlers;
mod mcp;
mod oauth;
mod post_store;
mod posts;
mod site;
mod state;
mod tools;
mod zola;

use std::{future::IntoFuture, io::Write, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};

use config::Config;
use state::AppState;

fn main() {
    if askpass() {
        return;
    }
    dotenvy::dotenv().ok();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to create Tokio runtime");
    if let Err(error) = runtime.block_on(run()) {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn askpass() -> bool {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    let prompt = if first.as_deref() == Some(std::ffi::OsStr::new("--askpass")) {
        args.next().unwrap_or_default()
    } else if std::env::var_os("BLOGGER_ASKPASS_MODE").as_deref() == Some(std::ffi::OsStr::new("1"))
    {
        first.unwrap_or_default()
    } else {
        return false;
    };
    let value = if prompt.to_string_lossy().starts_with("Username") {
        "x-access-token".to_owned()
    } else {
        std::env::var("GITHUB_TOKEN").unwrap_or_default()
    };
    let _ = writeln!(std::io::stdout(), "{value}");
    true
}

async fn run() -> Result<(), String> {
    let config = Config::load()?;
    let search_root = search_root()?;
    let zola_root = site::discover(&search_root)?;
    site::validate(&zola_root)?;
    let repository = git::validate_repository(&zola_root, &config).await?;

    let (ready_tx, ready_rx) = tokio::sync::watch::channel(false);
    let zola = zola::spawn(&zola_root, ready_tx).await?;
    let password_gate = Arc::new(auth::PasswordGate::new());
    let state = Arc::new(AppState {
        config,
        password_gate: password_gate.clone(),
        http: reqwest::Client::new(),
        zola_root,
        repository,
        ready: ready_rx,
        coordinator: Arc::new(tokio::sync::Mutex::new(())),
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
        .route("/preview-check", get(posts::preview_check))
        .route("/git/status", get(git::status))
        .route("/git/prepare", post(git::prepare))
        .route("/git/commit-push", post(git::commit_push))
        .route("/git/retry-push", post(git::retry_push))
        .route("/git/sync", post(git::sync));

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

    let oauth_state = oauth::OAuthState::new(
        state.config.password.clone(),
        password_gate,
        state.config.session_secret,
        state.config.mcp_public_url.clone(),
        state.config.mcp_issuer.clone(),
    );
    let (public_app, cancel_mcp) = oauth::public_router(
        oauth_state,
        state.zola_root.clone(),
        state.coordinator.clone(),
        state.config.mcp_host.clone(),
    );

    let listener = bind_listener(3000, "Blogger web UI", &state).await?;
    let mcp_listener = bind_listener(3001, "Blogger MCP service", &state).await?;

    println!("listening on 0.0.0.0:3000");
    println!("MCP listening on 0.0.0.0:3001");
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(wait_for_shutdown(shutdown_tx));

    let mut server_shutdown = shutdown_rx.clone();
    let server = axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = server_shutdown.changed().await;
        })
        .into_future();
    tokio::pin!(server);

    let mut mcp_server_shutdown = shutdown_rx.clone();
    let mcp_server = axum::serve(mcp_listener, public_app)
        .with_graceful_shutdown(async move {
            let _ = mcp_server_shutdown.changed().await;
        })
        .into_future();
    tokio::pin!(mcp_server);

    let mut shutdown_rx = shutdown_rx;
    let server_result = tokio::select! {
        result = &mut server => result.map_err(|e| format!("server error: {e}")),
        result = &mut mcp_server => result.map_err(|e| format!("MCP server error: {e}")),
        _ = shutdown_rx.changed() => {
            println!("shutting down...");
            cancel_mcp();
            if tokio::time::timeout(Duration::from_secs(5), async {
                let _ = tokio::join!(&mut server, &mut mcp_server);
            }).await.is_err() {
                eprintln!("warning: HTTP shutdown grace period expired");
            }
            Ok(())
        }
    };

    cancel_mcp();
    state.zola.shutdown().await?;
    server_result
}

async fn bind_listener(
    port: u16,
    label: &str,
    state: &AppState,
) -> Result<tokio::net::TcpListener, String> {
    match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
        Ok(listener) => Ok(listener),
        Err(error) => {
            state.zola.shutdown().await?;
            Err(match error.kind() {
                std::io::ErrorKind::AddrInUse => {
                    format!("Blogger cannot start because port {port} is already in use")
                }
                _ => format!("failed to bind {label} to port {port}: {error}"),
            })
        }
    }
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
