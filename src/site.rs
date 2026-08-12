use std::{
    fmt::Write as _,
    fs::{self, File, OpenOptions},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};

static PROBE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrontMatter {
    pub title: Option<String>,
    pub date: Option<String>,
    pub draft: bool,
    pub slug: Option<String>,
    pub path: Option<String>,
    pub unsorted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PostMetadata {
    pub path: String,
    pub title: String,
    pub date: Option<String>,
    pub draft: bool,
    pub unsorted: bool,
    pub url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UrlCollision {
    pub path: String,
    pub url: String,
}

pub fn discover(search_root: &Path) -> Result<PathBuf, String> {
    if !search_root.exists() {
        return Err(format!(
            "search root does not exist: {}",
            search_root.display()
        ));
    }
    if !search_root.is_dir() {
        return Err(format!(
            "search root is not a directory: {}",
            search_root.display()
        ));
    }

    discover_in(search_root)?.ok_or_else(|| {
        format!(
            "no Zola config.toml found beneath search root: {}",
            search_root.display()
        )
    })
}

pub fn validate(zola_root: &Path) -> Result<(), String> {
    let config_path = zola_root.join("config.toml");
    let config = fs::read_to_string(&config_path).map_err(|error| {
        format!(
            "failed to read Zola configuration {}: {error}",
            config_path.display()
        )
    })?;
    validate_slugify_config(&config)
        .map_err(|error| format!("{}: {error}", config_path.display()))?;

    let post_dir = zola_root.join("content/post");
    if !post_dir.is_dir() {
        return Err(format!(
            "required post directory does not exist: {}",
            post_dir.display()
        ));
    }
    validate_markdown_files(&post_dir)?;
    probe_directory(&post_dir, "post directory")?;
    validate_images_directory(&zola_root.join("static/images"))
}

pub fn scan_posts(zola_root: &Path) -> Result<Vec<PostMetadata>, String> {
    let post_root = zola_root.join("content/post");
    let mut posts = Vec::new();
    scan_posts_in(&post_root, &zola_root.join("content"), &mut posts)?;
    Ok(posts)
}

pub fn parse_front_matter(content: &[u8]) -> FrontMatter {
    let malformed = || FrontMatter {
        title: None,
        date: None,
        draft: false,
        slug: None,
        path: None,
        unsorted: true,
    };

    let Ok(content) = std::str::from_utf8(content) else {
        return malformed();
    };
    let mut lines = content.lines();
    if lines.next() != Some("+++") {
        return malformed();
    }

    let mut front_matter = String::new();
    let mut closed = false;
    for line in lines {
        if line == "+++" {
            closed = true;
            break;
        }
        front_matter.push_str(line);
        front_matter.push('\n');
    }
    if !closed {
        return malformed();
    }

    let Ok(value) = toml::from_str::<toml::Value>(&front_matter) else {
        return malformed();
    };
    let title = value
        .get("title")
        .and_then(toml::Value::as_str)
        .map(str::to_owned);
    let date = match value.get("date") {
        Some(toml::Value::String(date)) => Some(date.clone()),
        Some(toml::Value::Datetime(date)) => Some(date.to_string()),
        _ => None,
    };
    let draft = value
        .get("draft")
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);
    let slug = value
        .get("slug")
        .and_then(toml::Value::as_str)
        .map(str::to_owned);
    let path = value
        .get("path")
        .and_then(toml::Value::as_str)
        .map(str::to_owned);
    let unsorted = title.is_none() || date.as_deref().is_none_or(|date| !valid_post_date(date));

    FrontMatter {
        title,
        date,
        draft,
        slug,
        path,
        unsorted,
    }
}

pub fn metadata_from_bytes(content_relative_path: &str, content: &[u8]) -> PostMetadata {
    let front_matter = parse_front_matter(content);
    let fallback_title = Path::new(content_relative_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(content_relative_path)
        .to_owned();

    PostMetadata {
        path: content_relative_path.to_owned(),
        title: front_matter.title.clone().unwrap_or(fallback_title),
        date: front_matter.date.clone(),
        draft: front_matter.draft,
        unsorted: front_matter.unsorted,
        url: effective_url(content_relative_path, &front_matter),
    }
}

pub fn post_from_content(content_relative_path: &Path, content: &str) -> PostMetadata {
    let path = content_relative_path
        .components()
        .filter_map(|component| match component {
            Component::Normal(segment) => segment.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    metadata_from_bytes(&path, content.as_bytes())
}

pub fn post_date(content: &str) -> Option<String> {
    parse_front_matter(content.as_bytes())
        .date
        .filter(|date| valid_post_date(date))
}

pub fn valid_post_date(date: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(date).is_ok()
        || chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok()
        || chrono::NaiveDateTime::parse_from_str(date, "%Y-%m-%dT%H:%M:%S%.f").is_ok()
}

pub fn effective_url(content_relative_path: &str, front_matter: &FrontMatter) -> String {
    let segments: Vec<&str> = if let Some(path) = front_matter.path.as_deref() {
        path.split('/')
            .filter(|segment| !segment.is_empty())
            .collect()
    } else {
        let mut segments: Vec<&str> = content_relative_path.split('/').collect();
        let fallback = segments
            .pop()
            .and_then(|filename| filename.strip_suffix(".md"))
            .unwrap_or_default();
        segments.push(front_matter.slug.as_deref().unwrap_or(fallback));
        segments
    };
    let slugified = segments
        .into_iter()
        .map(slug::slugify)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/");

    if slugified.is_empty() {
        "/".to_owned()
    } else {
        format!("/{slugified}/")
    }
}

pub fn revision(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut revision = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(revision, "{byte:02x}").expect("writing to a string cannot fail");
    }
    revision
}

pub fn validate_post_path(content_relative_path: &str) -> Result<PathBuf, String> {
    if content_relative_path.is_empty() {
        return Err("post path must not be empty".to_owned());
    }
    if content_relative_path.contains('\0') {
        return Err("post path must not contain NUL bytes".to_owned());
    }
    if content_relative_path.contains('\\') {
        return Err("post path must use forward slashes".to_owned());
    }
    if Path::new(content_relative_path).is_absolute() || content_relative_path.starts_with('/') {
        return Err("post path must be relative to the content directory".to_owned());
    }

    let segments = content_relative_path.split('/').collect::<Vec<_>>();
    if segments.iter().any(|segment| segment.is_empty()) {
        return Err("post path must not contain empty segments".to_owned());
    }
    if segments
        .iter()
        .any(|segment| matches!(*segment, "." | ".."))
    {
        return Err("post path must not contain '.' or '..' segments".to_owned());
    }
    if segments.first() != Some(&"post") || segments.len() < 2 {
        return Err("post path must be inside content/post".to_owned());
    }
    if !content_relative_path.ends_with(".md") {
        return Err("post path must name a Markdown file".to_owned());
    }
    if segments.last() == Some(&"_index.md") {
        return Err("section _index.md files are not posts".to_owned());
    }

    let relative = PathBuf::from(content_relative_path);
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("post path contains invalid components".to_owned());
    }
    Ok(relative)
}

pub fn validate_content_post_path(content_relative_path: &str) -> Result<PathBuf, String> {
    validate_post_path(content_relative_path)
}

pub fn find_url_collision(
    zola_root: &Path,
    candidate_path: &str,
    candidate_content: &[u8],
    excluded_path: Option<&str>,
) -> Result<Option<UrlCollision>, String> {
    let candidate_url = metadata_from_bytes(candidate_path, candidate_content).url;
    let collision = scan_posts(zola_root)?
        .into_iter()
        .find(|post| Some(post.path.as_str()) != excluded_path && post.url == candidate_url);
    Ok(collision.map(|post| UrlCollision {
        path: post.path,
        url: post.url,
    }))
}

fn scan_posts_in(
    directory: &Path,
    content_root: &Path,
    posts: &mut Vec<PostMetadata>,
) -> Result<(), String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| {
            format!(
                "failed to scan post directory {}: {error}",
                directory.display()
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            format!(
                "failed to scan post directory {}: {error}",
                directory.display()
            )
        })?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to inspect post path {}: {error}", path.display()))?;
        if file_type.is_dir() {
            scan_posts_in(&path, content_root, posts)?;
            continue;
        }
        if !file_type.is_file()
            || path.extension().is_none_or(|extension| extension != "md")
            || path.file_name().is_some_and(|name| name == "_index.md")
        {
            continue;
        }

        let relative = path
            .strip_prefix(content_root)
            .map_err(|_| format!("post path is outside content directory: {}", path.display()))?;
        let relative = relative
            .to_str()
            .ok_or_else(|| format!("post path is not valid UTF-8: {}", relative.display()))?;
        let content = fs::read(&path)
            .map_err(|error| format!("failed to read post {}: {error}", path.display()))?;
        posts.push(metadata_from_bytes(relative, &content));
    }

    Ok(())
}

