mod assets;
mod auth;
mod handlers;
mod state;
mod tools;
mod zola;

use std::{net::SocketAddr, sync::Arc};

use axum::{
    Router,
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
};

use state::{AppState, DocumentState};

fn slug_to_title(slug: &str) -> String {
    slug.split('-')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().to_string() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn find_blog_root(path: &std::path::Path) -> Option<std::path::PathBuf> {
    for ancestor in path.ancestors().skip(1) {
        if ancestor.join("site").is_dir() {
            return Some(ancestor.to_path_buf());
        }
        if ancestor.join("config.toml").exists() || ancestor.join("config.yaml").exists() {
            return Some(ancestor.parent().unwrap_or(ancestor).to_path_buf());
        }
    }
    None
}

fn create_post(input_path: &std::path::Path) {
    let abs_path = std::path::absolute(input_path).expect("failed to resolve path");

    if find_blog_root(&abs_path).is_none() {
        eprintln!("error: not inside a Zola site: {}", input_path.display());
        std::process::exit(1);
    }

    let slug = abs_path.file_stem().unwrap_or_default().to_string_lossy();
    let title = slug_to_title(&slug);
    let now = chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%:z");
    let front_matter =
        format!("+++\ndate = \"{now}\"\ntitle = \"{title}\"\n[taxonomies]\ntags = []\n+++\n");

    if let Some(parent) = abs_path.parent() {
        let mut new_dirs = Vec::new();
        let mut dir = parent;
        while !dir.exists() {
            new_dirs.push(dir.to_path_buf());
            dir = match dir.parent() {
                Some(p) => p,
                None => break,
            };
        }
        std::fs::create_dir_all(parent).expect("failed to create directories");
        for d in &new_dirs {
            let folder_name = d
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .replace('-', " ");
            let folder_title = slug_to_title(&folder_name);
            let index_path = d.join("_index.md");
            let index_content =
                format!("+++\ntitle = \"{folder_title}\"\nsort_by = \"date\"\n+++\n");
            std::fs::write(&index_path, &index_content).expect("failed to create _index.md");
            println!("created section: {}", index_path.display());
        }
    }
    std::fs::write(&abs_path, &front_matter).expect("failed to create file");
    println!("created new post: {}", abs_path.display());
}

const KEYRING_SERVICE: &str = "blogger";
const OLLAMA_KEYRING_USER: &str = "ollama_api_key";
const STT_KEYRING_USER: &str = "openai_api_key";

fn get_api_key(env_var: &str, keyring_user: &str) -> String {
    if let Ok(key) = std::env::var(env_var)
        && !key.is_empty()
    {
        return key;
    }

    match keyring::Entry::new(KEYRING_SERVICE, keyring_user) {
        Ok(entry) => match entry.get_password() {
            Ok(key) if !key.is_empty() => return key,
            _ => {}
        },
        Err(e) => eprintln!("warning: keyring unavailable: {e}"),
    }
    String::new()
}

fn cmd_set_key(label: &str, keyring_user: &str) {
    let key =
        rpassword::prompt_password(format!("{label} API key: ")).expect("failed to read input");
    if key.trim().is_empty() {
        eprintln!("error: empty key");
        std::process::exit(1);
    }
    let entry =
        keyring::Entry::new(KEYRING_SERVICE, keyring_user).expect("failed to access keyring");
    entry
        .set_password(key.trim())
        .expect("failed to store key in keyring");
    println!("{label} API key stored in system keyring");
}

fn local_lan_ip() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("8.8.8.8", 80)).ok()?;
    socket.local_addr().ok().map(|addr| addr.ip())
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("set-key") {
        cmd_set_key("Ollama", OLLAMA_KEYRING_USER);
        return;
    }
    if args.get(1).map(|s| s.as_str()) == Some("set-stt-key") {
        cmd_set_key("OpenAI STT", STT_KEYRING_USER);
        return;
    }

    let ollama_key = get_api_key("OLLAMA_API_KEY", OLLAMA_KEYRING_USER);
    if ollama_key.is_empty() {
        eprintln!("warning: no API key found — run `blogger set-key` or set OLLAMA_API_KEY");
    }
    let stt_api_key = get_api_key("OPENAI_API_KEY", STT_KEYRING_USER);
    if stt_api_key.is_empty() {
        eprintln!(
            "warning: no STT API key found — run `blogger set-stt-key` or set OPENAI_API_KEY"
        );
    }
    let auth = auth::new_auth_state();
    println!(
        "remote access PIN: \x1b[1m{}\x1b[0m (valid for {} seconds)",
        auth::format_pin(&auth.pin),
        auth::pin_seconds_remaining(&auth)
    );
    println!("localhost requests do not require the PIN");

    let (preview_tx, preview_rx) = tokio::sync::watch::channel(None);
    let mut initial_file: Option<(std::path::PathBuf, String)> = None;
    let mut site_root: Option<std::path::PathBuf> = None;

    if let Some(path) = args.get(1) {
        let input_path = std::path::Path::new(path);
        if !input_path.exists() {
            create_post(input_path);
        }

        let (site_path, file_path) = if input_path.is_file() {
            let file_abs = input_path
                .canonicalize()
                .expect("failed to canonicalize file path");
            let root = find_blog_root(&file_abs).unwrap_or_else(|| {
                eprintln!("error: could not find blog root from file path: {path}");
                std::process::exit(1);
            });
            (root, Some(file_abs))
        } else {
            (input_path.to_path_buf(), None)
        };

        if let Some(fp) = file_path {
            match std::fs::read_to_string(&fp) {
                Ok(content) => {
                    println!("opening file: {}", fp.display());
                    initial_file = Some((fp, content));
                }
                Err(e) => {
                    eprintln!("warning: could not read file {}: {e}", fp.display());
                }
            }
        }

        site_root = Some(site_path.clone());

        match zola::launch_zola_container(&site_path) {
            Ok(()) => {
                println!("zola container started, waiting for it to be ready...");
                tokio::spawn(zola::wait_for_zola(preview_tx));
            }
            Err(e) => {
                eprintln!("warning: failed to start zola: {e}");
            }
        }
    }

    let document_content = initial_file
        .as_ref()
        .map(|(_, content)| content.clone())
        .unwrap_or_default();

    let state = Arc::new(AppState {
        ollama_key,
        stt_api_key,
        auth,
        http: reqwest::Client::new(),
        preview_url: preview_rx,
        initial_file,
        site_root,
        document: tokio::sync::RwLock::new(DocumentState {
            content: document_content,
            revision: 1,
        }),
    });

    let api = Router::new()
        .route("/health", get(handlers::health))
        .route("/chat", post(handlers::chat))
        .route("/web_search", post(handlers::web_search))
        .route("/web_fetch", post(handlers::web_fetch))
        .route("/preview", get(handlers::preview))
        .route("/preview-check", get(handlers::preview_check))
        .route("/initial-content", get(handlers::initial_content))
        .route("/document-state", get(handlers::document_state))
        .route("/save", post(handlers::save_file))
        .route("/transcribe", post(handlers::transcribe))
        .route("/upload-image", post(handlers::upload_image))
        .route("/rename-image", post(handlers::rename_image))
        .route("/delete-image", post(handlers::delete_image));

    let app_state_for_auth = state.clone();
    let app = Router::new()
        .route("/auth/pin", post(auth::submit_pin))
        .nest("/api", api)
        .with_state(state)
        .fallback(assets::static_handler)
        .layer(DefaultBodyLimit::max(25 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(
            app_state_for_auth.clone(),
            auth::require_auth,
        ));

    let listener = match tokio::net::TcpListener::bind("0.0.0.0:3000").await {
        Ok(listener) => listener,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!("error: Blogger cannot start because port 3000 is already in use");
            eprintln!("close the other Blogger instance or free port 3000, then try again");
            zola::stop_zola();
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("error: failed to bind Blogger web UI to port 3000: {e}");
            zola::stop_zola();
            std::process::exit(1);
        }
    };

    println!("listening on 0.0.0.0:3000");
    println!("local access: http://localhost:3000");
    match local_lan_ip() {
        Some(ip) => println!("network access: http://{ip}:3000"),
        None => println!("network access: http://<lan-ip>:3000"),
    }

    let shutdown = async {
        tokio::signal::ctrl_c().await.ok();
        println!("\nshutting down...");
        zola::stop_zola();
    };

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .expect("server error");
}
