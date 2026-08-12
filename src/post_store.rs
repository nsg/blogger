use std::{
    cmp::Ordering,
    fmt,
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering as AtomicOrdering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{Datelike, Utc};
use serde::Serialize;
use tokio::sync::Mutex;

use crate::site;

pub const MAX_SEARCH_RESULTS: usize = 20;

const MAX_EXCERPT_CHARS: usize = 240;
const EXCERPT_CONTEXT_CHARS: usize = 80;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PostDocument {
    pub path: String,
    pub title: String,
    pub date: Option<String>,
    pub draft: bool,
    pub unsorted: bool,
    pub url: String,
    pub revision: String,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PostMatch {
    pub path: String,
    pub title: String,
    pub date: Option<String>,
    pub draft: bool,
    pub url: String,
    pub excerpt: String,
}

#[derive(Clone, Debug)]
pub struct DraftStore {
    zola_root: Arc<PathBuf>,
    coordinator: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactReplacement {
    pub old_text: String,
    pub new_text: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AppendSeparator {
    None,
    Newline,
    BlankLine,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DraftMutation {
    pub message: String,
    pub path: String,
    pub title: String,
    pub draft: bool,
    pub url: String,
    pub revision: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DraftError {
    pub error: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replacement_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_count: Option<usize>,
}

impl fmt::Display for DraftError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl DraftStore {
    pub fn new(zola_root: PathBuf, coordinator: Arc<Mutex<()>>) -> Self {
        Self {
            zola_root: Arc::new(zola_root),
            coordinator,
        }
    }

    pub fn load_post(&self, path: &str) -> Result<PostDocument, String> {
        load_post(&self.zola_root, path)
    }

    pub fn search_posts(&self, query: &str, limit: usize) -> Result<Vec<PostMatch>, String> {
        search_posts(&self.zola_root, query, limit)
    }

    pub async fn create_draft(
        &self,
        front_matter: &str,
        body: &str,
        requested_slug: Option<&str>,
    ) -> Result<DraftMutation, DraftError> {
        let prepared = prepare_document(front_matter, body)?;
        let slug = match requested_slug {
            Some(slug) => validate_slug(slug)?,
            None => {
                let generated = slug::slugify(&prepared.title);
                if generated.is_empty() {
                    return Err(draft_error(
                        "invalid_slug",
                        "The title cannot be converted into a filename slug. Supply a non-empty normalized slug.",
                    ));
                }
                generated
            }
        };
        let year = prepared
            .date
            .as_deref()
            .and_then(date_year)
            .unwrap_or_else(|| Utc::now().year());
        let path = format!("post/{year}/{slug}.md");

        let _guard = self.coordinator.lock().await;
        let file = resolve_new_post(&self.zola_root, year, &slug)?;
        if entry_exists(&file)? {
            return Err(collision_error(&path));
        }
        ensure_url_available(&self.zola_root, &path, prepared.content.as_bytes(), None)?;
        create_new_file(&file, prepared.content.as_bytes())?;

        Ok(mutation(
            format!("Created draft {:?} at {path}.", prepared.title),
            &path,
            &prepared.content,
        ))
    }

    pub async fn replace_draft(
        &self,
        path: &str,
        expected_revision: &str,
        front_matter: &str,
        body: &str,
    ) -> Result<DraftMutation, DraftError> {
        let prepared = prepare_document(front_matter, body)?;
        self.update_draft(path, expected_revision, move |_, _| Ok(prepared.content))
            .await
            .map(|mut result| {
                result.message = format!("Replaced draft {:?} at {path}.", result.title);
                result
            })
    }

    pub async fn edit_draft(
        &self,
        path: &str,
        expected_revision: &str,
        replacements: &[ExactReplacement],
    ) -> Result<DraftMutation, DraftError> {
        if replacements.is_empty() {
            return Err(draft_error(
                "invalid_replacements",
                "Provide at least one exact body replacement.",
            ));
        }
        self.update_draft(path, expected_revision, |content, body_start| {
            let body = &content[body_start..];
            let mut matches = Vec::with_capacity(replacements.len());
            for (index, replacement) in replacements.iter().enumerate() {
                if replacement.old_text.is_empty() {
                    return Err(replacement_error(
                        "invalid_replacement",
                        "Replacement old_text must not be empty.",
                        index,
                        None,
                    ));
                }
                let positions = body
                    .match_indices(&replacement.old_text)
                    .map(|(start, _)| start)
                    .collect::<Vec<_>>();
                if positions.len() != 1 {
                    let message = if positions.is_empty() {
                        format!(
                            "Replacement {} did not match the draft body. Call get_post and retry with the exact current text.",
                            index + 1
                        )
                    } else {
                        format!(
                            "Replacement {} matched {} places in the draft body. Provide a longer unique passage.",
                            index + 1,
                            positions.len()
                        )
                    };
                    return Err(replacement_error(
                        if positions.is_empty() {
                            "replacement_not_found"
                        } else {
                            "replacement_ambiguous"
                        },
                        message,
                        index,
                        Some(positions.len()),
                    ));
                }
                let start = positions[0];
                matches.push((
                    start,
                    start + replacement.old_text.len(),
                    index,
                    replacement.new_text.as_str(),
                ));
            }
            matches.sort_by_key(|entry| entry.0);
            for pair in matches.windows(2) {
                if pair[0].1 > pair[1].0 {
                    return Err(replacement_error(
                        "replacement_overlap",
                        "Exact replacements overlap. Combine them into one replacement and retry.",
                        pair[1].2,
                        None,
                    ));
                }
            }

            let mut next_body = String::with_capacity(body.len());
            let mut cursor = 0;
            for (start, end, _, new_text) in matches {
                next_body.push_str(&body[cursor..start]);
                next_body.push_str(new_text);
                cursor = end;
            }
            next_body.push_str(&body[cursor..]);
            let mut next = content[..body_start].to_owned();
            next.push_str(&next_body);
            Ok(next)
        })
        .await
        .map(|mut result| {
            result.message = format!(
                "Applied {} exact body replacement{} to draft {:?} at {path}.",
                replacements.len(),
                if replacements.len() == 1 { "" } else { "s" },
                result.title
            );
            result
        })
    }

    pub async fn append_draft(
        &self,
        path: &str,
        expected_revision: &str,
        text: &str,
        separator: AppendSeparator,
    ) -> Result<DraftMutation, DraftError> {
        self.update_draft(path, expected_revision, |content, _| {
            let separator = match separator {
                AppendSeparator::None => "",
                AppendSeparator::Newline => "\n",
                AppendSeparator::BlankLine => "\n\n",
            };
            let mut next = String::with_capacity(content.len() + separator.len() + text.len());
            next.push_str(content);
            next.push_str(separator);
            next.push_str(text);
            Ok(next)
        })
        .await
        .map(|mut result| {
            result.message = format!("Appended text to draft {:?} at {path}.", result.title);
            result
        })
    }

    async fn update_draft(
        &self,
        path: &str,
        expected_revision: &str,
        update: impl FnOnce(&str, usize) -> Result<String, DraftError>,
    ) -> Result<DraftMutation, DraftError> {
        let _guard = self.coordinator.lock().await;
        let file = resolve_existing_post(&self.zola_root, path)?;
        let bytes = fs::read(&file)
            .map_err(|_| draft_error("post_read_failed", "The draft could not be read safely."))?;
        let content = String::from_utf8(bytes.clone()).map_err(|_| {
            draft_error(
                "invalid_post_encoding",
                "The draft is not valid UTF-8 and cannot be modified through MCP.",
            )
        })?;
        let current = site::metadata_from_bytes(path, &bytes);
        if !current.draft {
            return Err(draft_error(
                "published_post",
                "MCP may modify drafts only; this post is published and was not changed.",
            ));
        }
        if expected_revision != current.revision {
            return Err(DraftError {
                error: "revision_conflict",
                message: "The draft changed after it was read. Call get_post, review the current content, and retry with its new revision.".to_owned(),
                current_revision: Some(current.revision),
                replacement_index: None,
                match_count: None,
            });
        }
        let (_, _, body_start) = split_document(&content).map_err(|message| {
            draft_error(
                "invalid_front_matter",
                format!("The existing draft has invalid front matter: {message}"),
            )
        })?;
        let next = update(&content, body_start)?;
        let next_metadata = site::metadata_from_bytes(path, next.as_bytes());
        if !next_metadata.draft {
            return Err(draft_error(
                "draft_required",
                "The requested change would remove draft status, so nothing was written.",
            ));
        }
        ensure_url_available(&self.zola_root, path, next.as_bytes(), Some(path))?;
        atomic_replace(&file, next.as_bytes())?;
        Ok(mutation(String::new(), path, &next))
    }
}

pub fn load_post(zola_root: &Path, path: &str) -> Result<PostDocument, String> {
    let relative = site::validate_content_post_path(path)?;
    let post_root = zola_root.join("content/post");
    let canonical_post_root = post_root.canonicalize().map_err(|error| {
        format!(
            "failed to resolve post directory {}: {error}",
            post_root.display()
        )
    })?;
    let candidate = zola_root.join("content").join(relative);
    let canonical_file = candidate
        .canonicalize()
        .map_err(|error| format!("failed to resolve post {path}: {error}"))?;

    if canonical_file == canonical_post_root || !canonical_file.starts_with(&canonical_post_root) {
        return Err(format!("post path resolves outside content/post: {path}"));
    }
    if !canonical_file.is_file() {
        return Err(format!("post path is not a regular file: {path}"));
    }

    let bytes = fs::read(&canonical_file)
        .map_err(|error| format!("failed to read post {path}: {error}"))?;
    let content =
        String::from_utf8(bytes.clone()).map_err(|_| format!("post is not valid UTF-8: {path}"))?;
    let metadata = site::metadata_from_bytes(path, &bytes);
    Ok(PostDocument {
        path: metadata.path,
        title: metadata.title,
        date: metadata.date,
        draft: metadata.draft,
        unsorted: metadata.unsorted,
        url: metadata.url,
        revision: metadata.revision,
        content,
    })
}

pub fn search_posts(zola_root: &Path, query: &str, limit: usize) -> Result<Vec<PostMatch>, String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("search query must not be empty".to_owned());
    }

    let lowercase_query = query.to_lowercase();
    let mut matches = site::scan_posts(zola_root)?
        .into_iter()
        .map(|metadata| load_post(zola_root, &metadata.path))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter_map(|post| {
            let title_match = post.title.to_lowercase().contains(&lowercase_query);
            let content_match = post.content.to_lowercase().contains(&lowercase_query);
            (title_match || content_match).then(|| RankedMatch {
                excerpt: matching_excerpt(&post.content, query),
                post,
                title_match,
            })
        })
        .collect::<Vec<_>>();

    matches.sort_by(|left, right| {
        right
            .title_match
            .cmp(&left.title_match)
            .then_with(|| compare_dates_descending(&left.post.date, &right.post.date))
            .then_with(|| left.post.path.cmp(&right.post.path))
    });

    Ok(matches
        .into_iter()
        .take(limit.min(MAX_SEARCH_RESULTS))
        .map(|entry| PostMatch {
            path: entry.post.path,
            title: entry.post.title,
            date: entry.post.date,
            draft: entry.post.draft,
            url: entry.post.url,
            excerpt: entry.excerpt,
        })
        .collect())
}

struct PreparedDocument {
    content: String,
    title: String,
    date: Option<String>,
}

fn prepare_document(front_matter: &str, body: &str) -> Result<PreparedDocument, DraftError> {
    if front_matter.lines().any(|line| line.trim() == "+++") {
        return Err(draft_error(
            "invalid_front_matter",
            "Front matter must be raw TOML without the surrounding +++ delimiters.",
        ));
    }
    let mut table = toml::from_str::<toml::Table>(front_matter).map_err(|error| {
        draft_error(
            "invalid_front_matter",
            format!(
                "Front matter is not valid TOML: {error}. Send raw TOML without +++ delimiters."
            ),
        )
    })?;
    let title = table
        .get("title")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            draft_error(
                "invalid_front_matter",
                "Front matter must contain a non-empty TOML string named title.",
            )
        })?;
    let date = match table.get("date") {
        Some(toml::Value::String(value)) => Some(value.clone()),
        Some(toml::Value::Datetime(value)) => Some(value.to_string()),
        Some(_) => {
            return Err(draft_error(
                "invalid_front_matter",
                "The front matter date must be a TOML date, datetime, or string.",
            ));
        }
        None => None,
    };
    table.insert("draft".to_owned(), toml::Value::Boolean(true));
    let mut normalized = toml::to_string(&table).map_err(|error| {
        draft_error(
            "invalid_front_matter",
            format!("Front matter could not be normalized: {error}"),
        )
    })?;
    if !normalized.ends_with('\n') {
        normalized.push('\n');
    }
    let content = format!("+++\n{normalized}+++\n{body}");
    Ok(PreparedDocument {
        content,
        title,
        date,
    })
}

fn split_document(content: &str) -> Result<(&str, &str, usize), String> {
    let mut offset = 0;
    let mut lines = content.split_inclusive('\n');
    let first = lines
        .next()
        .ok_or_else(|| "the document is empty".to_owned())?;
    if first.trim_end_matches(['\r', '\n']) != "+++" {
        return Err("the opening +++ delimiter is missing".to_owned());
    }
    offset += first.len();
    let front_matter_start = offset;
    for line in lines {
        let line_start = offset;
        offset += line.len();
        if line.trim_end_matches(['\r', '\n']) == "+++" {
            let front_matter =
                content[front_matter_start..line_start].trim_end_matches(['\r', '\n']);
            return Ok((front_matter, &content[offset..], offset));
        }
    }
    Err("the closing +++ delimiter is missing".to_owned())
}

fn resolve_existing_post(zola_root: &Path, path: &str) -> Result<PathBuf, DraftError> {
    let relative = site::validate_content_post_path(path)
        .map_err(|message| draft_error("invalid_post_path", message))?;
    let canonical_root = canonical_post_root(zola_root)?;
    let file = zola_root.join("content").join(relative);
    let canonical_file = file.canonicalize().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            draft_error("post_not_found", format!("No blog post exists at {path}."))
        } else {
            draft_error(
                "post_read_failed",
                "The post path could not be resolved safely.",
            )
        }
    })?;
    if canonical_file == canonical_root || !canonical_file.starts_with(&canonical_root) {
        return Err(draft_error(
            "invalid_post_path",
            "The post path resolves outside content/post and was rejected.",
        ));
    }
    if !canonical_file.is_file() {
        return Err(draft_error(
            "invalid_post_path",
            "The post path is not a regular Markdown file.",
        ));
    }
    Ok(canonical_file)
}

