use std::{
    collections::HashMap,
    ffi::{CString, c_char, c_int},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json,
    extract::{Query, State, rejection::JsonRejection},
    http::StatusCode,
};
use chrono::{Datelike, Utc};
use serde_json::{Value, json};

use crate::{site, state::AppState};

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub async fn list(State(state): State<Arc<AppState>>) -> ApiResult {
    let posts = site::scan_posts(&state.zola_root).map_err(internal_error)?;
    let posts = posts
        .into_iter()
        .map(|post| {
            json!({
                "path": post.path,
                "title": post.title,
                "date": post.date,
                "draft": post.draft,
                "unsorted": post.unsorted,
                "url": post.url,
                "revision": post.revision,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "posts": posts })))
}

pub async fn load(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let path = query_value(&query, "path")?;
    let relative = validate_post_path(path)?;
    let file = resolve_existing_post(&state.zola_root, &relative)?;
    let bytes = read_post(&file)?;
    let content =
        String::from_utf8(bytes.clone()).map_err(|_| internal_error("post is not valid UTF-8"))?;
    let metadata = site::post_from_content(&relative, &content);

    Ok(Json(json!({
        "path": path,
        "content": content,
        "revision": revision(&bytes),
        "url": metadata.url,
        "title": metadata.title,
    })))
}

pub async fn save(
    State(state): State<Arc<AppState>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> ApiResult {
    let request = json_body(payload)?;
    let path = body_string(&request, "path")?;
    let content = body_string(&request, "content")?;
    let base_revision = body_string(&request, "base_revision")?;
    let relative = validate_post_path(path)?;
    let _guard = state.coordinator.lock().await;
    let file = match resolve_existing_post(&state.zola_root, &relative) {
        Ok(file) => file,
        Err((StatusCode::NOT_FOUND, _)) => return Err(deleted_conflict()),
        Err(error) => return Err(error),
    };
    let current = match fs::read(&file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(deleted_conflict());
        }
        Err(error) => return Err(internal_error(format!("failed to read post: {error}"))),
    };
    let current_revision = revision(&current);
    if base_revision != "overwrite" && base_revision != current_revision {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "post changed on disk",
                "current_revision": current_revision,
            })),
        ));
    }

    let metadata = site::post_from_content(&relative, content);
    ensure_url_available(
        &state.zola_root,
        &relative,
        content.as_bytes(),
        Some(&relative),
    )?;
    atomic_replace(&file, content.as_bytes()).map_err(internal_error)?;

    Ok(Json(json!({
        "revision": revision(content.as_bytes()),
        "url": metadata.url,
    })))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> ApiResult {
    let request = json_body(payload)?;
    let title = body_string(&request, "title")?;
    let slug = body_string(&request, "slug")?;
    let date_value = body_string(&request, "date")?;
    let draft = body_bool(&request, "draft")?;
    validate_slug(slug)?;
    let date = chrono::DateTime::parse_from_rfc3339(date_value).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "date must be RFC3339" })),
        )
    })?;
    let relative = PathBuf::from(format!("post/{}/{}.md", date.year(), slug));
    let path_string = content_path_string(&relative);
    let content = serialize_new_post(title, date_value, draft);
    let metadata = site::post_from_content(&relative, &content);

    let _guard = state.coordinator.lock().await;
    let file = resolve_new_post(&state.zola_root, &relative)?;
    if directory_entry_exists(&file).map_err(internal_error)? {
        return Err(filename_collision(&path_string, &metadata.url));
    }
    ensure_url_available(&state.zola_root, &relative, content.as_bytes(), None)?;
    ensure_post_parent(&state.zola_root, &file)?;
    create_new_file(&file, content.as_bytes()).map_err(|error| match error.kind() {
        std::io::ErrorKind::AlreadyExists => filename_collision(&path_string, &metadata.url),
        _ => internal_error(format!("failed to create post: {error}")),
    })?;

    Ok(Json(json!({
        "path": path_string,
        "revision": revision(content.as_bytes()),
        "url": metadata.url,
        "content": content,
    })))
}