fn discover_in(directory: &Path) -> Result<Option<PathBuf>, String> {
    if directory.join("config.toml").is_file() {
        return Ok(Some(directory.to_path_buf()));
    }

    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("failed to scan directory {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to scan directory {}: {error}", directory.display()))?;
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let file_type = entry.file_type().map_err(|error| {
            format!("failed to inspect path {}: {error}", entry.path().display())
        })?;
        if !file_type.is_dir() || should_skip_directory(&entry.file_name()) {
            continue;
        }
        if let Some(root) = discover_in(&entry.path())? {
            return Ok(Some(root));
        }
    }

    Ok(None)
}

fn should_skip_directory(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    name.starts_with('.') || matches!(name.as_ref(), "node_modules" | "target")
}

fn validate_slugify_config(config: &str) -> Result<(), String> {
    let value: toml::Value = toml::from_str(config).map_err(|error: toml::de::Error| {
        format!("malformed Zola configuration: {}", error.message())
    })?;

    if let Some(paths) = value
        .get("slugify")
        .and_then(|slugify| slugify.get("paths"))
        && paths.as_str() != Some("on")
    {
        return Err(format!(
            "unsupported configuration [slugify].paths = {paths}; Blogger requires \"on\""
        ));
    }

    Ok(())
}

fn validate_markdown_files(directory: &Path) -> Result<(), String> {
    let entries = fs::read_dir(directory).map_err(|error| {
        format!(
            "failed to scan post directory {}: {error}",
            directory.display()
        )
    })?;

    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "failed to scan post directory {}: {error}",
                directory.display()
            )
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to inspect post path {}: {error}", path.display()))?;
        if file_type.is_dir() {
            validate_markdown_files(&path)?;
        } else if path.is_file() && path.extension().is_some_and(|extension| extension == "md") {
            File::open(&path).map_err(|error| {
                format!("Markdown post is not readable {}: {error}", path.display())
            })?;
        }
    }

    Ok(())
}

