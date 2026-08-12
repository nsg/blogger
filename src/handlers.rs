use std::{
    io::Write,
    path::{Component, Path as FsPath, PathBuf},
    sync::Arc,
};

use axum::{
    Json,
    extract::{Multipart, Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::tools::{exec_tool, web_tools};
use crate::{posts, state::AppState};

const MAX_TOOL_ROUNDS: usize = 6;
const OPENAI_TRANSCRIPTIONS_URL: &str = "https://api.openai.com/v1/audio/transcriptions";
const STT_MODEL: &str = "gpt-4o-transcribe";

type ApiErr = (StatusCode, Json<Value>);

fn bad_gateway(msg: String) -> ApiErr {
    (StatusCode::BAD_GATEWAY, Json(json!({ "error": msg })))
}

pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

pub async fn ready(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let ready = *state.ready.borrow();
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(json!({ "ready": ready })))
}

async fn ollama_chat(state: &AppState, payload: &Value) -> Result<Value, ApiErr> {
    let res = state
        .http
        .post("https://ollama.com/api/chat")
        .bearer_auth(&state.ollama_key)
        .json(payload)
        .send()
        .await
        .map_err(|e| bad_gateway(format!("Ollama request failed: {e}")))?;

    let status = res.status();
    let body: Value = res
        .json()
        .await
        .map_err(|e| bad_gateway(format!("Failed to parse Ollama response: {e}")))?;

    if !status.is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("Ollama returned {status}"), "detail": body })),
        ));
    }
    Ok(body)
}

fn inject_tool_log(mut response: Value, tool_log: Vec<Value>) -> Value {
    if !tool_log.is_empty() {
        response["tool_log"] = Value::Array(tool_log);
    }
    response
}

pub async fn chat(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiErr> {
    let messages = body.get("messages").ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "missing 'messages' field" })),
        )
    })?;

    let model = body
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("qwen3.5:397b");

    let use_tools = body.get("tools").and_then(|t| t.as_bool()).unwrap_or(true);

    let mut msgs = messages.as_array().cloned().unwrap_or_default();
    let mut tool_log: Vec<Value> = Vec::new();

    for _ in 0..MAX_TOOL_ROUNDS {
        let mut payload = json!({
            "model": model,
            "messages": msgs,
            "stream": false,
        });
        if use_tools {
            payload["tools"] = web_tools();
        }

        let response = ollama_chat(&state, &payload).await?;

        let tool_calls = response
            .get("message")
            .and_then(|m| m.get("tool_calls"))
            .and_then(|tc| tc.as_array())
            .cloned();

        if let Some(calls) = tool_calls {
            if calls.is_empty() {
                return Ok(Json(inject_tool_log(response, tool_log)));
            }

            let edit_calls: Vec<&Value> = calls
                .iter()
                .filter(|c| {
                    c.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        == Some("edit_paragraph")
                })
                .collect();

            if !edit_calls.is_empty() {
                return Ok(Json(inject_tool_log(response, tool_log)));
            }

            if let Some(msg) = response.get("message") {
                msgs.push(msg.clone());
            }

            for call in &calls {
                let name = call
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .unwrap_or("unknown");
                let args = call
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .cloned()
                    .unwrap_or(json!({}));

                tool_log.push(json!({ "tool": name, "args": args }));

                let result = exec_tool(&state, name, &args).await;

                msgs.push(json!({
                    "role": "tool",
                    "content": serde_json::to_string(&result).unwrap_or_default(),
                }));
            }
        } else {
            return Ok(Json(inject_tool_log(response, tool_log)));
        }
    }

    let payload = json!({
        "model": model,
        "messages": msgs,
        "stream": false,
    });
    let response = ollama_chat(&state, &payload).await?;
    Ok(Json(inject_tool_log(response, tool_log)))
}

pub async fn web_search(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiErr> {
    let query = body.get("query").and_then(|q| q.as_str()).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "missing 'query' field" })),
        )
    })?;
    let max_results = body
        .get("max_results")
        .and_then(|m| m.as_u64())
        .unwrap_or(5);

    let result = exec_tool(
        &state,
        "web_search",
        &json!({ "query": query, "max_results": max_results }),
    )
    .await;
    Ok(Json(result))
}

pub async fn web_fetch(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiErr> {
    let url = body.get("url").and_then(|u| u.as_str()).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "missing 'url' field" })),
        )
    })?;

    let result = exec_tool(&state, "web_fetch", &json!({ "url": url })).await;
    Ok(Json(result))
}