pub async fn rename_preview(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let path = query_value(&query, "path")?;
    let new_filename = query_value(&query, "new_filename")?;
    let relative = validate_post_path(path)?;
    let normalized =
        normalize_filename(new_filename).map_err(|_| bad_request("invalid new filename"))?;
    let file = resolve_existing_post(&state.zola_root, &relative)?;
    let content = String::from_utf8(read_post(&file)?)
        .map_err(|_| internal_error("post is not valid UTF-8"))?;
    let old = site::post_from_content(&relative, &content);
    let new_relative = relative.with_file_name(format!("{normalized}.md"));
    let new = site::post_from_content(&new_relative, &content);

    Ok(Json(json!({
        "old_url": old.url,
        "new_url": new.url,
        "url_changes": old.url != new.url,
    })))
}

pub async fn rename(
    State(state): State<Arc<AppState>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> ApiResult {
    let request = json_body(payload)?;
    let path = body_string(&request, "path")?;
    let new_filename = body_string(&request, "new_filename")?;
    let base_revision = body_string(&request, "base_revision")?;
    let relative = validate_post_path(path)?;
    let normalized = normalize_filename(new_filename)?;
    let new_relative = relative.with_file_name(format!("{normalized}.md"));
    let new_path_string = content_path_string(&new_relative);

    let _guard = state.coordinator.lock().await;
    let source = resolve_existing_post(&state.zola_root, &relative)?;
    let bytes = read_post(&source)?;
    let current_revision = revision(&bytes);
    if base_revision != current_revision {
        return Err(revision_conflict(current_revision));
    }
    let content =
        String::from_utf8(bytes).map_err(|_| internal_error("post is not valid UTF-8"))?;
    let metadata = site::post_from_content(&new_relative, &content);
    let destination = resolve_new_post(&state.zola_root, &new_relative)?;
    if destination != source && directory_entry_exists(&destination).map_err(internal_error)? {
        return Err(filename_collision(&new_path_string, &metadata.url));
    }
    ensure_url_available(
        &state.zola_root,
        &new_relative,
        content.as_bytes(),
        Some(&relative),
    )?;
    if destination != source {
        rename_without_overwrite(&source, &destination).map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists => {
                filename_collision(&new_path_string, &metadata.url)
            }
            _ => internal_error(format!("failed to rename post: {error}")),
        })?;
    }

    Ok(Json(json!({
        "path": new_path_string,
        "url": metadata.url,
    })))
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> ApiResult {
    let request = json_body(payload)?;
    let path = body_string(&request, "path")?;
    let base_revision = body_string(&request, "base_revision")?;
    let relative = validate_post_path(path)?;
    let _guard = state.coordinator.lock().await;
    let file = resolve_existing_post(&state.zola_root, &relative)?;
    let current_revision = revision(&read_post(&file)?);
    if base_revision != current_revision {
        return Err(revision_conflict(current_revision));
    }
    fs::remove_file(&file)
        .map_err(|error| internal_error(format!("failed to delete post: {error}")))?;
    Ok(Json(json!({})))
}

