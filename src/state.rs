pub struct AuthState {
    pub pin: String,
    pub pin_expires_at: std::time::Instant,
    pub session_token: String,
}

pub struct DocumentState {
    pub content: String,
    pub revision: u64,
}

pub struct AppState {
    pub ollama_key: String,
    pub stt_api_key: String,
    pub auth: AuthState,
    pub http: reqwest::Client,
    pub preview_url: tokio::sync::watch::Receiver<Option<String>>,
    pub initial_file: Option<(std::path::PathBuf, String)>,
    pub site_root: Option<std::path::PathBuf>,
    pub document: tokio::sync::RwLock<DocumentState>,
}