fn strip_livereload(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find("<script") {
        let Some(end) = rest[start..].find("</script>") else {
            break;
        };
        let tag_end = start + end + "</script>".len();
        if rest[start..tag_end].contains("livereload") {
            result.push_str(&rest[..start]);
        } else {
            result.push_str(&rest[..tag_end]);
        }
        rest = &rest[tag_end..];
    }
    result.push_str(rest);
    result
}

fn rewrite_preview_html(html: &str) -> String {
    strip_livereload(html)
        .replace("href=\"/", "href=\"/preview-site/")
        .replace("src=\"/", "src=\"/preview-site/")
        .replace("action=\"/", "action=\"/preview-site/")
        .replace("http://localhost:1111", "/preview-site")
        .replace("http://127.0.0.1:1111", "/preview-site")
        .replace("//localhost:1111", "/preview-site")
        .replace("//127.0.0.1:1111", "/preview-site")
}

fn rewrite_preview_css(css: &str) -> String {
    css.replace("url(/", "url(/preview-site/")
        .replace("url('/", "url('/preview-site/")
        .replace("url(\"/", "url(\"/preview-site/")
        .replace("http://localhost:1111", "/preview-site")
        .replace("http://127.0.0.1:1111", "/preview-site")
        .replace("//localhost:1111", "/preview-site")
        .replace("//127.0.0.1:1111", "/preview-site")
}

async fn proxy_preview_path(state: Arc<AppState>, path: &str) -> Response {
    let path = path.trim_start_matches('/');
    let url = if path.is_empty() {
        "http://127.0.0.1:1111/".to_string()
    } else {
        format!("http://127.0.0.1:1111/{path}")
    };

    let Ok(res) = state.http.get(url).send().await else {
        return (
            StatusCode::BAD_GATEWAY,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "preview unavailable",
        )
            .into_response();
    };

    let status = res.status();
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();

    let Ok(body) = res.bytes().await else {
        return (
            StatusCode::BAD_GATEWAY,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "failed to read preview response",
        )
            .into_response();
    };

    if content_type.starts_with("text/html") {
        let html = String::from_utf8_lossy(&body);
        return (
            status,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            rewrite_preview_html(&html),
        )
            .into_response();
    }

    if content_type.starts_with("text/css") {
        let css = String::from_utf8_lossy(&body);
        return (
            status,
            [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
            rewrite_preview_css(&css),
        )
            .into_response();
    }

    (status, [(header::CONTENT_TYPE, content_type)], body).into_response()
}

pub async fn preview_site(State(state): State<Arc<AppState>>) -> Response {
    proxy_preview_path(state, "").await
}

pub async fn preview_site_path(
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
) -> Response {
    proxy_preview_path(state, &path).await
}

fn multipart_boundary() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("blogger-stt-{nanos}-{}", std::process::id())
}

fn clean_header_value(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '"' | '\r' | '\n' => '-',
            _ => c,
        })
        .collect()
}

fn append_form_field(body: &mut Vec<u8>, boundary: &str, name: &str, value: &str) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(value.as_bytes());
    body.extend_from_slice(b"\r\n");
}

fn build_transcription_body(
    boundary: &str,
    filename: &str,
    content_type: &str,
    data: &[u8],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(data.len() + 512);
    append_form_field(&mut body, boundary, "model", STT_MODEL);
    append_form_field(&mut body, boundary, "response_format", "json");
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n",
            clean_header_value(filename)
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        format!("Content-Type: {}\r\n\r\n", clean_header_value(content_type)).as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

pub async fn transcribe(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<Value>, ApiErr> {
    let field = multipart.next_field().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("invalid multipart: {e}") })),
        )
    })?;
    let field = field.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "no audio field" })),
        )
    })?;

    let filename = field.file_name().unwrap_or("dictation.webm").to_string();
    let content_type = field.content_type().unwrap_or("audio/webm").to_string();
    let data = field.bytes().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("failed to read audio: {e}") })),
        )
    })?;

    if data.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "empty audio" })),
        ));
    }

    let boundary = multipart_boundary();
    let body = build_transcription_body(&boundary, &filename, &content_type, &data);
    let res = state
        .http
        .post(OPENAI_TRANSCRIPTIONS_URL)
        .bearer_auth(&state.stt_api_key)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .send()
        .await
        .map_err(|e| bad_gateway(format!("Transcription request failed: {e}")))?;

    let status = res.status();
    let body: Value = res
        .json()
        .await
        .map_err(|e| bad_gateway(format!("Failed to parse transcription response: {e}")))?;

    if !status.is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("Transcription returned {status}"), "detail": body })),
        ));
    }

    let text = body
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    Ok(Json(json!({ "text": text })))
}