fn resolve_new_post(zola_root: &Path, year: i32, slug: &str) -> Result<PathBuf, DraftError> {
    let canonical_root = canonical_post_root(zola_root)?;
    let parent = zola_root.join("content/post").join(year.to_string());
    match fs::create_dir(&parent) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => {
            return Err(draft_error(
                "post_create_failed",
                "The draft year directory could not be created.",
            ));
        }
    }
    let canonical_parent = parent.canonicalize().map_err(|_| {
        draft_error(
            "post_create_failed",
            "The draft year directory could not be resolved safely.",
        )
    })?;
    if canonical_parent == canonical_root || !canonical_parent.starts_with(&canonical_root) {
        return Err(draft_error(
            "invalid_post_path",
            "The generated draft path resolves outside content/post and was rejected.",
        ));
    }
    Ok(canonical_parent.join(format!("{slug}.md")))
}

fn canonical_post_root(zola_root: &Path) -> Result<PathBuf, DraftError> {
    zola_root.join("content/post").canonicalize().map_err(|_| {
        draft_error(
            "post_directory_unavailable",
            "The blog post directory could not be resolved safely.",
        )
    })
}

fn validate_slug(slug: &str) -> Result<String, DraftError> {
    if slug.is_empty() || slug::slugify(slug) != slug {
        return Err(draft_error(
            "invalid_slug",
            "The slug must be a non-empty normalized slug such as my-new-post.",
        ));
    }
    Ok(slug.to_owned())
}

