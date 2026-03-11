pub fn launch_zola_container(site_path: &std::path::Path) -> Result<(), String> {
    let site_dir = site_path.join("site");
    let serve_path = if site_dir.is_dir() {
        &site_dir
    } else {
        site_path
    };

    let canonical = serve_path
        .canonicalize()
        .map_err(|e| format!("invalid path: {e}"))?;

    let _ = std::process::Command::new("podman")
        .args(["rm", "-f", "blogger-zola"])
        .output();

    let status = std::process::Command::new("podman")
        .args([
            "run",
            "-d",
            "--name",
            "blogger-zola",
            "-p",
            "1111:1111",
            "-p",
            "1024:1024",
            "-v",
            &format!("{}:/site:z", canonical.display()),
            "-w",
            "/site",
            "ghcr.io/getzola/zola:v0.19.2",
            "serve",
            "-p",
            "1111",
            "-i",
            "0.0.0.0",
            "--base-url",
            "localhost",
        ])
        .status()
        .map_err(|e| format!("failed to run podman: {e}"))?;

    if !status.success() {
        return Err("podman container failed to start".into());
    }

    Ok(())
}

pub async fn wait_for_zola(tx: tokio::sync::watch::Sender<Option<String>>) {
    let client = reqwest::Client::new();
    for i in 0..60 {
        if client.get("http://localhost:1111").send().await.is_ok() {
            println!("zola ready after ~{}ms", i * 500);
            let _ = tx.send(Some("http://localhost:1111".into()));
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    eprintln!("warning: zola container never became ready");
}

pub fn stop_zola() {
    let _ = std::process::Command::new("podman")
        .args(["rm", "-f", "blogger-zola"])
        .output();
}