pub async fn upload_image(
    State(state): State<Arc<AppState>>,
    mut multipart: Multipart,
) -> Result<Json<Value>, ApiErr> {
    let field = multipart.next_field().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("invalid multipart: {e}") })),
        )
    })?;
    let field = field.ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "no file field" })),
        )
    })?;

    let original_name = field.file_name().unwrap_or("paste.png").to_string();
    let data = field.bytes().await.map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("failed to read upload: {e}") })),
        )
    })?;

    if data.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "empty file" })),
        ));
    }

    let year = chrono::Local::now().format("%Y").to_string();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let sanitized: String = original_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let filename = format!("{timestamp}-{sanitized}");

    let _guard = state.coordinator.lock().await;
    let images_root = ensure_images_root(&state.zola_root)?;
    let dir = images_root.join(&year);
    std::fs::create_dir_all(&dir).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to create directory: {e}") })),
        )
    })?;
    let dir = confined_image_parent(&images_root, &dir)?;

    let file_path = dir.join(&filename);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file_path)
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("failed to create image: {e}") })),
            )
        })?;
    file.write_all(&data).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to write image: {e}") })),
        )
    })?;

    let markdown_path = format!("/images/{year}/{filename}");
    Ok(Json(json!({ "path": markdown_path })))
}

fn ensure_images_root(site_root: &FsPath) -> Result<PathBuf, ApiErr> {
    let canonical_site_root = site_root.canonicalize().map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to resolve site directory: {error}") })),
        )
    })?;
    let images_root = canonical_site_root.join("static/images");
    std::fs::create_dir_all(&images_root).map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to create image directory: {error}") })),
        )
    })?;
    let canonical_images_root = images_root.canonicalize().map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to resolve image directory: {error}") })),
        )
    })?;
    if canonical_images_root != images_root {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "error": format!(
                    "image directory misconfiguration: {} resolves to {}, expected {}",
                    images_root.display(),
                    canonical_images_root.display(),
                    images_root.display()
                )
            })),
        ));
    }
    Ok(canonical_images_root)
}

fn confined_image_parent(images_root: &FsPath, parent: &FsPath) -> Result<PathBuf, ApiErr> {
    let parent = parent.canonicalize().map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("invalid image path: {error}") })),
        )
    })?;
    if !parent.starts_with(images_root) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid image path" })),
        ));
    }
    Ok(parent)
}

fn resolve_image_path(images_root: &FsPath, image_path: &str) -> Result<PathBuf, ApiErr> {
    if image_path.contains(['\\', '\0']) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid image path" })),
        ));
    }
    let relative = image_path.strip_prefix("/images/").ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid image path" })),
        )
    })?;
    let relative = FsPath::new(relative);
    let components = relative.components().collect::<Vec<_>>();
    if components.len() < 2
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid image path" })),
        ));
    }
    let filename = relative.file_name().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid image path" })),
        )
    })?;
    let parent = relative.parent().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid image path" })),
        )
    })?;
    let requested_parent = images_root.join(parent);
    let mut existing_parent = requested_parent.as_path();
    while !existing_parent.exists() {
        existing_parent = existing_parent.parent().ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "invalid image path" })),
            )
        })?;
    }
    confined_image_parent(images_root, existing_parent)?;
    Ok(requested_parent.join(filename))
}

pub async fn rename_image(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiErr> {
    let old_path = body
        .get("old_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "missing 'old_path'" })),
            )
        })?;
    let new_name = body
        .get("new_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "missing 'new_name'" })),
            )
        })?;

    let sanitized: String = new_name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.is_empty() || matches!(sanitized.as_str(), "." | "..") {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid image name" })),
        ));
    }

    let _guard = state.coordinator.lock().await;
    let images_root = ensure_images_root(&state.zola_root)?;
    let new_md_path = rename_image_file(&images_root, old_path, &sanitized)?;
    Ok(Json(json!({ "path": new_md_path })))
}

