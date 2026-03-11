pub struct AppState {
    pub ollama_key: String,
    pub http: reqwest::Client,
    pub preview_url: tokio::sync::watch::Receiver<Option<String>>,
    pub initial_file: Option<(std::path::PathBuf, String)>,
}