pub async fn recover(
    State(state): State<Arc<AppState>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> ApiResult {
    let request = json_body(payload)?;
    let content = body_string(&request, "content")?;
    let slug = body_string(&request, "slug")?;
    validate_slug(slug)?;
    let year = site::post_date(content)
        .as_deref()
        .and_then(date_year)
        .unwrap_or_else(|| Utc::now().year());
    let relative = PathBuf::from(format!("post/{year}/{slug}.md"));
    let path_string = content_path_string(&relative);
    let metadata = site::post_from_content(&relative, content);

    let _guard = state.coordinator.lock().await;
    let file = resolve_new_post(&state.zola_root, &relative)?;
    if directory_entry_exists(&file).map_err(internal_error)? {
        return Err(filename_collision(&path_string, &metadata.url));
    }
    ensure_url_available(&state.zola_root, &relative, content.as_bytes(), None)?;
    ensure_post_parent(&state.zola_root, &file)?;
    create_new_file(&file, content.as_bytes()).map_err(|error| match error.kind() {
        std::io::ErrorKind::AlreadyExists => filename_collision(&path_string, &metadata.url),
        _ => internal_error(format!("failed to create post: {error}")),
    })?;

    Ok(Json(json!({
        "path": path_string,
        "revision": revision(content.as_bytes()),
        "url": metadata.url,
    })))
}

pub async fn preview_check(
    State(state): State<Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult {
    let path = query_value(&query, "path")?;
    let relative = validate_post_path(path)?;
    let file = resolve_existing_post(&state.zola_root, &relative)?;
    let content = String::from_utf8(read_post(&file)?)
        .map_err(|_| internal_error("post is not valid UTF-8"))?;
    let metadata = site::post_from_content(&relative, &content);
    let response = state
        .http
        .get(format!("http://127.0.0.1:1111{}", metadata.url))
        .send()
        .await
        .map_err(|error| {
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("preview unavailable: {error}") })),
            )
        })?;
    if !response.status().is_success() {
        return Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "error": format!("preview returned {}", response.status()),
            })),
        ));
    }
    let body = response.bytes().await.map_err(|error| {
        (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "error": format!("failed to read preview: {error}") })),
        )
    })?;
    Ok(Json(json!({ "digest": preview_digest(&body) })))
}

fn preview_digest(bytes: &[u8]) -> String {
    site::revision(bytes)
}

pub fn revision(bytes: &[u8]) -> String {
    site::revision(bytes)
}

fn validate_post_path(path: &str) -> Result<PathBuf, ApiError> {
    site::validate_content_post_path(path).map_err(bad_request)
}

fn resolve_existing_post(zola_root: &Path, relative: &Path) -> Result<PathBuf, ApiError> {
    let file = zola_root.join("content").join(relative);
    if !file.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "post not found" })),
        ));
    }
    if !file.is_file() {
        return Err(bad_request("post path is not a file"));
    }
    confine_existing(zola_root, &file)?;
    Ok(file)
}

fn resolve_new_post(zola_root: &Path, relative: &Path) -> Result<PathBuf, ApiError> {
    let post_root = zola_root.join("content/post");
    let canonical_root = post_root
        .canonicalize()
        .map_err(|error| internal_error(format!("failed to resolve post directory: {error}")))?;
    let file = zola_root.join("content").join(relative);
    let parent = file
        .parent()
        .ok_or_else(|| bad_request("invalid post path"))?;
    let mut existing_parent = parent;
    while !existing_parent.exists() {
        existing_parent = existing_parent
            .parent()
            .ok_or_else(|| bad_request("post path has no existing parent"))?;
    }
    let canonical_existing_parent = existing_parent
        .canonicalize()
        .map_err(|error| internal_error(format!("failed to resolve post directory: {error}")))?;
    if !canonical_existing_parent.starts_with(&canonical_root) {
        return Err(bad_request("post path escapes content/post"));
    }
    Ok(file)
}

fn ensure_post_parent(zola_root: &Path, file: &Path) -> Result<(), ApiError> {
    let canonical_root = zola_root
        .join("content/post")
        .canonicalize()
        .map_err(|error| internal_error(format!("failed to resolve post directory: {error}")))?;
    let parent = file
        .parent()
        .ok_or_else(|| bad_request("invalid post path"))?;
    fs::create_dir_all(parent)
        .map_err(|error| internal_error(format!("failed to create post directory: {error}")))?;
    let canonical_parent = parent
        .canonicalize()
        .map_err(|error| internal_error(format!("failed to resolve post directory: {error}")))?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(bad_request("post path escapes content/post"));
    }
    Ok(())
}

fn confine_existing(zola_root: &Path, file: &Path) -> Result<(), ApiError> {
    let canonical_root = zola_root
        .join("content/post")
        .canonicalize()
        .map_err(|error| internal_error(format!("failed to resolve post directory: {error}")))?;
    let canonical_file = file
        .canonicalize()
        .map_err(|error| internal_error(format!("failed to resolve post path: {error}")))?;
    if !canonical_file.starts_with(canonical_root) {
        return Err(bad_request("post path escapes content/post"));
    }
    Ok(())
}