fn rename_image_file(
    images_root: &FsPath,
    old_path: &str,
    sanitized: &str,
) -> Result<String, ApiErr> {
    let src = resolve_image_path(images_root, old_path)?;
    match std::fs::symlink_metadata(&src) {
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "image path is not a file" })),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({ "error": "image not found" })),
            ));
        }
        Err(error) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("failed to inspect image: {error}") })),
            ));
        }
    }

    let dir = src.parent().unwrap();
    let dest = dir.join(sanitized);
    if src != dest {
        posts::rename_without_overwrite(&src, &dest).map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": "image destination already exists" })),
            ),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("rename failed: {error}") })),
            ),
        })?;
    }

    let relative = dest.strip_prefix(images_root).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "renamed image is outside the image directory" })),
        )
    })?;
    let relative = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Ok(format!("/images/{relative}"))
}

pub async fn delete_image(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiErr> {
    let image_path = body.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "missing 'path'" })),
        )
    })?;

    let _guard = state.coordinator.lock().await;
    let images_root = ensure_images_root(&state.zola_root)?;
    let full = resolve_image_path(&images_root, image_path)?;

    match std::fs::symlink_metadata(&full) {
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            std::fs::remove_file(&full).map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("delete failed: {e}") })),
                )
            })?;
        }
        Ok(_) => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "image path is not a file" })),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("failed to inspect image: {error}") })),
            ));
        }
    }

    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{ensure_images_root, rename_image_file, rewrite_preview_css, rewrite_preview_html};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "blogger-handlers-test-{}-{nanos}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn image_rename_never_overwrites_and_preserves_nested_markdown_path() {
        let tree = TempTree::new();
        let images_root = tree.path().join("static/images");
        fs::create_dir_all(images_root.join("2026/trip")).unwrap();
        let source = images_root.join("2026/trip/photo.png");
        let destination = images_root.join("2026/trip/renamed.png");
        fs::write(&source, "source").unwrap();
        fs::write(&destination, "destination").unwrap();

        let error = rename_image_file(&images_root, "/images/2026/trip/photo.png", "renamed.png")
            .unwrap_err();
        assert_eq!(error.0, axum::http::StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(fs::read_to_string(&source).unwrap(), "source");
        assert_eq!(fs::read_to_string(&destination).unwrap(), "destination");

        fs::remove_file(&destination).unwrap();
        let path =
            rename_image_file(&images_root, "/images/2026/trip/photo.png", "renamed.png").unwrap();
        assert_eq!(path, "/images/2026/trip/renamed.png");
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(&destination).unwrap(), "source");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_images_root_as_internal_misconfiguration() {
        use std::os::unix::fs::symlink;

        let tree = TempTree::new();
        fs::create_dir_all(tree.path().join("static")).unwrap();
        fs::create_dir_all(tree.path().join("content")).unwrap();
        symlink("../content", tree.path().join("static/images")).unwrap();

        let error = ensure_images_root(tree.path()).unwrap_err();
        assert_eq!(error.0, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            error.1.0["error"]
                .as_str()
                .unwrap()
                .contains("misconfiguration")
        );
    }

    #[test]
    fn strips_livereload_script_but_keeps_other_scripts() {
        let html = r#"<body><script src="/app.js"></script><p>hi</p><script src="/livereload.js?port=1111&amp;mindelay=10"></script></body>"#;

        let rewritten = rewrite_preview_html(html);

        assert!(rewritten.contains(r#"src="/preview-site/app.js""#));
        assert!(rewritten.contains("<p>hi</p>"));
        assert!(!rewritten.contains("livereload"));
    }

    #[test]
    fn rewrites_preview_asset_urls_to_proxy() {
        let html = r#"<link rel="stylesheet" href="/style.css"><script src="http://localhost:1111/app.js"></script>"#;

        let rewritten = rewrite_preview_html(html);

        assert!(rewritten.contains(r#"href="/preview-site/style.css""#));
        assert!(rewritten.contains(r#"src="/preview-site/app.js""#));
        assert!(!rewritten.contains("localhost:1111"));
        assert!(!rewritten.contains("/preview-site/preview-site"));
    }

    #[test]
    fn rewrites_preview_css_asset_urls_to_proxy() {
        let css =
            r#"@font-face{src:url("/fonts/Hanken.ttf")}body{background:url('/images/bg.png')}"#;

        let rewritten = rewrite_preview_css(css);

        assert!(rewritten.contains(r#"url("/preview-site/fonts/Hanken.ttf")"#));
        assert!(rewritten.contains(r#"url('/preview-site/images/bg.png')"#));
        assert!(!rewritten.contains("url(\"/fonts"));
        assert!(!rewritten.contains("url('/images"));
    }
}
