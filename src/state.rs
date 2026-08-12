use std::{ops::Deref, path::PathBuf, sync::Arc};

use crate::{config::Config, zola::ZolaChild};

pub struct AppState {
    pub config: Config,
    pub http: reqwest::Client,
    pub zola_root: PathBuf,
    pub ready: tokio::sync::watch::Receiver<bool>,
    pub coordinator: tokio::sync::Mutex<()>,
    pub zola: Arc<ZolaChild>,
}

impl Deref for AppState {
    type Target = Config;

    fn deref(&self) -> &Self::Target {
        &self.config
    }
}