fn date_year(date: &str) -> Option<i32> {
    if !site::valid_post_date(date) {
        return None;
    }
    date.get(..10)
        .and_then(|prefix| chrono::NaiveDate::parse_from_str(prefix, "%Y-%m-%d").ok())
        .map(|date| date.year())
}

fn ensure_url_available(
    zola_root: &Path,
    path: &str,
    content: &[u8],
    excluded_path: Option<&str>,
) -> Result<(), DraftError> {
    if let Some(conflict) = site::find_url_collision(zola_root, path, content, excluded_path)
        .map_err(|_| {
            draft_error(
                "url_check_failed",
                "Blogger could not safely check the draft URL for collisions.",
            )
        })?
    {
        return Err(DraftError {
            error: "url_collision",
            message: format!(
                "The effective URL {} is already used by {}. Choose another slug or front matter path.",
                conflict.url, conflict.path
            ),
            current_revision: None,
            replacement_index: None,
            match_count: None,
        });
    }
    Ok(())
}

fn collision_error(path: &str) -> DraftError {
    draft_error(
        "filename_collision",
        format!("A post already exists at {path}. Choose another slug."),
    )
}

fn entry_exists(path: &Path) -> Result<bool, DraftError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(draft_error(
            "post_create_failed",
            "Blogger could not safely inspect the requested draft path.",
        )),
    }
}

