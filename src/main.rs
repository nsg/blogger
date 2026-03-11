use std::sync::Arc;

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rust_embed::Embed;
use serde_json::{Value, json};
use tower_http::cors::CorsLayer;

#[derive(Embed)]
#[folder = "frontend/dist/"]
struct Assets;

struct AppState {
    ollama_key: String,
    http: reqwest::Client,
}

const MAX_TOOL_ROUNDS: usize = 6;

fn web_tools() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web for current information. Use this when the user asks about recent events, facts you're unsure about, or anything that benefits from up-to-date sources.",
                "parameters": {
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query"
                        },
                        "max_results": {
                            "type": "integer",
                            "description": "Number of results (1-10, default 5)"
                        }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "web_fetch",
                "description": "Fetch the full content of a web page by URL. Use this after web_search to read a promising result in detail.",
                "parameters": {
                    "type": "object",
                    "required": ["url"],
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The URL to fetch"
                        }
                    }
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit_paragraph",
                "description": "Propose a concrete edit to the current paragraph. Use this whenever you have a specific improvement — the writer will see an 'Apply fix' button. Provide the exact old text to find and the new replacement text.",
                "parameters": {
                    "type": "object",
                    "required": ["old_text", "new_text"],
                    "properties": {
                        "old_text": {
                            "type": "string",
                            "description": "The exact text to replace (must match current paragraph text exactly or be a substring of it)"
                        },
                        "new_text": {
                            "type": "string",
                            "description": "The replacement text"
                        }
                    }
                }
            }
        }
    ])
}

async fn exec_tool(state: &AppState, name: &str, args: &Value) -> Value {
    let (url, payload) = match name {
        "web_search" => {
            let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("");
            let max_results = args
                .get("max_results")
                .and_then(|m| m.as_u64())
                .unwrap_or(5);
            (
                "https://ollama.com/api/web_search",
                json!({ "query": query, "max_results": max_results }),
            )
        }
        "web_fetch" => {
            let url = args.get("url").and_then(|u| u.as_str()).unwrap_or("");
            ("https://ollama.com/api/web_fetch", json!({ "url": url }))
        }
        _ => return json!({ "error": format!("unknown tool: {name}") }),
    };

    match state
        .http
        .post(url)
        .bearer_auth(&state.ollama_key)
        .json(&payload)
        .send()
        .await
    {
        Ok(res) => res
            .json::<Value>()
            .await
            .unwrap_or(json!({ "error": "bad response" })),
        Err(e) => json!({ "error": format!("{e}") }),
    }
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

type ApiErr = (StatusCode, Json<Value>);

fn bad_gateway(msg: String) -> ApiErr {
    (StatusCode::BAD_GATEWAY, Json(json!({ "error": msg })))
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

async fn chat(
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

            // Check if any call is edit_paragraph — return those to the frontend
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
                // Return the response as-is so the frontend can render Apply buttons
                return Ok(Json(response));
            }

            // Append the assistant's tool-call message to history
            if let Some(msg) = response.get("message") {
                msgs.push(msg.clone());
            }

            // Execute each tool call and append results
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

    // Exhausted tool rounds — do one final call without tools
    let payload = json!({
        "model": model,
        "messages": msgs,
        "stream": false,
    });
    ollama_chat(&state, &payload).await.map(Json)
}

async fn web_search(
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

async fn web_fetch(
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

async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], file.data).into_response()
        }
        None => match Assets::get("index.html") {
            Some(file) => ([(header::CONTENT_TYPE, "text/html")], file.data).into_response(),
            None => (StatusCode::NOT_FOUND, "404").into_response(),
        },
    }
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let ollama_key = std::env::var("OLLAMA_API_KEY").unwrap_or_default();

    if ollama_key.is_empty() {
        eprintln!("warning: OLLAMA_API_KEY not set — /api/chat will fail");
    }

    let state = Arc::new(AppState {
        ollama_key,
        http: reqwest::Client::new(),
    });

    let api = Router::new()
        .route("/health", get(health))
        .route("/chat", post(chat))
        .route("/web_search", post(web_search))
        .route("/web_fetch", post(web_fetch));

    let app = Router::new()
        .nest("/api", api)
        .with_state(state)
        .fallback(static_handler)
        .layer(CorsLayer::permissive());

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
        .await
        .expect("failed to bind to port 3000");

    println!("listening on http://localhost:3000");
    axum::serve(listener, app).await.expect("server error");
}
