use serde_json::{Value, json};

use crate::state::AppState;

pub fn web_tools() -> Value {
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

pub async fn exec_tool(state: &AppState, name: &str, args: &Value) -> Value {
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