fn create_new_file(path: &Path, bytes: &[u8]) -> Result<(), DraftError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                draft_error(
                    "filename_collision",
                    "A post appeared at the requested path. Choose another slug.",
                )
            } else {
                draft_error("post_create_failed", "The draft file could not be created.")
            }
        })?;
    if file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .is_err()
    {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(draft_error(
            "post_create_failed",
            "The complete draft could not be written, so the partial file was removed.",
        ));
    }
    Ok(())
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), DraftError> {
    let temp = unique_temp_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .map_err(|_| ())?;
        file.write_all(bytes).map_err(|_| ())?;
        file.sync_all().map_err(|_| ())?;
        fs::rename(&temp, path).map_err(|_| ())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
        return Err(draft_error(
            "post_write_failed",
            "The draft could not be replaced atomically; the original file was left unchanged.",
        ));
    }
    Ok(())
}

fn unique_temp_path(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = TEMP_SEQUENCE.fetch_add(1, AtomicOrdering::Relaxed);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(
        ".{name}.blogger-mcp-{}-{nanos}-{sequence}.tmp",
        std::process::id()
    ))
}

fn mutation(message: String, path: &str, content: &str) -> DraftMutation {
    let metadata = site::metadata_from_bytes(path, content.as_bytes());
    DraftMutation {
        message,
        path: path.to_owned(),
        title: metadata.title,
        draft: metadata.draft,
        url: metadata.url,
        revision: metadata.revision,
    }
}

