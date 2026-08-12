use std::{cmp::Ordering, fs, path::Path};

use serde::Serialize;

use crate::site;

pub const MAX_SEARCH_RESULTS: usize = 20;

const MAX_EXCERPT_CHARS: usize = 240;
const EXCERPT_CONTEXT_CHARS: usize = 80;

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

    use super::{MAX_EXCERPT_CHARS, MAX_SEARCH_RESULTS, load_post, search_posts};

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
}
