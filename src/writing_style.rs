use std::{
    fmt,
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;
use tokio::sync::Mutex;

use crate::site;

pub const WRITING_STYLE_PATH: &str = "WRITING_STYLE.md";

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct WritingStyleStore {
    zola_root: Arc<PathBuf>,
    coordinator: Arc<Mutex<()>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WritingStyleDocument {
    pub revision: Option<String>,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WritingStyleMutation {
    pub revision: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WritingStyleError {
    pub error: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_revision: Option<String>,
}

impl fmt::Display for WritingStyleError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl WritingStyleStore {
    pub fn new(zola_root: PathBuf, coordinator: Arc<Mutex<()>>) -> Self {
        Self {
            zola_root: Arc::new(zola_root),
            coordinator,
        }
    }

    pub fn load(&self) -> Result<WritingStyleDocument, WritingStyleError> {
        load(&self.zola_root)
    }

    pub async fn replace(
        &self,
        expected_revision: Option<&str>,
        content: &str,
    ) -> Result<WritingStyleMutation, WritingStyleError> {
        let _guard = self.coordinator.lock().await;
        let current = load(&self.zola_root)?;
        if current.revision.as_deref() != expected_revision {
            return Err(WritingStyleError {
                error: "revision_conflict",
                message: "The writing style changed after it was read. Call get_writing_style, review the current content, and retry with its revision.".to_owned(),
                current_revision: current.revision,
            });
        }

        let path = self.zola_root.join(WRITING_STYLE_PATH);
        atomic_replace(&path, content.as_bytes())?;
        Ok(WritingStyleMutation {
            revision: site::revision(content.as_bytes()),
        })
    }
}

fn load(zola_root: &Path) -> Result<WritingStyleDocument, WritingStyleError> {
    let path = zola_root.join(WRITING_STYLE_PATH);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(WritingStyleDocument {
                revision: None,
                content: String::new(),
            });
        }
        Err(_) => {
            return Err(style_error(
                "writing_style_read_failed",
                "The writing style could not be read safely.",
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(style_error(
            "invalid_writing_style_file",
            format!("{WRITING_STYLE_PATH} must be a regular file and cannot be a symbolic link."),
        ));
    }

    let bytes = fs::read(&path).map_err(|_| {
        style_error(
            "writing_style_read_failed",
            "The writing style could not be read safely.",
        )
    })?;
    let revision = site::revision(&bytes);
    let content = String::from_utf8(bytes).map_err(|_| {
        style_error(
            "invalid_writing_style_encoding",
            format!("{WRITING_STYLE_PATH} must contain valid UTF-8 Markdown."),
        )
    })?;
    Ok(WritingStyleDocument {
        revision: Some(revision),
        content,
    })
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), WritingStyleError> {
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
        return Err(style_error(
            "writing_style_write_failed",
            "The writing style could not be replaced atomically; the original file was left unchanged.",
        ));
    }
    Ok(())
}

fn unique_temp_path(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(
        ".{WRITING_STYLE_PATH}.blogger-mcp-{}-{nanos}-{sequence}.tmp",
        std::process::id()
    ))
}

fn style_error(error: &'static str, message: impl Into<String>) -> WritingStyleError {
    WritingStyleError {
        error,
        message: message.into(),
        current_revision: None,
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    };

    use tokio::sync::Mutex;

    use super::{WRITING_STYLE_PATH, WritingStyleStore};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TempSite(PathBuf);

    impl TempSite {
        fn new() -> Self {
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "blogger-writing-style-test-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempSite {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn store(site: &TempSite) -> WritingStyleStore {
        WritingStyleStore::new(site.path().to_owned(), Arc::new(Mutex::new(())))
    }

    #[tokio::test]
    async fn reads_missing_creates_and_replaces_the_complete_file() {
        let site = TempSite::new();
        let store = store(&site);

        let missing = store.load().unwrap();
        assert_eq!(missing.revision, None);
        assert!(missing.content.is_empty());

        let created = store
            .replace(None, "# Voice\n\nWarm and direct.\n")
            .await
            .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.revision.as_deref(), Some(created.revision.as_str()));
        assert_eq!(loaded.content, "# Voice\n\nWarm and direct.\n");

        let replaced = store
            .replace(loaded.revision.as_deref(), "# Voice\n\nConcise.\n")
            .await
            .unwrap();
        assert_ne!(replaced.revision, created.revision);
        assert_eq!(
            fs::read_to_string(site.path().join(WRITING_STYLE_PATH)).unwrap(),
            "# Voice\n\nConcise.\n"
        );
    }

    #[tokio::test]
    async fn rejects_stale_or_incorrect_creation_revisions() {
        let site = TempSite::new();
        let store = store(&site);
        let created = store.replace(None, "Original").await.unwrap();

        let stale = store.replace(None, "Lost update").await.unwrap_err();
        assert_eq!(stale.error, "revision_conflict");
        assert_eq!(
            stale.current_revision.as_deref(),
            Some(created.revision.as_str())
        );
        assert_eq!(store.load().unwrap().content, "Original");

        fs::remove_file(site.path().join(WRITING_STYLE_PATH)).unwrap();
        let missing = store
            .replace(Some(&created.revision), "Unexpected recreation")
            .await
            .unwrap_err();
        assert_eq!(missing.error, "revision_conflict");
        assert_eq!(missing.current_revision, None);
    }

    #[test]
    fn rejects_non_utf8_and_non_regular_files() {
        let site = TempSite::new();
        fs::write(site.path().join(WRITING_STYLE_PATH), [0xff, 0xfe]).unwrap();
        assert_eq!(
            store(&site).load().unwrap_err().error,
            "invalid_writing_style_encoding"
        );

        fs::remove_file(site.path().join(WRITING_STYLE_PATH)).unwrap();
        fs::create_dir(site.path().join(WRITING_STYLE_PATH)).unwrap();
        assert_eq!(
            store(&site).load().unwrap_err().error,
            "invalid_writing_style_file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let site = TempSite::new();
        fs::write(site.path().join("outside.md"), "secret").unwrap();
        symlink("outside.md", site.path().join(WRITING_STYLE_PATH)).unwrap();

        assert_eq!(
            store(&site).load().unwrap_err().error,
            "invalid_writing_style_file"
        );
    }
}