fn read_post(file: &Path) -> Result<Vec<u8>, ApiError> {
    fs::read(file).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "post not found" })),
        ),
        _ => internal_error(format!("failed to read post: {error}")),
    })
}

fn ensure_url_available(
    zola_root: &Path,
    candidate_path: &Path,
    candidate_content: &[u8],
    excluded_path: Option<&Path>,
) -> Result<(), ApiError> {
    let candidate_path = content_path_string(candidate_path);
    let excluded_path = excluded_path.map(content_path_string);
    if let Some(conflict) = site::find_url_collision(
        zola_root,
        &candidate_path,
        candidate_content,
        excluded_path.as_deref(),
    )
    .map_err(internal_error)?
    {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({
                "error": "effective URL is already used by another post",
                "conflicting_path": conflict.path,
                "conflicting_url": conflict.url,
            })),
        ));
    }
    Ok(())
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temp = unique_temp_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|error| format!("failed to create temporary file: {error}"))?;
        file.write_all(bytes)
            .map_err(|error| format!("failed to write temporary file: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("failed to flush temporary file: {error}"))?;
        fs::rename(&temp, path)
            .map_err(|error| format!("failed to replace post atomically: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn create_new_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

fn directory_entry_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("failed to inspect destination post: {error}")),
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn rename_without_overwrite(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    const AT_FDCWD: c_int = -100;
    const RENAME_NOREPLACE: u32 = 1;

    unsafe extern "C" {
        fn renameat2(
            olddirfd: c_int,
            oldpath: *const c_char,
            newdirfd: c_int,
            newpath: *const c_char,
            flags: u32,
        ) -> c_int;
    }

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let destination = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // SAFETY: both C strings remain alive for the call and are NUL-terminated.
    let result = unsafe {
        renameat2(
            AT_FDCWD,
            source.as_ptr(),
            AT_FDCWD,
            destination.as_ptr(),
            RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn rename_without_overwrite(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::hard_link(source, destination)?;
    if let Err(error) = fs::remove_file(source) {
        let _ = fs::remove_file(destination);
        return Err(error);
    }
    Ok(())
}

fn unique_temp_path(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(
        ".{name}.blogger-{}-{nanos}-{sequence}.tmp",
        std::process::id()
    ))
}

fn serialize_new_post(title: &str, date: &str, draft: bool) -> String {
    let title = serde_json::to_string(title).expect("serializing a string cannot fail");
    let date = serde_json::to_string(date).expect("serializing a string cannot fail");
    let draft = if draft { "draft = true\n" } else { "" };
    format!("+++\ndate = {date}\ntitle = {title}\n{draft}\n[taxonomies]\ntags = []\n+++\n")
}

fn date_year(date: &str) -> Option<i32> {
    if !site::valid_post_date(date) {
        return None;
    }
    date.get(..10)
        .and_then(|prefix| chrono::NaiveDate::parse_from_str(prefix, "%Y-%m-%d").ok())
        .map(|date| date.year())
}

fn validate_slug(slug: &str) -> Result<(), ApiError> {
    if slug.is_empty() || slug::slugify(slug) != slug {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "slug must be a non-empty normalized slug" })),
        ));
    }
    Ok(())
}

fn normalize_filename(filename: &str) -> Result<String, ApiError> {
    let normalized = slug::slugify(filename);
    if normalized.is_empty() {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "new filename must contain slug characters" })),
        ));
    }
    Ok(normalized)
}

fn content_path_string(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn json_body(payload: Result<Json<Value>, JsonRejection>) -> Result<Value, ApiError> {
    payload.map(|Json(value)| value).map_err(|error| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": format!("invalid request body: {}", error.body_text()) })),
        )
    })
}

fn body_string<'a>(body: &'a Value, field: &str) -> Result<&'a str, ApiError> {
    body.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| bad_request(format!("missing or invalid '{field}'")))
}

