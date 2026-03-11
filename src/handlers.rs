use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use serde_json::{Value, json};

use crate::state::AppState;
use crate::tools::{exec_tool, web_tools};

const MAX_TOOL_ROUNDS: usize = 6;

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
                return Ok(Json(response));
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
                return Ok(Json(response));
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

                let result = exec_tool(&state, name, &args).await;

                msgs.push(json!({
                    "role": "tool",
                    "content": serde_json::to_string(&result).unwrap_or_default(),
                }));
            }
        } else {
            return Ok(Json(response));
        }
    }

    let payload = json!({
        "model": model,
        "messages": msgs,
        "stream": false,
    });
    ollama_chat(&state, &payload).await.map(Json)
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

pub async fn preview(State(state): State<Arc<AppState>>) -> Json<Value> {
    let base_url = state.preview_url.borrow().clone();

    let slug = state.initial_file.as_ref().and_then(|(path, _)| {
        let path_str = path.to_string_lossy();
        let content_marker = "/content/";
        let idx = path_str.find(content_marker)?;
        let relative = &path_str[idx + content_marker.len()..];
        let stem = relative.strip_suffix(".md").unwrap_or(relative);
        Some(format!("/{stem}/"))
    });

    let url = match (&base_url, &slug) {
        (Some(base), Some(s)) => Some(format!("{}{}", base.trim_end_matches('/'), s)),
        (Some(base), None) => Some(base.clone()),
        _ => None,
    };

    Json(json!({ "url": url }))
}

pub async fn initial_content(State(state): State<Arc<AppState>>) -> Json<Value> {
    match &state.initial_file {
        Some((path, _)) => {
            let content = std::fs::read_to_string(path).unwrap_or_default();
            Json(json!({
                "path": path.display().to_string(),
                "content": content,
            }))
        }
        None => Json(json!({ "path": null, "content": null })),
    }
}

pub async fn save_file(
    State(state): State<Arc<AppState>>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiErr> {
    let path = match &state.initial_file {
        Some((path, _)) => path,
        None => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "no file loaded" })),
            ));
        }
    };

    let content = body
        .get("content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "missing 'content' field" })),
            )
        })?;

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

    Ok(Json(json!({ "ok": true })))
}
