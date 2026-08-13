use std::{fmt::Display, path::PathBuf, sync::Arc};

use axum::http::request::Parts;
use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, tool::Extension, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerInfo},
    schemars::JsonSchema,
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
    },
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{post_store, writing_style};

const DEFAULT_SEARCH_LIMIT: usize = 10;
const MAX_SEARCH_LIMIT: usize = 20;
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccessScopes {
    can_write: bool,
}

impl AccessScopes {
    pub fn new(can_write: bool) -> Self {
        Self { can_write }
    }
}

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

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct CreateDraftRequest {
    /// Raw TOML front matter without +++ delimiters. Example: title = "Voice notes"\ndate = 2026-08-12\n[taxonomies]\ntags = ["notes"]. Blogger always sets draft = true.
    pub front_matter: String,
    /// Complete Markdown body, separate from the TOML front matter.
    pub body: String,
    /// Optional normalized filename slug such as voice-notes. Omit it to generate one from the title.
    pub slug: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ReplaceDraftRequest {
    /// Draft path returned by create_draft, search_posts, or get_post.
    pub path: String,
    /// Exact current revision returned by the previous draft tool call or get_post.
    pub revision: String,
    /// Complete raw TOML front matter without +++ delimiters. Blogger always sets draft = true.
    pub front_matter: String,
    /// Complete replacement Markdown body.
    pub body: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct DraftReplacementRequest {
    /// Exact, unique text currently present in the Markdown body.
    pub old_text: String,
    /// Text that replaces old_text. Use an empty string to remove the passage.
    pub new_text: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct EditDraftRequest {
    /// Draft path returned by create_draft, search_posts, or get_post.
    pub path: String,
    /// Exact current revision returned by the previous draft tool call or get_post.
    pub revision: String,
    /// Exact body-only replacements. Every old_text must occur exactly once; all changes succeed or none are written.
    pub replacements: Vec<DraftReplacementRequest>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[schemars(crate = "rmcp::schemars", rename_all = "snake_case")]
pub enum AppendSeparatorRequest {
    #[default]
    None,
    Newline,
    BlankLine,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct AppendDraftRequest {
    /// Draft path returned by create_draft, search_posts, or get_post.
    pub path: String,
    /// Exact current revision returned by the previous draft tool call or get_post.
    pub revision: String,
    /// Text to append to the Markdown body.
    pub text: String,
    /// Optional separator inserted before text. Defaults to none, which appends text exactly as supplied.
    #[serde(default)]
    pub separator: AppendSeparatorRequest,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
pub struct ReplaceWritingStyleRequest {
    /// Exact current revision returned by get_writing_style. Omit it only when get_writing_style returns a null revision.
    pub revision: Option<String>,
    /// Complete replacement writing-style Markdown.
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct BlogMcp {
    posts: post_store::DraftStore,
    writing_style: writing_style::WritingStyleStore,
    tool_router: ToolRouter<Self>,
}

impl BlogMcp {
    pub fn new(zola_root: impl Into<PathBuf>, coordinator: Arc<Mutex<()>>) -> Self {
        let zola_root = zola_root.into();
        Self {
            posts: post_store::DraftStore::new(zola_root.clone(), coordinator.clone()),
            writing_style: writing_style::WritingStyleStore::new(zola_root, coordinator),
            tool_router: Self::tool_router(),
        }
    }
}

#[tool_router]
impl BlogMcp {
    #[tool(
        description = "Retrieve the blog's complete writing-style profile. Use it before drafting or revising prose so the result follows the author's voice, tone, structure, vocabulary, formatting, and stated avoidances. An uninitialized profile is returned as empty content with a null revision.",
        annotations(
            title = "Get writing style",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn get_writing_style(&self) -> CallToolResult {
        writing_style_load_result(self.writing_style.load())
    }

    #[tool(
        description = "List the complete published and draft blog archive as title strings only, with dated posts newest first and unsorted posts last. Use this compact overview to discover relevant topics and titles before calling search_posts.",
        annotations(
            title = "List blog archive",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_archive(&self) -> CallToolResult {
        structured_result(self.posts.list_archive())
    }

    #[tool(
        description = "List all tags used by published and draft blog posts as unique strings, sorted alphabetically without changing their authored casing. Use this compact overview to discover topics before calling search_posts.",
        annotations(
            title = "List blog tags",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_tags(&self) -> CallToolResult {
        structured_result(self.posts.list_tags())
    }

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
        structured_result(self.posts.search_posts(&query, limit))
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
        structured_result(self.posts.load_post(&path))
    }

    #[tool(
        description = "Create a new draft post. Send front_matter as raw TOML without +++ delimiters, for example: title = \"Voice notes\"\ndate = 2026-08-12\n[taxonomies]\ntags = [\"notes\"]. Send the Markdown body separately. draft = true is enforced. An omitted slug is generated from the title. This only changes the working checkout; it does not commit, push, or publish.",
        annotations(
            title = "Create blog draft",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn create_draft(
        &self,
        Parameters(request): Parameters<CreateDraftRequest>,
        Extension(parts): Extension<Parts>,
    ) -> CallToolResult {
        if let Err(result) = require_write_scope(&parts) {
            return result;
        }
        draft_result(
            self.posts
                .create_draft(
                    &request.front_matter,
                    &request.body,
                    request.slug.as_deref(),
                )
                .await,
        )
    }

    #[tool(
        description = "Replace one complete draft using separate raw TOML front_matter (without +++ delimiters) and Markdown body. The exact current revision is required. Blogger rejects published posts and always preserves draft = true. This only changes the working checkout; it does not commit, push, or publish.",
        annotations(
            title = "Replace complete blog draft",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn replace_draft(
        &self,
        Parameters(request): Parameters<ReplaceDraftRequest>,
        Extension(parts): Extension<Parts>,
    ) -> CallToolResult {
        if let Err(result) = require_write_scope(&parts) {
            return result;
        }
        draft_result(
            self.posts
                .replace_draft(
                    &request.path,
                    &request.revision,
                    &request.front_matter,
                    &request.body,
                )
                .await,
        )
    }

    #[tool(
        description = "Replace the complete blog writing-style profile. Pass the exact revision returned by get_writing_style; omit revision only when that call returns a null revision. Normal Blogger Git publication versions the profile with the blog. This only changes the working checkout; it does not commit or push.",
        annotations(
            title = "Replace complete writing style",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn replace_writing_style(
        &self,
        Parameters(request): Parameters<ReplaceWritingStyleRequest>,
        Extension(parts): Extension<Parts>,
    ) -> CallToolResult {
        if let Err(result) = require_write_scope(&parts) {
            return result;
        }
        writing_style_result(
            self.writing_style
                .replace(request.revision.as_deref(), &request.content)
                .await,
        )
    }

    #[tool(
        description = "Atomically revise only a draft's Markdown body with exact old_text/new_text replacements. Every old_text must be non-empty and occur exactly once, and the exact current revision is required. Use replace_draft to change front matter. Nothing is written if any replacement fails.",
        annotations(
            title = "Edit passages in blog draft",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn edit_draft(
        &self,
        Parameters(request): Parameters<EditDraftRequest>,
        Extension(parts): Extension<Parts>,
    ) -> CallToolResult {
        if let Err(result) = require_write_scope(&parts) {
            return result;
        }
        let replacements = request
            .replacements
            .into_iter()
            .map(|replacement| post_store::ExactReplacement {
                old_text: replacement.old_text,
                new_text: replacement.new_text,
            })
            .collect::<Vec<_>>();
        draft_result(
            self.posts
                .edit_draft(&request.path, &request.revision, &replacements)
                .await,
        )
    }

    #[tool(
        description = "Append text to a draft's Markdown body using the exact current revision. Text is appended exactly as supplied unless separator is newline or blank_line. This only changes the working checkout; it does not commit, push, or publish.",
        annotations(
            title = "Append to blog draft",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn append_draft(
        &self,
        Parameters(request): Parameters<AppendDraftRequest>,
        Extension(parts): Extension<Parts>,
    ) -> CallToolResult {
        if let Err(result) = require_write_scope(&parts) {
            return result;
        }
        let separator = match request.separator {
            AppendSeparatorRequest::None => post_store::AppendSeparator::None,
            AppendSeparatorRequest::Newline => post_store::AppendSeparator::Newline,
            AppendSeparatorRequest::BlankLine => post_store::AppendSeparator::BlankLine,
        };
        draft_result(
            self.posts
                .append_draft(&request.path, &request.revision, &request.text, separator)
                .await,
        )
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for BlogMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("blogger", env!("CARGO_PKG_VERSION"))
                    .with_title("Blogger")
                    .with_description(
                        "Blog post access, versioned writing-style guidance, and safe draft writing",
                    ),
            )
            .with_instructions(
                "Call get_writing_style before drafting or revising prose. Use list_archive and list_tags for compact overviews of existing posts, then search_posts and get_post as needed. Writing tools require posts:write and current revisions. Draft tools preserve draft status. No MCP tool commits, pushes, publishes, or deletes.",
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

fn draft_result(
    result: Result<post_store::DraftMutation, post_store::DraftError>,
) -> CallToolResult {
    match result {
        Ok(value) => structured_result::<_, post_store::DraftError>(Ok(value)),
        Err(error) => match serde_json::to_value(&error) {
            Ok(value) => CallToolResult::structured_error(value),
            Err(serialization) => structured_error("serialization_error", serialization),
        },
    }
}

fn writing_style_result(
    result: Result<writing_style::WritingStyleMutation, writing_style::WritingStyleError>,
) -> CallToolResult {
    match result {
        Ok(value) => structured_result::<_, writing_style::WritingStyleError>(Ok(value)),
        Err(error) => match serde_json::to_value(&error) {
            Ok(value) => CallToolResult::structured_error(value),
            Err(serialization) => structured_error("serialization_error", serialization),
        },
    }
}

fn writing_style_load_result(
    result: Result<writing_style::WritingStyleDocument, writing_style::WritingStyleError>,
) -> CallToolResult {
    match result {
        Ok(value) => structured_result::<_, writing_style::WritingStyleError>(Ok(value)),
        Err(error) => match serde_json::to_value(&error) {
            Ok(value) => CallToolResult::structured_error(value),
            Err(serialization) => structured_error("serialization_error", serialization),
        },
    }
}

fn require_write_scope(parts: &Parts) -> Result<(), CallToolResult> {
    if parts
        .extensions
        .get::<AccessScopes>()
        .is_some_and(|scopes| scopes.can_write)
    {
        Ok(())
    } else {
        Err(CallToolResult::structured_error(serde_json::json!({
            "error": "insufficient_scope",
            "message": "This connector has read-only access. Reauthorize it with write access and retry."
        })))
    }
}

pub type BlogMcpHttpService = StreamableHttpService<BlogMcp, LocalSessionManager>;
pub type McpCancellation = Arc<dyn Fn() + Send + Sync>;

pub fn http_service(
    zola_root: PathBuf,
    coordinator: Arc<Mutex<()>>,
    allowed_host: String,
    config: StreamableHttpServerConfig,
) -> (BlogMcpHttpService, McpCancellation) {
    let cancellation_token = config.cancellation_token.clone();
    let config = config
        .with_allowed_hosts([allowed_host])
        .with_allowed_origins(["https://claude.ai"])
        .with_max_request_body_bytes(MAX_REQUEST_BODY_BYTES)
        .with_legacy_session_mode(true);
    let server = BlogMcp::new(zola_root, coordinator);
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
    fn describes_closed_world_read_and_writing_tools() {
        let server = BlogMcp::new("/unused", Arc::new(Mutex::new(())));
        let tools = server.tool_router.list_all();

        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.name.as_ref())
                .collect::<Vec<_>>(),
            [
                "append_draft",
                "create_draft",
                "edit_draft",
                "get_post",
                "get_writing_style",
                "list_archive",
                "list_tags",
                "replace_draft",
                "replace_writing_style",
                "search_posts"
            ]
        );
        for tool in tools {
            let annotations = tool.annotations.as_ref().unwrap();
            assert_eq!(annotations.open_world_hint, Some(false));
            if matches!(
                tool.name.as_ref(),
                "get_post" | "get_writing_style" | "list_archive" | "list_tags" | "search_posts"
            ) {
                assert_eq!(annotations.read_only_hint, Some(true));
                assert_eq!(annotations.destructive_hint, Some(false));
                assert_eq!(annotations.idempotent_hint, Some(true));
            } else {
                assert_eq!(annotations.read_only_hint, Some(false));
                assert_eq!(annotations.idempotent_hint, Some(false));
            }
        }
    }

    #[test]
    fn configures_the_public_http_boundary() {
        let (service, _cancel) = http_service(
            PathBuf::from("/unused"),
            Arc::new(Mutex::new(())),
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
        let server = BlogMcp::new("/unused", Arc::new(Mutex::new(())));
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
