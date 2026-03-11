mod assets;
mod handlers;
mod state;
mod tools;
mod zola;

use std::sync::Arc;

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};

use state::AppState;

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
const KEYRING_USER: &str = "ollama_api_key";

fn get_api_key() -> String {
    // 1. Environment variable (or .env)
    if let Ok(key) = std::env::var("OLLAMA_API_KEY")
        && !key.is_empty()
    {
        return key;
    }
    // 2. System keyring
    match keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER) {
        Ok(entry) => match entry.get_password() {
            Ok(key) if !key.is_empty() => return key,
            _ => {}
        },
        Err(e) => eprintln!("warning: keyring unavailable: {e}"),
    }
    String::new()
}

fn cmd_set_key() {
    let key = rpassword::prompt_password("Ollama API key: ").expect("failed to read input");
    if key.trim().is_empty() {
        eprintln!("error: empty key");
        std::process::exit(1);
    }
    let entry =
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).expect("failed to access keyring");
    entry
        .set_password(key.trim())
        .expect("failed to store key in keyring");
    println!("API key stored in system keyring");
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();

    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("set-key") {
        cmd_set_key();
        return;
    }

    let ollama_key = get_api_key();
    if ollama_key.is_empty() {
        eprintln!("warning: no API key found — run `blogger set-key` or set OLLAMA_API_KEY");
    }

    let (preview_tx, preview_rx) = tokio::sync::watch::channel(None);
    let mut initial_file: Option<(std::path::PathBuf, String)> = None;

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

    let state = Arc::new(AppState {
        ollama_key,
        http: reqwest::Client::new(),
        preview_url: preview_rx,
        initial_file,
    });

    let api = Router::new()
        .route("/health", get(handlers::health))
        .route("/chat", post(handlers::chat))
        .route("/web_search", post(handlers::web_search))
        .route("/web_fetch", post(handlers::web_fetch))
        .route("/preview", get(handlers::preview))
        .route("/initial-content", get(handlers::initial_content))
        .route("/save", post(handlers::save_file));

    let app = Router::new()
        .nest("/api", api)
        .with_state(state)
        .fallback(assets::static_handler)
        .layer(DefaultBodyLimit::max(10 * 1024 * 1024));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("failed to bind to port 3000");

    println!("listening on http://localhost:3000");

    let shutdown = async {
        tokio::signal::ctrl_c().await.ok();
        println!("\nshutting down...");
        zola::stop_zola();
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
        .expect("server error");
}