fn body_bool(body: &Value, field: &str) -> Result<bool, ApiError> {
    body.get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| bad_request(format!("missing or invalid '{field}'")))
}

fn query_value<'a>(query: &'a HashMap<String, String>, field: &str) -> Result<&'a str, ApiError> {
    query
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| bad_request(format!("missing '{field}' query parameter")))
}

fn filename_collision(path: &str, url: &str) -> ApiError {
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        Json(json!({
            "error": "destination post already exists",
            "conflicting_path": path,
            "conflicting_url": url,
        })),
    )
}

fn revision_conflict(current_revision: String) -> ApiError {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": "post changed on disk",
            "current_revision": current_revision,
        })),
    )
}

fn deleted_conflict() -> ApiError {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": "post was deleted on disk",
            "current_revision": Value::Null,
            "deleted": true,
        })),
    )
}

fn bad_request(message: impl ToString) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message.to_string() })),
    )
}

fn internal_error(message: impl ToString) -> ApiError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": message.to_string() })),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{
        date_year, preview_digest, rename_without_overwrite, resolve_existing_post,
        resolve_new_post, revision, serialize_new_post,
    };

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
                "blogger-posts-test-{}-{nanos}-{sequence}",
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
    fn computes_sha256_revision() {
        assert_eq!(
            revision(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn preview_digest_distinguishes_equal_length_pages() {
        let before = b"<main><p>cat</p></main>";
        let after = b"<main><p>dog</p></main>";

        assert_eq!(before.len(), after.len());
        assert_ne!(preview_digest(before), preview_digest(after));
    }

    #[test]
    fn serializes_new_post_and_escapes_title() {
        let content =
            serialize_new_post(r#"A "quoted" \\ title"#, "2026-08-12T09:30:00+00:00", true);
        let parsed = crate::site::parse_front_matter(content.as_bytes());
        assert_eq!(parsed.title.as_deref(), Some(r#"A "quoted" \\ title"#));
        assert!(content.contains("\\\"quoted\\\""));
        assert!(content.ends_with("draft = true\n\n[taxonomies]\ntags = []\n+++\n"));

        let published = serialize_new_post("Published", "2026-08-12T09:30:00Z", false);
        assert_eq!(
            published,
            "+++\ndate = \"2026-08-12T09:30:00Z\"\ntitle = \"Published\"\n\n[taxonomies]\ntags = []\n+++\n"
        );
        assert!(!published.contains("draft ="));
        assert!(published.contains("\n\n[taxonomies]\n"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_post_paths_through_escaping_symlink_parents() {
        use std::os::unix::fs::symlink;

        let tree = TempTree::new();
        let site = tree.path().join("site");
        let outside = tree.path().join("outside");
        fs::create_dir_all(site.join("content/post")).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("existing.md"), "outside").unwrap();
        symlink(&outside, site.join("content/post/escape")).unwrap();

        assert!(
            resolve_existing_post(&site, Path::new("post/escape/existing.md"))
                .unwrap_err()
                .0
                .is_client_error()
        );
        assert!(
            resolve_new_post(&site, Path::new("post/escape/new.md"))
                .unwrap_err()
                .0
                .is_client_error()
        );
        assert!(!outside.join("new.md").exists());
    }

    #[test]
    fn rename_never_overwrites_and_recovery_year_requires_a_complete_date() {
        let tree = TempTree::new();
        let source = tree.path().join("source.md");
        let destination = tree.path().join("destination.md");
        fs::write(&source, "source").unwrap();
        fs::write(&destination, "destination").unwrap();

        assert_eq!(
            rename_without_overwrite(&source, &destination)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read_to_string(&source).unwrap(), "source");
        assert_eq!(fs::read_to_string(&destination).unwrap(), "destination");
        assert_eq!(date_year("2026-08-12"), Some(2026));
        assert_eq!(date_year("2026-08-12T10:30:00Z"), Some(2026));
        assert_eq!(date_year("2026-08-12garbage"), None);
    }
}
