use std::{fmt::Display, path::PathBuf, sync::Arc};

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo},
    schemars::JsonSchema,
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::{Deserialize, Serialize};

use crate::post_store;

const DEFAULT_SEARCH_LIMIT: usize = 10;
const MAX_SEARCH_LIMIT: usize = 20;
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct SearchPostsRequest {
    /// Case-insensitive text to find in post titles or Markdown content.
    pub query: String,
    /// Maximum number of matches to return. Defaults to 10 and cannot exceed 20.
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct GetPostRequest {
    /// Post path returned by search_posts.
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct BlogMcp {
    zola_root: Arc<PathBuf>,
    tool_router: ToolRouter<Self>,
}

impl BlogMcp {
    pub fn new(zola_root: impl Into<PathBuf>) -> Self {
        Self {
            zola_root: Arc::new(zola_root.into()),
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl BlogMcp {
    #[tool(
        description = "Search published and draft blog posts by title and Markdown content. Returns matching post paths, metadata, and excerpts; use get_post with a returned path to retrieve the complete post.",
        annotations(
            title = "Search blog posts",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn search_posts(
        &self,
        Parameters(SearchPostsRequest { query, limit }): Parameters<SearchPostsRequest>,
    ) -> CallToolResult {
        let limit = limit.unwrap_or(DEFAULT_SEARCH_LIMIT).min(MAX_SEARCH_LIMIT);
        structured_result(post_store::search_posts(
            self.zola_root.as_path(),
            &query,
            limit,
        ))
    }

    #[tool(
        description = "Retrieve the complete Markdown and metadata for a published or draft blog post. The path must be one returned by search_posts and is confined to the blog post directory.",
        annotations(
            title = "Get blog post",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_post(
        &self,
        Parameters(GetPostRequest { path }): Parameters<GetPostRequest>,
    ) -> CallToolResult {
        structured_result(post_store::load_post(self.zola_root.as_path(), &path))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for BlogMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("blogger", env!("CARGO_PKG_VERSION"))
                    .with_title("Blogger")
                    .with_description("Read-only access to blog posts and drafts"),
            )
            .with_instructions(
                "Use search_posts to find blog posts, then get_post with a returned path to read the complete Markdown. These tools are read-only and include drafts.",
            )
    }
}

fn structured_result<T, E>(result: Result<T, E>) -> CallToolResult
where
    T: Serialize,
    E: Display,
{
    match result {
        Ok(value) => match serde_json::to_value(value) {
            Ok(value) => CallToolResult::structured(value),
            Err(error) => structured_error("serialization_error", error),
        },
        Err(error) => structured_error("post_store_error", error),
    }
}

fn structured_error(code: &str, error: impl Display) -> CallToolResult {
    CallToolResult::structured_error(serde_json::json!({
        "error": code,
        "message": error.to_string(),
    }))
}

pub type BlogMcpHttpService = StreamableHttpService<BlogMcp, LocalSessionManager>;
pub type McpCancellation = Arc<dyn Fn() + Send + Sync>;

pub fn http_service(
    zola_root: PathBuf,
    allowed_host: String,
    config: StreamableHttpServerConfig,
) -> (BlogMcpHttpService, McpCancellation) {
    let cancellation_token = config.cancellation_token.clone();
    let config = config
        .with_allowed_hosts([allowed_host])
        .with_allowed_origins(["https://claude.ai"])
        .with_max_request_body_bytes(MAX_REQUEST_BODY_BYTES)
        .with_legacy_session_mode(true);
    let server = BlogMcp::new(zola_root);
    (
        StreamableHttpService::new(
            move || Ok(server.clone()),
            Arc::new(LocalSessionManager::default()),
            config,
        ),
        Arc::new(move || cancellation_token.cancel()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_read_only_closed_world_tools() {
        let server = BlogMcp::new("/unused");
        let tools = server.tool_router.list_all();

        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.as_ref())
                .collect::<Vec<_>>(),
            ["get_post", "search_posts"]
        );
        for tool in tools {
            let annotations = tool.annotations.as_ref().unwrap();
            assert_eq!(annotations.read_only_hint, Some(true));
            assert_eq!(annotations.destructive_hint, Some(false));
            assert_eq!(annotations.idempotent_hint, Some(true));
            assert_eq!(annotations.open_world_hint, Some(false));
        }
    }

    #[test]
    fn configures_the_public_http_boundary() {
        let (service, _cancel) = http_service(
            PathBuf::from("/unused"),
            "mcp.example.com".to_owned(),
            StreamableHttpServerConfig::default(),
        );

        assert_eq!(service.config.allowed_hosts, ["mcp.example.com"]);
        assert_eq!(service.config.allowed_origins, ["https://claude.ai"]);
        assert_eq!(
            service.config.max_request_body_bytes,
            MAX_REQUEST_BODY_BYTES
        );
        assert!(service.config.legacy_session_mode);
    }

    #[tokio::test]
    async fn returns_store_failures_as_structured_tool_errors() {
        let server = BlogMcp::new("/unused");
        let result = server
            .search_posts(Parameters(SearchPostsRequest {
                query: " ".to_owned(),
                limit: None,
            }))
            .await;

        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.unwrap(),
            serde_json::json!({
                "error": "post_store_error",
                "message": "search query must not be empty",
            })
        );
    }
}