fn draft_error(error: &'static str, message: impl Into<String>) -> DraftError {
    DraftError {
        error,
        message: message.into(),
        current_revision: None,
        replacement_index: None,
        match_count: None,
    }
}

fn replacement_error(
    error: &'static str,
    message: impl Into<String>,
    index: usize,
    match_count: Option<usize>,
) -> DraftError {
    DraftError {
        error,
        message: message.into(),
        current_revision: None,
        replacement_index: Some(index),
        match_count,
    }
}

struct RankedMatch {
    post: PostDocument,
    title_match: bool,
    excerpt: String,
}

fn compare_dates_descending(left: &Option<String>, right: &Option<String>) -> Ordering {
    date_key(right).cmp(&date_key(left))
}

fn date_key(date: &Option<String>) -> Option<i64> {
    let date = date.as_deref()?;
    chrono::DateTime::parse_from_rfc3339(date)
        .map(|date| date.timestamp())
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(date, "%Y-%m-%dT%H:%M:%S%.f")
                .map(|date| date.and_utc().timestamp())
        })
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").map(|date| {
                date.and_hms_opt(0, 0, 0)
                    .expect("midnight is a valid time")
                    .and_utc()
                    .timestamp()
            })
        })
        .ok()
}

fn matching_excerpt(content: &str, query: &str) -> String {
    let Some((match_start_byte, match_end_byte)) = find_case_insensitive(content, query) else {
        return String::new();
    };
    let chars = content.chars().collect::<Vec<_>>();
    let match_start = content[..match_start_byte].chars().count();
    let match_end = match_start + content[match_start_byte..match_end_byte].chars().count();
    let start = match_start.saturating_sub(EXCERPT_CONTEXT_CHARS);
    let end = chars.len().min((start + MAX_EXCERPT_CHARS).max(match_end));
    let mut excerpt = chars[start..end]
        .iter()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    if start > 0 {
        excerpt.insert(0, '…');
    }
    if end < chars.len() {
        excerpt.push('…');
    }
    excerpt.chars().take(MAX_EXCERPT_CHARS).collect()
}

