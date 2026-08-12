use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static PROBE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

    use super::{discover, validate, validate_slugify_config};

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
}
