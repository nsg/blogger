use std::{
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::{process::Command, sync::Mutex};

pub struct ZolaChild {
    child: Mutex<Option<tokio::process::Child>>,
    shutting_down: AtomicBool,
}

impl ZolaChild {
    pub async fn shutdown(&self) -> Result<(), String> {
        self.shutting_down.store(true, Ordering::SeqCst);
        let mut child = self.child.lock().await.take();
        if let Some(child) = child.as_mut() {
            child
                .kill()
                .await
                .map_err(|e| format!("failed to stop Zola: {e}"))?;
            child
                .wait()
                .await
                .map_err(|e| format!("failed to reap Zola: {e}"))?;
        }
        Ok(())
    }
}

pub async fn spawn(
    zola_root: &Path,
    ready: tokio::sync::watch::Sender<bool>,
) -> Result<Arc<ZolaChild>, String> {
    let child = Command::new("zola")
        .args([
            "serve",
            "--interface",
            "127.0.0.1",
            "--port",
            "1111",
            "--drafts",
        ])
        .current_dir(zola_root)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start required Zola preview process: {e}"))?;

    let zola = Arc::new(ZolaChild {
        child: Mutex::new(Some(child)),
        shutting_down: AtomicBool::new(false),
    });

    tokio::spawn(probe_readiness(ready, zola.clone()));
    tokio::spawn(monitor(zola.clone()));
    Ok(zola)
}

async fn probe_readiness(ready: tokio::sync::watch::Sender<bool>, zola: Arc<ZolaChild>) {
    let client = reqwest::Client::new();
    loop {
        if zola.shutting_down.load(Ordering::SeqCst) {
            return;
        }
        if client
            .get("http://127.0.0.1:1111")
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            let _ = ready.send(true);
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn monitor(zola: Arc<ZolaChild>) {
    loop {
        let status = {
            let mut child = zola.child.lock().await;
            match child.as_mut() {
                Some(child) => child.try_wait(),
                None => return,
            }
        };

        match status {
            Ok(Some(status)) => {
                if !zola.shutting_down.load(Ordering::SeqCst) {
                    eprintln!("error: Zola preview process exited unexpectedly with {status}");
                    std::process::exit(1);
                }
                return;
            }
            Ok(None) => tokio::time::sleep(Duration::from_millis(250)).await,
            Err(e) => {
                if !zola.shutting_down.load(Ordering::SeqCst) {
                    eprintln!("error: failed to monitor Zola preview process: {e}");
                    std::process::exit(1);
                }
                return;
            }
        }
    }
}
