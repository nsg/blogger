use std::sync::Arc;

use axum::{
    Json,
    extract::{Multipart, Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::state::AppState;
use crate::tools::{exec_tool, web_tools};

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

pub async fn preview_check(State(state): State<Arc<AppState>>) -> Json<Value> {
    let base_url = state.preview_url.borrow().clone();
    let slug = preview_slug(&state);

    let url = match (&base_url, &slug) {
        (Some(base), Some(s)) => format!("{}{}", base.trim_end_matches('/'), s),
        (Some(base), None) => base.clone(),
        _ => return Json(json!({ "content_length": null })),
    };

    let cl = state
        .http
        .head(&url)
        .send()
        .await
        .ok()
        .filter(|r| r.status().is_success())
        .and_then(|r| r.headers().get("content-length").cloned())
        .and_then(|v| v.to_str().ok().map(String::from))
        .filter(|v| v != "0");

    Json(json!({ "content_length": cl }))
}

fn preview_slug(state: &AppState) -> Option<String> {
    state.initial_file.as_ref().and_then(|(path, _)| {
        let path_str = path.to_string_lossy();
        let content_marker = "/content/";
        let idx = path_str.find(content_marker)?;
        let relative = &path_str[idx + content_marker.len()..];
        let stem = relative.strip_suffix(".md").unwrap_or(relative);
        Some(format!("/{stem}/"))
    })
}

pub async fn preview(State(state): State<Arc<AppState>>) -> Json<Value> {
    let base_url = state.preview_url.borrow().clone();
    let slug = preview_slug(&state);

    let url = match (&base_url, &slug) {
        (Some(_), Some(s)) => Some(format!("/preview-site{}", s)),
        (Some(_), None) => Some("/preview-site/".to_string()),
        _ => None,
    };

    Json(json!({ "url": url }))
}

fn rewrite_preview_html(html: &str) -> String {
    html.replace("href=\"/", "href=\"/preview-site/")
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
        "http://localhost:1111/".to_string()
    } else {
        format!("http://localhost:1111/{path}")
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

pub async fn initial_content(State(state): State<Arc<AppState>>) -> Json<Value> {
    let document = state.document.read().await;
    match &state.initial_file {
        Some((path, _)) => Json(json!({
            "path": path.display().to_string(),
            "content": document.content.clone(),
            "revision": document.revision,
        })),
        None => Json(json!({
            "path": null,
            "content": if document.revision > 1 {
                Value::String(document.content.clone())
            } else {
                Value::Null
            },
            "revision": document.revision,
        })),
    }
}

pub async fn document_state(State(state): State<Arc<AppState>>) -> Json<Value> {
    let document = state.document.read().await;
    Json(json!({
        "content": document.content.clone(),
        "revision": document.revision,
        "has_file": state.initial_file.is_some(),
    }))
}

pub async fn save_file(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiErr> {
    let path = state.initial_file.as_ref().map(|(path, _)| path.clone());

    let content = body
        .get("content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "missing 'content' field" })),
            )
        })?;

    if let Some(path) = path {
        let dir = path.parent().unwrap_or(std::path::Path::new("."));
        let tmp = dir.join(format!(".blogger-save-{}.tmp", std::process::id()));
        std::fs::write(&tmp, content).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("failed to write temp file: {e}") })),
            )
        })?;
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("failed to rename file: {e}") })),
            )
        })?;
    }

    let mut document = state.document.write().await;
    document.content = content.to_string();
    document.revision += 1;
    let revision = document.revision;

    Ok(Json(json!({ "ok": true, "revision": revision })))
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
    if state.stt_api_key.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(
                json!({ "error": "missing STT API key; run `blogger set-stt-key` or set OPENAI_API_KEY" }),
            ),
        ));
    }

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
    let site_root = state.site_root.as_ref().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "no site root configured" })),
        )
    })?;

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

    let dir = site_root.join("site/static/images").join(&year);
    std::fs::create_dir_all(&dir).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to create directory: {e}") })),
        )
    })?;

    let file_path = dir.join(&filename);
    std::fs::write(&file_path, &data).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("failed to write image: {e}") })),
        )
    })?;

    let markdown_path = format!("/images/{year}/{filename}");
    Ok(Json(json!({ "path": markdown_path })))
}

fn resolve_image_path(site_root: &std::path::Path, image_path: &str) -> Option<std::path::PathBuf> {
    let relative = image_path.strip_prefix('/')?;
    let full = site_root.join("site/static").join(relative);
    if !full.starts_with(site_root.join("site/static/images")) {
        return None;
    }
    Some(full)
}

pub async fn rename_image(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiErr> {
    let site_root = state.site_root.as_ref().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "no site root configured" })),
        )
    })?;

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

    let src = resolve_image_path(site_root, old_path).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid image path" })),
        )
    })?;

    if !src.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "image not found" })),
        ));
    }

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

    let dir = src.parent().unwrap();
    let dest = dir.join(&sanitized);
    std::fs::rename(&src, &dest).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("rename failed: {e}") })),
        )
    })?;

    let parent_name = dir.file_name().unwrap_or_default().to_string_lossy();
    let new_md_path = format!("/images/{parent_name}/{sanitized}");
    Ok(Json(json!({ "path": new_md_path })))
}

pub async fn delete_image(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiErr> {
    let site_root = state.site_root.as_ref().ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "no site root configured" })),
        )
    })?;

    let image_path = body.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "missing 'path'" })),
        )
    })?;

    let full = resolve_image_path(site_root, image_path).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid image path" })),
        )
    })?;

    if full.exists() {
        std::fs::remove_file(&full).map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("delete failed: {e}") })),
            )
        })?;
    }

    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::{rewrite_preview_css, rewrite_preview_html};

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