fn validate_images_directory(images_dir: &Path) -> Result<(), String> {
    if images_dir.exists() {
        if !images_dir.is_dir() {
            return Err(format!(
                "image directory path is not a directory: {}",
                images_dir.display()
            ));
        }
        return probe_directory(images_dir, "image directory");
    }

    let mut created = Vec::new();
    let mut path = images_dir;
    while !path.exists() {
        created.push(path.to_path_buf());
        path = path.parent().ok_or_else(|| {
            format!(
                "image directory has no existing parent: {}",
                images_dir.display()
            )
        })?;
    }
    if !path.is_dir() {
        return Err(format!(
            "nearest existing image parent is not a directory: {}",
            path.display()
        ));
    }

    fs::create_dir_all(images_dir).map_err(|error| {
        format!(
            "image directory cannot be created at {}: {error}",
            images_dir.display()
        )
    })?;

    let probe_result = probe_directory(images_dir, "image directory");
    let cleanup_result = cleanup_created_directories(&created);
    probe_result?;
    cleanup_result
}

fn cleanup_created_directories(created: &[PathBuf]) -> Result<(), String> {
    for path in created {
        fs::remove_dir(path).map_err(|error| {
            format!(
                "failed to clean up image-directory writability probe {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn probe_directory(directory: &Path, label: &str) -> Result<(), String> {
    let probe_path = directory.join(unique_probe_name());
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe_path)
        .map_err(|error| format!("{label} is not writable {}: {error}", directory.display()))?;
    drop(file);
    fs::remove_file(&probe_path).map_err(|error| {
        format!(
            "failed to remove writability probe {}: {error}",
            probe_path.display()
        )
    })
}

fn unique_probe_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = PROBE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(
        ".blogger-write-probe-{}-{nanos}-{sequence}",
        std::process::id()
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
        FrontMatter, discover, effective_url, find_url_collision, metadata_from_bytes,
        parse_front_matter, revision, scan_posts, validate, validate_post_path,
        validate_slugify_config,
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
                "blogger-site-test-{}-{nanos}-{sequence}",
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
    fn accepts_supported_slugify_configuration() {
        assert!(validate_slugify_config("base_url = 'https://example.com'").is_ok());
        assert!(validate_slugify_config("[slugify]\npaths = 'on'").is_ok());
    }

    #[test]
    fn rejects_unsupported_or_malformed_slugify_configuration() {
        let unsupported = validate_slugify_config("[slugify]\npaths = 'safe'").unwrap_err();
        assert!(unsupported.contains("unsupported"));

        let wrong_type = validate_slugify_config("[slugify]\npaths = false").unwrap_err();
        assert!(wrong_type.contains("unsupported"));

        let malformed = validate_slugify_config("[slugify\npaths = 'on'").unwrap_err();
        assert!(malformed.contains("malformed"));
    }

    #[test]
    fn discovers_and_validates_site_in_temporary_tree() {
        let tree = TempTree::new();
        fs::create_dir_all(tree.path().join(".hidden/site")).unwrap();
        fs::write(tree.path().join(".hidden/site/config.toml"), "not toml").unwrap();
        fs::create_dir_all(tree.path().join("node_modules/site")).unwrap();
        fs::write(
            tree.path().join("node_modules/site/config.toml"),
            "not toml",
        )
        .unwrap();

        let site = tree.path().join("workspace/blog");
        fs::create_dir_all(site.join("content/post/2026")).unwrap();
        fs::write(
            site.join("config.toml"),
            "base_url = 'https://example.com'\n[slugify]\npaths = 'on'\n",
        )
        .unwrap();
        fs::write(site.join("content/post/2026/hello.md"), "+++\n+++\n").unwrap();

        let discovered = discover(tree.path()).unwrap();
        assert_eq!(discovered, site);
        validate(&discovered).unwrap();
        assert!(!discovered.join("static/images").exists());
    }

    #[test]
    fn parses_valid_toml_front_matter_and_defaults_draft() {
        let parsed = parse_front_matter(
            b"+++\ntitle = \"Hello\"\ndate = \"2026-08-12T10:30:00Z\"\nslug = \"hello\"\npath = \"custom/hello\"\n+++\nBody\n",
        );
        assert_eq!(parsed.title.as_deref(), Some("Hello"));
        assert_eq!(parsed.date.as_deref(), Some("2026-08-12T10:30:00Z"));
        assert_eq!(parsed.slug.as_deref(), Some("hello"));
        assert_eq!(parsed.path.as_deref(), Some("custom/hello"));
        assert!(!parsed.draft);
        assert!(!parsed.unsorted);

        let draft =
            parse_front_matter(b"+++\ntitle = \"Draft\"\ndate = 2026-08-12\ndraft = true\n+++\n");
        assert!(draft.draft);
    }

    #[test]
    fn rejects_yaml_and_malformed_front_matter_without_failing_metadata() {
        for content in [
            b"---\ntitle: YAML\ndate: 2026-08-12\n---\n".as_slice(),
            b"+++\ntitle = [\n+++\n".as_slice(),
            b"+++\ntitle = \"No closing delimiter\"\n".as_slice(),
        ] {
            let metadata = metadata_from_bytes("post/2026/fallback.md", content);
            assert!(metadata.unsorted);
            assert_eq!(metadata.title, "fallback");
            assert_eq!(metadata.date, None);
            assert!(!metadata.draft);
        }
    }

    #[test]
    fn accepts_string_and_toml_datetime_dates() {
        let string_date =
            parse_front_matter(b"+++\ntitle = \"String\"\ndate = \"2026-08-12\"\n+++\n");
        assert_eq!(string_date.date.as_deref(), Some("2026-08-12"));
        assert!(!string_date.unsorted);

        let datetime =
            parse_front_matter(b"+++\ntitle = \"Datetime\"\ndate = 2026-08-12T10:30:00Z\n+++\n");
        assert_eq!(datetime.date.as_deref(), Some("2026-08-12T10:30:00Z"));
        assert!(!datetime.unsorted);

        let malformed_required =
            parse_front_matter(b"+++\ntitle = 12\ndate = false\ndraft = true\n+++\n");
        assert!(malformed_required.unsorted);
        assert_eq!(malformed_required.title, None);
        assert_eq!(malformed_required.date, None);
        assert!(malformed_required.draft);

        let invalid_string =
            parse_front_matter(b"+++\ntitle = \"Invalid\"\ndate = \"banana\"\n+++\n");
        assert_eq!(invalid_string.date.as_deref(), Some("banana"));
        assert!(invalid_string.unsorted);
    }

    #[test]
    fn derives_effective_url_with_the_documented_precedence() {
        let base = FrontMatter {
            title: None,
            date: None,
            draft: false,
            slug: None,
            path: None,
            unsorted: true,
        };
        assert_eq!(
            effective_url("post/2026/My First Post.md", &base),
            "/post/2026/my-first-post/"
        );

        let with_slug = FrontMatter {
            slug: Some("Pinned Slug".to_owned()),
            ..base.clone()
        };
        assert_eq!(
            effective_url("post/Travel Notes/ignored.md", &with_slug),
            "/post/travel-notes/pinned-slug/"
        );

        let with_path = FrontMatter {
            path: Some("/A Complete/Custom Path/".to_owned()),
            ..with_slug
        };
        assert_eq!(
            effective_url("post/ignored.md", &with_path),
            "/a-complete/custom-path/"
        );
    }

    #[test]
    fn detects_collisions_with_drafts_and_honors_exclusion() {
        let tree = TempTree::new();
        let site = tree.path();
        fs::create_dir_all(site.join("content/post/2026")).unwrap();
        fs::write(
            site.join("content/post/2026/existing.md"),
            "+++\ntitle = \"Existing\"\ndate = 2026-08-12\ndraft = true\nslug = \"same\"\n+++\n",
        )
        .unwrap();
        let candidate = b"+++\ntitle = \"Candidate\"\ndate = 2026-08-13\nslug = \"same\"\n+++\n";

        let collision = find_url_collision(site, "post/2026/candidate.md", candidate, None)
            .unwrap()
            .unwrap();
        assert_eq!(collision.path, "post/2026/existing.md");
        assert_eq!(collision.url, "/post/2026/same/");
        assert!(
            find_url_collision(
                site,
                "post/2026/candidate.md",
                candidate,
                Some("post/2026/existing.md")
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn computes_revision_from_exact_bytes() {
        assert_eq!(
            revision(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_ne!(revision(b"abc"), revision(b"abc\n"));
    }

    #[test]
    fn rejects_invalid_content_relative_post_paths() {
        for path in [
            "",
            "/post/2026/hello.md",
            "post/../secret.md",
            "post/./hello.md",
            "post//hello.md",
            "post\\2026\\hello.md",
            "other/hello.md",
            "post",
            "post/hello.txt",
            "post/_index.md",
            "post/2026/_index.md",
            "post/hello\0.md",
        ] {
            assert!(validate_post_path(path).is_err(), "accepted {path:?}");
        }
        assert_eq!(
            validate_post_path("post/2026/hello.md").unwrap(),
            PathBuf::from("post/2026/hello.md")
        );
    }

    #[test]
    fn scans_recursively_and_excludes_all_section_indexes() {
        let tree = TempTree::new();
        fs::create_dir_all(tree.path().join("content/post/2026/deep")).unwrap();
        fs::write(tree.path().join("content/post/_index.md"), "section").unwrap();
        fs::write(
            tree.path().join("content/post/2026/deep/_index.md"),
            "nested section",
        )
        .unwrap();
        fs::write(
            tree.path().join("content/post/2026/valid.md"),
            "+++\ntitle = \"Valid\"\ndate = 2026-08-12\n+++\n",
        )
        .unwrap();
        fs::write(
            tree.path().join("content/post/2026/deep/malformed.md"),
            "---\ntitle: Malformed\n---\n",
        )
        .unwrap();
        fs::write(tree.path().join("content/post/2026/ignored.txt"), "text").unwrap();

        let posts = scan_posts(tree.path()).unwrap();
        assert_eq!(posts.len(), 2);
        assert_eq!(posts[0].path, "post/2026/deep/malformed.md");
        assert!(posts[0].unsorted);
        assert_eq!(posts[1].path, "post/2026/valid.md");
        assert!(!posts[1].unsorted);
    }
}