fn find_case_insensitive(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let needle = needle.to_lowercase();
    for (start, _) in haystack.char_indices() {
        let mut candidate = String::new();
        for (offset, character) in haystack[start..].char_indices() {
            candidate.extend(character.to_lowercase());
            if candidate == needle {
                return Some((start, start + offset + character.len_utf8()));
            }
            if !needle.starts_with(&candidate) {
                break;
            }
        }
    }
    None
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
        AppendSeparator, DraftStore, ExactReplacement, MAX_EXCERPT_CHARS, MAX_SEARCH_RESULTS,
        load_post, search_posts, split_document,
    };
    use std::sync::Arc;
    use tokio::sync::Mutex;

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TempSite(PathBuf);

    impl TempSite {
        fn new() -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "blogger-post-store-test-{}-{nanos}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(root.join("content/post")).unwrap();
            Self(root)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write_post(&self, path: &str, content: &[u8]) {
            let file = self.0.join("content").join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, content).unwrap();
        }
    }

    impl Drop for TempSite {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn post(title: &str, date: &str, draft: bool, body: &str) -> String {
        format!("+++\ntitle = {title:?}\ndate = {date:?}\ndraft = {draft}\n+++\n{body}\n")
    }

    fn store(site: &TempSite) -> DraftStore {
        DraftStore::new(site.path().to_owned(), Arc::new(Mutex::new(())))
    }

    fn body(document: &super::PostDocument) -> &str {
        split_document(&document.content).unwrap().1
    }

    #[test]
    fn loads_complete_utf8_post_and_metadata() {
        let site = TempSite::new();
        let content = post("An Café Post", "2026-08-12", true, "Full Markdown body.");
        site.write_post("post/2026/cafe.md", content.as_bytes());

        let loaded = load_post(site.path(), "post/2026/cafe.md").unwrap();

        assert_eq!(loaded.path, "post/2026/cafe.md");
        assert_eq!(loaded.title, "An Café Post");
        assert_eq!(loaded.date.as_deref(), Some("2026-08-12"));
        assert!(loaded.draft);
        assert_eq!(loaded.url, "/post/2026/cafe/");
        assert_eq!(loaded.content, content);
        assert_eq!(loaded.revision.len(), 64);
    }

    #[test]
    fn rejects_invalid_paths_and_non_utf8_posts() {
        let site = TempSite::new();
        site.write_post("post/binary.md", &[0xff, 0xfe]);

        assert!(load_post(site.path(), "../secret.md").is_err());
        assert!(load_post(site.path(), "/post/binary.md").is_err());
        assert!(load_post(site.path(), "post/_index.md").is_err());
        assert!(
            load_post(site.path(), "post/binary.md")
                .unwrap_err()
                .contains("UTF-8")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_escape_the_post_directory() {
        use std::os::unix::fs::symlink;

        let site = TempSite::new();
        let outside = site.path().join("secret.md");
        fs::write(&outside, post("Secret", "2026-08-12", false, "private")).unwrap();
        symlink(&outside, site.path().join("content/post/escape.md")).unwrap();

        let error = load_post(site.path(), "post/escape.md").unwrap_err();
        assert!(error.contains("outside content/post"));
    }

    #[test]
    fn searches_titles_and_complete_markdown_case_insensitively() {
        let site = TempSite::new();
        site.write_post(
            "post/2026/title.md",
            post("Rust Notes", "2024-01-01", false, "unrelated").as_bytes(),
        );
        site.write_post(
            "post/2026/body.md",
            post("A Draft", "2026-08-12", true, "Learning RUST today").as_bytes(),
        );

        let matches = search_posts(site.path(), "rust", 20).unwrap();

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].path, "post/2026/title.md");
        assert_eq!(matches[1].path, "post/2026/body.md");
        assert!(matches[1].draft);
        assert!(matches[1].excerpt.to_lowercase().contains("rust"));
    }

    #[test]
    fn ranks_equal_match_types_by_date_then_path() {
        let site = TempSite::new();
        site.write_post(
            "post/z.md",
            post("Match", "2025-01-01", false, "body").as_bytes(),
        );
        site.write_post(
            "post/b.md",
            post("Match", "2026-01-01", false, "body").as_bytes(),
        );
        site.write_post(
            "post/a.md",
            post("Match", "2026-01-01", true, "body").as_bytes(),
        );

        let paths = search_posts(site.path(), "match", 20)
            .unwrap()
            .into_iter()
            .map(|entry| entry.path)
            .collect::<Vec<_>>();

        assert_eq!(paths, ["post/a.md", "post/b.md", "post/z.md"]);
    }

    #[test]
    fn rejects_blank_queries_and_honors_result_bounds() {
        let site = TempSite::new();
        for index in 0..25 {
            site.write_post(
                &format!("post/{index:02}.md"),
                post("Needle", "2026-01-01", false, "body").as_bytes(),
            );
        }

        assert!(search_posts(site.path(), " \n\t", 5).is_err());
        assert!(search_posts(site.path(), "needle", 0).unwrap().is_empty());
        assert_eq!(
            search_posts(site.path(), "needle", usize::MAX)
                .unwrap()
                .len(),
            MAX_SEARCH_RESULTS
        );
    }

    #[test]
    fn bounds_long_unicode_excerpts() {
        let site = TempSite::new();
        let body = format!("{}NEEDLE{}", "é".repeat(300), "é".repeat(300));
        site.write_post(
            "post/long.md",
            post("Long", "2026-01-01", false, &body).as_bytes(),
        );

        let matches = search_posts(site.path(), "needle", 1).unwrap();

        assert!(matches[0].excerpt.contains("NEEDLE"));
        assert!(matches[0].excerpt.chars().count() <= MAX_EXCERPT_CHARS);
    }

    #[test]
    fn supports_unicode_case_insensitive_matching() {
        let site = TempSite::new();
        site.write_post(
            "post/unicode.md",
            post("Unicode", "2026-01-01", false, "STRAßE and İstanbul").as_bytes(),
        );

        let matches = search_posts(site.path(), "İSTANBUL", 5).unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0].excerpt.contains("İstanbul"));
    }

    #[tokio::test]
    async fn creates_a_forced_draft_with_generated_or_explicit_slug() {
        let site = TempSite::new();
        let store = store(&site);
        let front_matter =
            "title = \"Voice Notes\"\ndate = 2026-08-12\ndraft = false\ncustom = \"kept\"";

        let created = store
            .create_draft(front_matter, "First paragraph.", None)
            .await
            .unwrap();
        assert_eq!(created.path, "post/2026/voice-notes.md");
        assert!(created.draft);
        assert!(created.message.contains("Created draft"));
        let loaded = store.load_post(&created.path).unwrap();
        assert!(loaded.draft);
        assert_eq!(body(&loaded), "First paragraph.");
        assert!(
            split_document(&loaded.content)
                .unwrap()
                .0
                .contains("custom = \"kept\"")
        );

        let explicit = store
            .create_draft("title = \"Other title\"", "Body", Some("chosen-name"))
            .await
            .unwrap();
        assert!(explicit.path.ends_with("/chosen-name.md"));
    }

    #[tokio::test]
    async fn rejects_bad_front_matter_and_creation_collisions() {
        let site = TempSite::new();
        let store = store(&site);

        let delimiters = store
            .create_draft("+++\ntitle = \"No\"\n+++", "", None)
            .await
            .unwrap_err();
        assert_eq!(delimiters.error, "invalid_front_matter");
        assert!(delimiters.message.contains("without"));

        store
            .create_draft("title = \"First\"\ndate = 2026-08-12", "", Some("same"))
            .await
            .unwrap();
        let collision = store
            .create_draft("title = \"Second\"\ndate = 2026-08-12", "", Some("same"))
            .await
            .unwrap_err();
        assert_eq!(collision.error, "filename_collision");
        assert!(collision.message.contains("another slug"));

        let url_collision = store
            .create_draft(
                "title = \"URL collision\"\ndate = 2026-08-12\nslug = \"same\"",
                "",
                Some("different-file"),
            )
            .await
            .unwrap_err();
        assert_eq!(url_collision.error, "url_collision");
        assert!(url_collision.message.contains("Choose another slug"));
    }

    #[tokio::test]
    async fn replaces_only_current_drafts_and_forces_draft_status() {
        let site = TempSite::new();
        site.write_post(
            "post/2026/draft.md",
            post("Draft", "2026-08-12", true, "Old body").as_bytes(),
        );
        site.write_post(
            "post/2026/published.md",
            post("Published", "2026-08-12", false, "Public body").as_bytes(),
        );
        let store = store(&site);
        let draft = store.load_post("post/2026/draft.md").unwrap();
        let replaced = store
            .replace_draft(
                &draft.path,
                &draft.revision,
                "title = \"Rewritten\"\ndate = 2026-08-12\ndraft = false",
                "New body",
            )
            .await
            .unwrap();
        assert!(replaced.draft);
        assert_eq!(body(&store.load_post(&draft.path).unwrap()), "New body");

        let published = store.load_post("post/2026/published.md").unwrap();
        let error = store
            .replace_draft(
                &published.path,
                &published.revision,
                "title = \"Sneaky\"",
                "Changed",
            )
            .await
            .unwrap_err();
        assert_eq!(error.error, "published_post");
        assert_eq!(
            body(&store.load_post(&published.path).unwrap()),
            "Public body\n"
        );

        let traversal = store
            .replace_draft(
                "post/../outside.md",
                &draft.revision,
                "title = \"Outside\"",
                "Changed",
            )
            .await
            .unwrap_err();
        assert_eq!(traversal.error, "invalid_post_path");
    }

    #[tokio::test]
    async fn exact_edits_are_atomic_unique_and_body_only() {
        let site = TempSite::new();
        let original = post(
            "Keep this title",
            "2026-08-12",
            true,
            "Alpha paragraph.\n\nRepeated.\nRepeated.",
        );
        site.write_post("post/2026/edit.md", original.as_bytes());
        let store = store(&site);
        let loaded = store.load_post("post/2026/edit.md").unwrap();
        let ambiguous = store
            .edit_draft(
                &loaded.path,
                &loaded.revision,
                &[
                    ExactReplacement {
                        old_text: "Alpha paragraph.".to_owned(),
                        new_text: "This must not land.".to_owned(),
                    },
                    ExactReplacement {
                        old_text: "Repeated.".to_owned(),
                        new_text: "Ambiguous.".to_owned(),
                    },
                ],
            )
            .await
            .unwrap_err();
        assert_eq!(ambiguous.error, "replacement_ambiguous");
        assert_eq!(ambiguous.replacement_index, Some(1));
        assert_eq!(ambiguous.match_count, Some(2));
        assert_eq!(
            fs::read_to_string(site.path().join("content/post/2026/edit.md")).unwrap(),
            original
        );

        let missing = store
            .edit_draft(
                &loaded.path,
                &loaded.revision,
                &[ExactReplacement {
                    old_text: "Not present".to_owned(),
                    new_text: "Nope".to_owned(),
                }],
            )
            .await
            .unwrap_err();
        assert_eq!(missing.error, "replacement_not_found");
        assert_eq!(missing.match_count, Some(0));

        let edited = store
            .edit_draft(
                &loaded.path,
                &loaded.revision,
                &[ExactReplacement {
                    old_text: "Alpha paragraph.".to_owned(),
                    new_text: "Revised paragraph.".to_owned(),
                }],
            )
            .await
            .unwrap();
        let current = store.load_post(&loaded.path).unwrap();
        assert_eq!(current.title, "Keep this title");
        assert!(body(&current).contains("Revised paragraph."));
        assert_ne!(edited.revision, loaded.revision);
    }

    #[tokio::test]
    async fn append_respects_separator_and_revisions() {
        let site = TempSite::new();
        site.write_post(
            "post/2026/append.md",
            post("Append", "2026-08-12", true, "First").as_bytes(),
        );
        let store = store(&site);
        let original = store.load_post("post/2026/append.md").unwrap();
        let appended = store
            .append_draft(
                &original.path,
                &original.revision,
                "Second",
                AppendSeparator::BlankLine,
            )
            .await
            .unwrap();
        assert!(body(&store.load_post(&original.path).unwrap()).ends_with("First\n\n\nSecond"));

        let conflict = store
            .append_draft(
                &original.path,
                &original.revision,
                "stale",
                AppendSeparator::None,
            )
            .await
            .unwrap_err();
        assert_eq!(conflict.error, "revision_conflict");
        assert_eq!(
            conflict.current_revision.as_deref(),
            Some(appended.revision.as_str())
        );
        assert!(conflict.message.contains("get_post"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn draft_writes_reject_symlink_escapes() {
        use std::os::unix::fs::symlink;

        let site = TempSite::new();
        let outside = site.path().join("outside.md");
        fs::write(&outside, post("Outside", "2026-08-12", true, "secret")).unwrap();
        symlink(&outside, site.path().join("content/post/escape-write.md")).unwrap();
        let store = store(&site);

        let error = store
            .append_draft(
                "post/escape-write.md",
                &crate::site::revision(fs::read(&outside).unwrap().as_slice()),
                "must not escape",
                AppendSeparator::None,
            )
            .await
            .unwrap_err();
        assert_eq!(error.error, "invalid_post_path");
        assert!(
            !fs::read_to_string(outside)
                .unwrap()
                .contains("must not escape")
        );
    }
}
