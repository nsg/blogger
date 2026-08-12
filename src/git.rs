use std::{
    collections::{BTreeSet, HashMap},
    fs,
    path::{Component, Path, PathBuf},
    process::ExitStatus,
    sync::Arc,
    time::Duration,
};

use axum::{
    Json,
    extract::{State, rejection::JsonRejection},
    http::StatusCode,
};
use serde_json::{Value, json};
use tokio::process::Command;

use crate::{config::Config, site, state::AppState};

type ApiError = (StatusCode, Json<Value>);
type ApiResult = Result<Json<Value>, ApiError>;

const MANUAL_RESOLUTION: &str = "Local and remote changes touch the same paths. Resolve the repository state manually with Git on a computer. If useful, first preserve local article content under a new post filename, then perform the Git integration manually.";
const REBASE_FAILED: &str = "Git could not safely rebase the local commit. The rebase was aborted and the commit was preserved. Resolve the repository state manually with Git on a computer.";

#[derive(Clone)]
pub struct Repository {
    root: PathBuf,
    branch: String,
    upstream: String,
    post_prefix: String,
    image_prefix: String,
    merge_head: PathBuf,
    rebase_merge: PathBuf,
    rebase_apply: PathBuf,
    cherry_pick_head: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl ChangeKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Change {
    path: String,
    kind: ChangeKind,
    old_path: Option<String>,
}

#[derive(Debug, Default, Eq, PartialEq)]
struct StatusSnapshot {
    changes: Vec<Change>,
    ahead: u64,
    behind: u64,
}

struct GitOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: String,
}

#[derive(Clone, Copy, Default)]
struct CommandOptions {
    identity: bool,
    network: bool,
}

pub async fn validate_repository(zola_root: &Path, config: &Config) -> Result<Repository, String> {
    let inside = required_output(
        run_git(
            zola_root,
            config,
            &["rev-parse", "--is-inside-work-tree"],
            None,
        )
        .await?,
        "determine whether the Zola site is in a Git working tree",
    )?;
    if output_text(&inside)?.trim() != "true" {
        return Err(format!(
            "Zola site is not inside a non-bare Git working tree: {}",
            zola_root.display()
        ));
    }

    let top = required_output(
        run_git(zola_root, config, &["rev-parse", "--show-toplevel"], None).await?,
        "discover the Git repository root",
    )?;
    let root = PathBuf::from(output_text(&top)?.trim());
    let bare = required_output(
        run_git(&root, config, &["rev-parse", "--is-bare-repository"], None).await?,
        "check whether the Git repository is bare",
    )?;
    if output_text(&bare)?.trim() != "false" {
        return Err(format!(
            "Git repository must be a non-bare working tree: {}",
            root.display()
        ));
    }

    let branch_output = run_git(
        &root,
        config,
        &["symbolic-ref", "--quiet", "--short", "HEAD"],
        None,
    )
    .await?;
    if !branch_output.status.success() {
        return Err(
            "Git HEAD is detached; check out the branch Blogger should publish before starting"
                .to_owned(),
        );
    }
    let branch = output_text(&branch_output)?.trim().to_owned();
    if branch.is_empty() {
        return Err("Git returned an empty checked-out branch name".to_owned());
    }
    let expected_upstream = format!("origin/{branch}");
    let upstream_output = run_git(
        &root,
        config,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
        None,
    )
    .await?;
    if !upstream_output.status.success()
        || output_text(&upstream_output)?.trim() != expected_upstream
    {
        return Err(format!(
            "checked-out branch {branch} must track {expected_upstream}; configure that upstream before starting Blogger"
        ));
    }
    required_output(
        run_git(
            &root,
            config,
            &["rev-parse", "--verify", &expected_upstream],
            None,
        )
        .await?,
        &format!("verify upstream {expected_upstream}"),
    )?;

    let remote = required_output(
        run_git(&root, config, &["remote", "get-url", "origin"], None).await?,
        "read the origin remote URL",
    )?;
    let remote = output_text(&remote)?.trim();
    if !remote.starts_with("https://") {
        return Err(format!(
            "origin remote must use HTTPS for Blogger publication; current URL is {remote}"
        ));
    }

    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("failed to resolve Git repository root: {error}"))?;
    let canonical_zola = zola_root
        .canonicalize()
        .map_err(|error| format!("failed to resolve Zola site root: {error}"))?;
    let zola_relative = canonical_zola.strip_prefix(&canonical_root).map_err(|_| {
        format!(
            "Zola site root {} is outside Git repository {}",
            zola_root.display(),
            root.display()
        )
    })?;
    let post_prefix = repository_path(&zola_relative.join("content/post"))? + "/";
    let image_prefix = repository_path(&zola_relative.join("static/images"))? + "/";

    Ok(Repository {
        root: canonical_root,
        branch,
        upstream: expected_upstream,
        post_prefix,
        image_prefix,
        merge_head: git_path(&root, config, "MERGE_HEAD").await?,
        rebase_merge: git_path(&root, config, "rebase-merge").await?,
        rebase_apply: git_path(&root, config, "rebase-apply").await?,
        cherry_pick_head: git_path(&root, config, "CHERRY_PICK_HEAD").await?,
    })
}

pub async fn status(State(state): State<Arc<AppState>>) -> ApiResult {
    let snapshot = repository_status(&state).await.map_err(internal_error)?;
    Ok(Json(json!({
        "changes": changes_json(&snapshot.changes),
        "unpushed": snapshot.ahead > 0,
        "repo_blocked": repo_blocked(&state.repository),
    })))
}

pub async fn prepare(State(state): State<Arc<AppState>>) -> ApiResult {
    refuse_blocked(&state.repository)?;
    let before = repository_status(&state).await.map_err(internal_error)?;
    if before.ahead > 0 {
        return Err(unpushed_conflict());
    }

    fetch(&state).await.map_err(internal_error)?;
    refuse_blocked(&state.repository)?;
    let snapshot = repository_status(&state).await.map_err(internal_error)?;
    if snapshot.ahead > 0 {
        return Err(unpushed_conflict());
    }
    let remote = remote_changes(&state, "HEAD")
        .await
        .map_err(internal_error)?;
    let local_paths = touched_paths(&snapshot.changes);
    let remote_paths = touched_paths(&remote);
    let overlapping_paths = intersection(&local_paths, &remote_paths);
    if !overlapping_paths.is_empty() {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": MANUAL_RESOLUTION,
                "overlapping_paths": overlapping_paths,
                "local_paths": local_paths,
                "remote_paths": remote_paths,
            })),
        ));
    }

    let subject = generated_subject(&state, &snapshot.changes).await;
    Ok(Json(json!({
        "files": changes_json(&snapshot.changes),
        "subject": subject,
        "behind": snapshot.behind > 0,
    })))
}

pub async fn commit_push(
    State(state): State<Arc<AppState>>,
    payload: Result<Json<Value>, JsonRejection>,
) -> ApiResult {
    let request = json_body(payload)?;
    let subject = body_string(&request, "subject")?;
    if subject.trim().is_empty() {
        return Err(bad_request("commit subject must not be empty"));
    }
    let requested_files = body_strings(&request, "files")?;

    let guard = state.coordinator.lock().await;
    refuse_blocked(&state.repository)?;
    let snapshot = repository_status(&state).await.map_err(internal_error)?;
    if snapshot.ahead > 0 {
        return Err(unpushed_conflict());
    }
    let fresh_files = public_paths(&snapshot.changes);
    let mut requested_files = requested_files;
    requested_files.sort();
    if requested_files != fresh_files {
        let subject = generated_subject(&state, &snapshot.changes).await;
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "checkout changes changed after confirmation",
                "files": changes_json(&snapshot.changes),
                "subject": subject,
            })),
        ));
    }
    if snapshot.changes.is_empty() {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": "there are no checkout changes to commit" })),
        ));
    }

    require_command(
        &state,
        &["add", "-A"],
        CommandOptions::default(),
        "stage checkout changes",
    )
    .await
    .map_err(internal_error)?;
    require_command(
        &state,
        &["commit", "-m", subject],
        CommandOptions {
            identity: true,
            network: false,
        },
        "create Git commit",
    )
    .await
    .map_err(internal_error)?;

    if snapshot.behind > 0
        && let Err(error) = rebase(&state).await
    {
        abort_rebase(&state).await;
        let commit = head_commit(&state).await.map_err(internal_error)?;
        return Ok(push_failed(commit, format!("{REBASE_FAILED} {error}")));
    }

    let commit = head_commit(&state).await.map_err(internal_error)?;
    drop(guard);
    match push(&state).await {
        Ok(()) => Ok(Json(json!({ "status": "pushed", "commit": commit }))),
        Err(error) => Ok(push_failed(commit, error)),
    }
}

pub async fn retry_push(State(state): State<Arc<AppState>>) -> ApiResult {
    refuse_blocked(&state.repository)?;
    let before = repository_status(&state).await.map_err(internal_error)?;
    if before.ahead == 0 {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": "there are no unpushed commits to retry" })),
        ));
    }
    let original_commit = head_commit(&state).await.map_err(internal_error)?;
    if let Err(error) = fetch(&state).await {
        return Ok(push_failed(original_commit, error));
    }

    let guard = state.coordinator.lock().await;
    refuse_blocked(&state.repository)?;

    let snapshot = repository_status(&state).await.map_err(internal_error)?;
    if snapshot.ahead == 0 {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": "there are no unpushed commits to retry" })),
        ));
    }
    let local_range = format!("{}...HEAD", state.repository.upstream);
    let local = diff_changes(&state, &local_range)
        .await
        .map_err(internal_error)?;
    let remote = remote_changes(&state, "HEAD")
        .await
        .map_err(internal_error)?;
    let overlapping = intersection(&touched_paths(&local), &touched_paths(&remote));
    if !overlapping.is_empty() {
        return Ok(push_failed(
            original_commit,
            format!(
                "{MANUAL_RESOLUTION} Overlapping paths: {}",
                overlapping.join(", ")
            ),
        ));
    }

    if snapshot.behind > 0
        && let Err(error) = rebase(&state).await
    {
        abort_rebase(&state).await;
        let commit = head_commit(&state).await.map_err(internal_error)?;
        return Ok(push_failed(commit, format!("{REBASE_FAILED} {error}")));
    }

    let commit = head_commit(&state).await.map_err(internal_error)?;
    drop(guard);
    match push(&state).await {
        Ok(()) => Ok(Json(json!({ "status": "pushed", "commit": commit }))),
        Err(error) => Ok(push_failed(commit, error)),
    }
}

pub async fn sync(State(state): State<Arc<AppState>>) -> ApiResult {
    refuse_blocked(&state.repository)?;
    fetch(&state).await.map_err(internal_error)?;
    refuse_blocked(&state.repository)?;
    let snapshot = repository_status(&state).await.map_err(internal_error)?;
    if !snapshot.changes.is_empty() {
        return Err(sync_conflict(
            "cannot sync while the checkout has uncommitted changes",
        ));
    }
    if snapshot.ahead > 0 && snapshot.behind > 0 {
        return Err(sync_conflict(
            "cannot sync because the local and remote branch histories have diverged",
        ));
    }
    if snapshot.ahead > 0 {
        return Err(sync_conflict(
            "cannot sync while the branch has unpushed local commits",
        ));
    }
    if snapshot.behind == 0 {
        return Ok(Json(json!({ "updated": false })));
    }

    let _guard = state.coordinator.lock().await;
    refuse_blocked(&state.repository)?;
    let snapshot = repository_status(&state).await.map_err(internal_error)?;
    if !snapshot.changes.is_empty() {
        return Err(sync_conflict(
            "cannot sync while the checkout has uncommitted changes",
        ));
    }
    if snapshot.ahead > 0 && snapshot.behind > 0 {
        return Err(sync_conflict(
            "cannot sync because the local and remote branch histories have diverged",
        ));
    }
    if snapshot.ahead > 0 {
        return Err(sync_conflict(
            "cannot sync while the branch has unpushed local commits",
        ));
    }
    if snapshot.behind == 0 {
        return Ok(Json(json!({ "updated": false })));
    }

    let upstream = state.repository.upstream.clone();
    require_command(
        &state,
        &["merge", "--ff-only", &upstream],
        CommandOptions::default(),
        "fast-forward the checked-out branch",
    )
    .await
    .map_err(internal_error)?;
    Ok(Json(json!({ "updated": true })))
}

async fn repository_status(state: &AppState) -> Result<StatusSnapshot, String> {
    let output = require_command(
        state,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=no",
        ],
        CommandOptions::default(),
        "read Git branch status",
    )
    .await?;
    let mut snapshot = parse_porcelain_v2(&output.stdout)?;
    snapshot.changes = working_tree_changes(&state.repository, &state.config).await?;
    Ok(snapshot)
}

async fn working_tree_changes(
    repository: &Repository,
    config: &Config,
) -> Result<Vec<Change>, String> {
    let index_path = temporary_index_path();
    let result: Result<Vec<Change>, String> = async {
        required_output(
            run_git_with_index(
                &repository.root,
                config,
                &["read-tree", "HEAD"],
                None,
                &index_path,
            )
            .await?,
            "seed temporary Git index",
        )?;
        required_output(
            run_git_with_index(&repository.root, config, &["add", "-A"], None, &index_path).await?,
            "stage checkout changes in temporary Git index",
        )?;
        let output = required_output(
            run_git_with_index(
                &repository.root,
                config,
                &[
                    "diff",
                    "--cached",
                    "--name-status",
                    "--find-renames",
                    "-z",
                    "HEAD",
                ],
                None,
                &index_path,
            )
            .await?,
            "read changed Git paths from temporary index",
        )?;
        parse_name_status(&output.stdout)
    }
    .await;

    let cleanup = remove_temporary_index(&index_path);
    match (result, cleanup) {
        (Ok(changes), Ok(())) => Ok(changes),
        (Ok(_), Err(error)) | (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(format!("{error}; {cleanup_error}")),
    }
}

fn temporary_index_path() -> PathBuf {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    loop {
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "blogger-git-index-{}-{sequence}",
            std::process::id()
        ));
        if !path.exists() && !temporary_index_lock_path(&path).exists() {
            return path;
        }
    }
}

fn temporary_index_lock_path(path: &Path) -> PathBuf {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    PathBuf::from(lock_path)
}

fn remove_temporary_index(path: &Path) -> Result<(), String> {
    for candidate in [path.to_owned(), temporary_index_lock_path(path)] {
        match fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "failed to remove temporary Git index {}: {error}",
                    candidate.display()
                ));
            }
        }
    }
    Ok(())
}

async fn fetch(state: &AppState) -> Result<(), String> {
    require_command(
        state,
        &["fetch", "origin"],
        CommandOptions {
            identity: false,
            network: true,
        },
        "fetch origin",
    )
    .await
    .map(|_| ())
}

async fn push(state: &AppState) -> Result<(), String> {
    let branch = state.repository.branch.clone();
    require_command(
        state,
        &["push", "origin", &branch],
        CommandOptions {
            identity: false,
            network: true,
        },
        "push the Git branch",
    )
    .await
    .map(|_| ())
}

async fn rebase(state: &AppState) -> Result<(), String> {
    let upstream = state.repository.upstream.clone();
    require_command(
        state,
        &["rebase", &upstream],
        CommandOptions {
            identity: true,
            network: false,
        },
        "rebase the local commit onto its upstream",
    )
    .await
    .map(|_| ())
}

async fn abort_rebase(state: &AppState) {
    let _ = run_command(
        state,
        &["rebase", "--abort"],
        CommandOptions {
            identity: true,
            network: false,
        },
    )
    .await;
}

async fn head_commit(state: &AppState) -> Result<String, String> {
    let output = require_command(
        state,
        &["rev-parse", "HEAD"],
        CommandOptions::default(),
        "read the current commit ID",
    )
    .await?;
    Ok(output_text(&output)?.trim().to_owned())
}

async fn remote_changes(state: &AppState, local: &str) -> Result<Vec<Change>, String> {
    let upstream = state.repository.upstream.clone();
    let base = require_command(
        state,
        &["merge-base", local, &upstream],
        CommandOptions::default(),
        "find the local/remote merge base",
    )
    .await?;
    let range = format!("{}..{upstream}", output_text(&base)?.trim());
    diff_changes(state, &range).await
}

async fn diff_changes(state: &AppState, range: &str) -> Result<Vec<Change>, String> {
    let output = require_command(
        state,
        &["diff", "--name-status", "--find-renames", "-z", range],
        CommandOptions::default(),
        "read changed Git paths",
    )
    .await?;
    parse_name_status(&output.stdout)
}

async fn generated_subject(state: &AppState, changes: &[Change]) -> String {
    let post_changes = changes
        .iter()
        .filter(|change| is_post_change(change, &state.repository.post_prefix))
        .collect::<Vec<_>>();
    let mut titles = HashMap::new();
    if let [change] = post_changes.as_slice()
        && let Some(title) = change_title(state, change).await
    {
        titles.insert(change.path.clone(), title);
    }
    generate_subject(
        changes,
        &state.repository.post_prefix,
        &state.repository.image_prefix,
        |change| titles.get(&change.path).cloned(),
    )
}

async fn change_title(state: &AppState, change: &Change) -> Option<String> {
    let bytes = if change.kind == ChangeKind::Deleted {
        let spec = format!("HEAD:{}", change.path);
        let output = run_command(state, &["show", &spec], CommandOptions::default())
            .await
            .ok()?;
        output.status.success().then_some(output.stdout)?
    } else {
        fs::read(state.repository.root.join(&change.path)).ok()?
    };
    site::parse_front_matter(&bytes)
        .title
        .filter(|title| !title.trim().is_empty())
}

fn generate_subject(
    changes: &[Change],
    post_prefix: &str,
    image_prefix: &str,
    mut title_lookup: impl FnMut(&Change) -> Option<String>,
) -> String {
    let posts = changes
        .iter()
        .filter(|change| is_post_change(change, post_prefix))
        .collect::<Vec<_>>();
    if let [post] = posts.as_slice() {
        let verb = match post.kind {
            ChangeKind::Added => "Add",
            ChangeKind::Modified => "Update",
            ChangeKind::Deleted => "Remove",
            ChangeKind::Renamed => "Rename",
        };
        let title = title_lookup(post).unwrap_or_else(|| filename_title(&post.path));
        return format!("{verb} blog post about {title}");
    }
    if posts.len() > 1 {
        let kinds = posts.iter().map(|post| post.kind).collect::<BTreeSet<_>>();
        if kinds.len() == 1 && kinds.contains(&ChangeKind::Added) {
            return format!("Add {} blog posts", posts.len());
        }
        if kinds.len() == 1 && kinds.contains(&ChangeKind::Modified) {
            return format!("Update {} blog posts", posts.len());
        }
        return "Update blog posts".to_owned();
    }

    if !changes.is_empty()
        && changes
            .iter()
            .all(|change| is_image_change(change, image_prefix))
    {
        "Update blog images".to_owned()
    } else {
        "Update blog site".to_owned()
    }
}

fn parse_porcelain_v2(bytes: &[u8]) -> Result<StatusSnapshot, String> {
    let records = nul_records(bytes)?;
    let mut snapshot = StatusSnapshot::default();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        if let Some(counts) = record.strip_prefix("# branch.ab ") {
            let mut fields = counts.split_whitespace();
            snapshot.ahead = parse_count(fields.next(), '+')?;
            snapshot.behind = parse_count(fields.next(), '-')?;
        } else if record.starts_with("1 ") {
            let fields = record.splitn(9, ' ').collect::<Vec<_>>();
            if fields.len() != 9 {
                return Err(format!("invalid Git porcelain record: {record}"));
            }
            snapshot.changes.push(Change {
                path: fields[8].to_owned(),
                kind: xy_kind(fields[1]),
                old_path: None,
            });
        } else if record.starts_with("2 ") {
            let fields = record.splitn(10, ' ').collect::<Vec<_>>();
            if fields.len() != 10 || index + 1 >= records.len() {
                return Err(format!("invalid Git rename record: {record}"));
            }
            index += 1;
            let renamed = fields[8].starts_with('R');
            snapshot.changes.push(Change {
                path: fields[9].to_owned(),
                kind: if renamed {
                    ChangeKind::Renamed
                } else {
                    ChangeKind::Added
                },
                old_path: renamed.then(|| records[index].to_owned()),
            });
        } else if let Some(path) = record.strip_prefix("? ") {
            snapshot.changes.push(Change {
                path: path.to_owned(),
                kind: ChangeKind::Added,
                old_path: None,
            });
        } else if record.starts_with("u ") {
            let fields = record.splitn(11, ' ').collect::<Vec<_>>();
            let path = fields
                .last()
                .filter(|_| fields.len() == 11)
                .ok_or_else(|| format!("invalid Git unmerged record: {record}"))?;
            snapshot.changes.push(Change {
                path: (*path).to_owned(),
                kind: ChangeKind::Modified,
                old_path: None,
            });
        }
        index += 1;
    }
    snapshot.changes.sort();
    Ok(snapshot)
}

fn parse_name_status(bytes: &[u8]) -> Result<Vec<Change>, String> {
    let records = nul_records(bytes)?;
    let mut changes = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let status = records[index];
        index += 1;
        let code = status
            .as_bytes()
            .first()
            .copied()
            .ok_or_else(|| "Git returned an empty name-status record".to_owned())?;
        if matches!(code, b'R' | b'C') {
            if index + 1 >= records.len() {
                return Err(format!("invalid Git name-status rename record: {status}"));
            }
            let old_path = records[index].to_owned();
            let path = records[index + 1].to_owned();
            index += 2;
            changes.push(Change {
                path,
                kind: if code == b'R' {
                    ChangeKind::Renamed
                } else {
                    ChangeKind::Added
                },
                old_path: (code == b'R').then_some(old_path),
            });
            continue;
        }
        if index >= records.len() {
            return Err(format!("Git name-status record has no path: {status}"));
        }
        let path = records[index].to_owned();
        index += 1;
        let kind = match code {
            b'A' => ChangeKind::Added,
            b'D' => ChangeKind::Deleted,
            _ => ChangeKind::Modified,
        };
        changes.push(Change {
            path,
            kind,
            old_path: None,
        });
    }
    changes.sort();
    Ok(changes)
}

fn nul_records(bytes: &[u8]) -> Result<Vec<&str>, String> {
    bytes
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty())
        .map(|record| {
            std::str::from_utf8(record)
                .map_err(|_| "Git returned a path that is not valid UTF-8".to_owned())
        })
        .collect()
}

fn xy_kind(xy: &str) -> ChangeKind {
    if xy.contains('D') {
        ChangeKind::Deleted
    } else if xy.contains('A') {
        ChangeKind::Added
    } else {
        ChangeKind::Modified
    }
}

fn parse_count(value: Option<&str>, prefix: char) -> Result<u64, String> {
    value
        .and_then(|value| value.strip_prefix(prefix))
        .ok_or_else(|| "invalid Git branch ahead/behind status".to_owned())?
        .parse()
        .map_err(|_| "invalid Git branch ahead/behind count".to_owned())
}

fn touched_paths(changes: &[Change]) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for change in changes {
        paths.insert(change.path.clone());
        if let Some(old_path) = &change.old_path {
            paths.insert(old_path.clone());
        }
    }
    paths.into_iter().collect()
}

fn intersection(left: &[String], right: &[String]) -> Vec<String> {
    let left = left.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let right = right.iter().map(String::as_str).collect::<BTreeSet<_>>();
    left.intersection(&right)
        .map(|path| (*path).to_owned())
        .collect()
}

fn is_post_change(change: &Change, prefix: &str) -> bool {
    is_post_path(&change.path, prefix)
        || change
            .old_path
            .as_deref()
            .is_some_and(|path| is_post_path(path, prefix))
}

fn is_post_path(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix).is_some_and(|relative| {
        !relative.is_empty()
            && relative.ends_with(".md")
            && relative.rsplit('/').next() != Some("_index.md")
    })
}

fn is_image_change(change: &Change, prefix: &str) -> bool {
    change.path.starts_with(prefix)
        && change
            .old_path
            .as_deref()
            .is_none_or(|path| path.starts_with(prefix))
}

fn filename_title(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or(path)
        .to_owned()
}

fn public_paths(changes: &[Change]) -> Vec<String> {
    changes.iter().map(|change| change.path.clone()).collect()
}

fn changes_json(changes: &[Change]) -> Vec<Value> {
    changes
        .iter()
        .map(|change| json!({ "path": change.path, "kind": change.kind.as_str() }))
        .collect()
}

fn repo_blocked(repository: &Repository) -> Option<&'static str> {
    if repository.merge_head.exists() {
        Some("merge")
    } else if repository.rebase_merge.exists() || repository.rebase_apply.exists() {
        Some("rebase")
    } else if repository.cherry_pick_head.exists() {
        Some("cherry-pick")
    } else {
        None
    }
}

fn refuse_blocked(repository: &Repository) -> Result<(), ApiError> {
    if let Some(blocked) = repo_blocked(repository) {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": format!("publishing is disabled until the unfinished {blocked} is resolved manually with Git"),
                "repo_blocked": blocked,
            })),
        ));
    }
    Ok(())
}

fn unpushed_conflict() -> ApiError {
    (
        StatusCode::CONFLICT,
        Json(json!({
            "error": "an unpushed local commit already exists; retry the push before creating another commit",
            "unpushed": true,
        })),
    )
}

fn sync_conflict(message: &str) -> ApiError {
    (StatusCode::CONFLICT, Json(json!({ "error": message })))
}

fn push_failed(commit: String, error: String) -> Json<Value> {
    Json(json!({
        "status": "push_failed",
        "commit": commit,
        "error": error,
    }))
}

fn json_body(payload: Result<Json<Value>, JsonRejection>) -> Result<Value, ApiError> {
    let Json(value) =
        payload.map_err(|error| bad_request(format!("invalid JSON body: {error}")))?;
    if !value.is_object() {
        return Err(bad_request("JSON body must be an object"));
    }
    Ok(value)
}

fn body_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, ApiError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| bad_request(format!("{field} must be a string")))
}

fn body_strings(value: &Value, field: &str) -> Result<Vec<String>, ApiError> {
    value
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| bad_request(format!("{field} must be an array of paths")))?
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| bad_request(format!("{field} must contain only strings")))
        })
        .collect()
}

fn bad_request(message: impl Into<String>) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message.into() })),
    )
}

fn internal_error(message: impl Into<String>) -> ApiError {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": message.into() })),
    )
}

async fn git_path(root: &Path, config: &Config, name: &str) -> Result<PathBuf, String> {
    let output = required_output(
        run_git(root, config, &["rev-parse", "--git-path", name], None).await?,
        &format!("resolve Git state path {name}"),
    )?;
    let path = PathBuf::from(output_text(&output)?.trim());
    Ok(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

fn repository_path(path: &Path) -> Result<String, String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(
                part.to_str()
                    .ok_or_else(|| "repository path is not valid UTF-8".to_owned())?,
            ),
            Component::CurDir => {}
            _ => return Err("repository path contains invalid components".to_owned()),
        }
    }
    Ok(parts.join("/"))
}

async fn require_command(
    state: &AppState,
    args: &[&str],
    options: CommandOptions,
    action: &str,
) -> Result<GitOutput, String> {
    required_output(run_command(state, args, options).await?, action)
}

async fn run_command(
    state: &AppState,
    args: &[&str],
    options: CommandOptions,
) -> Result<GitOutput, String> {
    run_git(&state.repository.root, &state.config, args, Some(options)).await
}

async fn run_git(
    directory: &Path,
    config: &Config,
    args: &[&str],
    options: Option<CommandOptions>,
) -> Result<GitOutput, String> {
    run_git_inner(directory, config, args, options, None).await
}

async fn run_git_with_index(
    directory: &Path,
    config: &Config,
    args: &[&str],
    options: Option<CommandOptions>,
    index_file: &Path,
) -> Result<GitOutput, String> {
    run_git_inner(directory, config, args, options, Some(index_file)).await
}

async fn run_git_inner(
    directory: &Path,
    config: &Config,
    args: &[&str],
    options: Option<CommandOptions>,
    index_file: Option<&Path>,
) -> Result<GitOutput, String> {
    let options = options.unwrap_or_default();
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(directory)
        .env("GIT_TERMINAL_PROMPT", "0")
        .kill_on_drop(true);
    if let Some(index_file) = index_file {
        command.env("GIT_INDEX_FILE", index_file);
    }
    if options.identity {
        command
            .env("GIT_AUTHOR_NAME", &config.git_name)
            .env("GIT_AUTHOR_EMAIL", &config.git_email)
            .env("GIT_COMMITTER_NAME", &config.git_name)
            .env("GIT_COMMITTER_EMAIL", &config.git_email);
    }
    if options.network {
        let executable = std::env::current_exe()
            .map_err(|error| format!("failed to locate Blogger askpass helper: {error}"))?;
        command
            .env("GIT_ASKPASS", executable)
            .env("BLOGGER_ASKPASS_MODE", "1");
    }
    let timeout_duration = if options.network {
        Duration::from_secs(120)
    } else {
        Duration::from_secs(30)
    };
    let output = tokio::time::timeout(timeout_duration, command.output())
        .await
        .map_err(|_| {
            format!(
                "Git {} command timed out after {} seconds",
                if options.network { "network" } else { "local" },
                timeout_duration.as_secs()
            )
        })?
        .map_err(|error| format!("failed to run Git: {error}"))?;
    Ok(GitOutput {
        status: output.status,
        stdout: sanitize_bytes(&output.stdout, config.github_token.as_bytes()),
        stderr: String::from_utf8_lossy(&sanitize_bytes(
            &output.stderr,
            config.github_token.as_bytes(),
        ))
        .into_owned(),
    })
}

fn required_output(output: GitOutput, action: &str) -> Result<GitOutput, String> {
    if output.status.success() {
        return Ok(output);
    }
    let detail = if output.stderr.trim().is_empty() {
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    } else {
        output.stderr.trim().to_owned()
    };
    if detail.is_empty() {
        Err(format!("failed to {action}"))
    } else {
        Err(format!("failed to {action}: {detail}"))
    }
}

fn output_text(output: &GitOutput) -> Result<&str, String> {
    std::str::from_utf8(&output.stdout)
        .map_err(|_| "Git returned output that is not valid UTF-8".to_owned())
}

fn sanitize_bytes(input: &[u8], token: &[u8]) -> Vec<u8> {
    if token.is_empty() {
        return input.to_vec();
    }
    let mut sanitized = Vec::with_capacity(input.len());
    let mut remaining = input;
    while let Some(index) = remaining
        .windows(token.len())
        .position(|window| window == token)
    {
        sanitized.extend_from_slice(&remaining[..index]);
        sanitized.extend_from_slice(b"***");
        remaining = &remaining[index + token.len()..];
    }
    sanitized.extend_from_slice(remaining);
    sanitized
}

#[cfg(test)]
mod tests {
    use super::*;

    const POSTS: &str = "site/content/post/";
    const IMAGES: &str = "site/static/images/";

    fn change(path: &str, kind: ChangeKind) -> Change {
        Change {
            path: path.to_owned(),
            kind,
            old_path: None,
        }
    }

    fn renamed(old: &str, new: &str) -> Change {
        Change {
            path: new.to_owned(),
            kind: ChangeKind::Renamed,
            old_path: Some(old.to_owned()),
        }
    }

    fn subject(changes: &[Change], title: Option<&str>) -> String {
        generate_subject(changes, POSTS, IMAGES, |_| title.map(str::to_owned))
    }

    #[test]
    fn generates_every_commit_subject_class() {
        assert_eq!(
            subject(
                &[change("site/content/post/2026/new.md", ChangeKind::Added)],
                Some("New")
            ),
            "Add blog post about New"
        );
        assert_eq!(
            subject(
                &[change(
                    "site/content/post/2026/changed.md",
                    ChangeKind::Modified
                )],
                Some("Changed")
            ),
            "Update blog post about Changed"
        );
        assert_eq!(
            subject(
                &[change(
                    "site/content/post/2026/deleted.md",
                    ChangeKind::Deleted
                )],
                Some("Deleted")
            ),
            "Remove blog post about Deleted"
        );
        assert_eq!(
            subject(
                &[renamed(
                    "site/content/post/2026/old.md",
                    "site/content/post/2026/new.md"
                )],
                Some("Renamed")
            ),
            "Rename blog post about Renamed"
        );

        let additions = [
            change("site/content/post/2026/one.md", ChangeKind::Added),
            change("site/content/post/2026/two.md", ChangeKind::Added),
        ];
        assert_eq!(subject(&additions, None), "Add 2 blog posts");
        let updates = [
            change("site/content/post/2026/one.md", ChangeKind::Modified),
            change("site/content/post/2026/two.md", ChangeKind::Modified),
        ];
        assert_eq!(subject(&updates, None), "Update 2 blog posts");
        let mixed = [additions[0].clone(), updates[1].clone()];
        assert_eq!(subject(&mixed, None), "Update blog posts");
        let deletions = [
            change("site/content/post/2026/one.md", ChangeKind::Deleted),
            change("site/content/post/2026/two.md", ChangeKind::Deleted),
        ];
        assert_eq!(subject(&deletions, None), "Update blog posts");
        let renames = [
            renamed(
                "site/content/post/2026/one-old.md",
                "site/content/post/2026/one.md",
            ),
            renamed(
                "site/content/post/2026/two-old.md",
                "site/content/post/2026/two.md",
            ),
        ];
        assert_eq!(subject(&renames, None), "Update blog posts");
        assert_eq!(
            subject(
                &[change(
                    "site/static/images/2026/photo.jpg",
                    ChangeKind::Added
                )],
                None
            ),
            "Update blog images"
        );
        assert_eq!(
            subject(
                &[
                    change("site/content/post/2026/post.md", ChangeKind::Modified),
                    change("site/static/images/2026/photo.jpg", ChangeKind::Added),
                ],
                Some("Post")
            ),
            "Update blog post about Post"
        );
        assert_eq!(
            subject(&[change("site/config.toml", ChangeKind::Modified)], None),
            "Update blog site"
        );
    }

    #[test]
    fn single_post_title_falls_back_to_filename_stem() {
        assert_eq!(
            subject(
                &[change(
                    "site/content/post/2026/file-name.md",
                    ChangeKind::Modified
                )],
                None
            ),
            "Update blog post about file-name"
        );
        assert_eq!(
            subject(
                &[change("site/content/post/_index.md", ChangeKind::Modified)],
                Some("Section")
            ),
            "Update blog site"
        );
    }

    #[test]
    fn parses_porcelain_v2_status_with_spaces_and_renames() {
        let input = concat!(
            "# branch.oid abc\0",
            "# branch.head master\0",
            "# branch.upstream origin/master\0",
            "# branch.ab +2 -3\0",
            "1 .M N... 100644 100644 100644 abc abc src/main.rs\0",
            "1 D. N... 100644 000000 000000 abc 000 old.txt\0",
            "2 R. N... 100644 100644 100644 abc def R100 new name.md\0",
            "old name.md\0",
            "? untracked file.txt\0"
        );
        let parsed = parse_porcelain_v2(input.as_bytes()).unwrap();
        assert_eq!(parsed.ahead, 2);
        assert_eq!(parsed.behind, 3);
        assert_eq!(parsed.changes.len(), 4);
        assert!(
            parsed
                .changes
                .contains(&change("src/main.rs", ChangeKind::Modified))
        );
        assert!(
            parsed
                .changes
                .contains(&change("old.txt", ChangeKind::Deleted))
        );
        assert!(
            parsed
                .changes
                .contains(&renamed("old name.md", "new name.md"))
        );
        assert!(
            parsed
                .changes
                .contains(&change("untracked file.txt", ChangeKind::Added))
        );
    }

    #[test]
    fn overlap_expands_renames_and_keeps_deletions() {
        let local = touched_paths(&[
            renamed("content/post/old.md", "content/post/new.md"),
            change("static/images/gone.png", ChangeKind::Deleted),
        ]);
        let remote = touched_paths(&[
            change("content/post/old.md", ChangeKind::Modified),
            change("static/images/gone.png", ChangeKind::Modified),
            change("unrelated", ChangeKind::Deleted),
        ]);
        assert_eq!(
            intersection(&local, &remote),
            vec![
                "content/post/old.md".to_owned(),
                "static/images/gone.png".to_owned()
            ]
        );
    }

    #[test]
    fn parses_name_status_renames_and_deletions() {
        let changes = parse_name_status(b"R100\0old.md\0new.md\0D\0gone.md\0").unwrap();
        assert_eq!(
            changes,
            vec![
                change("gone.md", ChangeKind::Deleted),
                renamed("old.md", "new.md")
            ]
        );
    }

    #[test]
    fn sanitizes_every_token_occurrence() {
        assert_eq!(
            sanitize_bytes(b"before-secret-middle-secret-after", b"secret"),
            b"before-***-middle-***-after"
        );
        assert_eq!(sanitize_bytes(b"unchanged", b""), b"unchanged");
    }

    #[tokio::test]
    async fn validates_a_nested_site_repository_without_network_access() {
        let temporary = TemporaryRepository::new();
        let site_root = temporary.root.join("site");
        fs::create_dir_all(&site_root).unwrap();
        fs::write(temporary.root.join("tracked.txt"), "initial").unwrap();
        temporary.git(&["init", "-b", "master"]);
        temporary.git(&["add", "-A"]);
        temporary.git(&["commit", "-m", "initial"]);
        temporary.git(&[
            "remote",
            "add",
            "origin",
            "https://example.invalid/blog.git",
        ]);
        temporary.git(&["update-ref", "refs/remotes/origin/master", "HEAD"]);
        temporary.git(&["branch", "--set-upstream-to=origin/master", "master"]);

        let config = Config {
            ollama_key: "ollama".to_owned(),
            ollama_model: "qwen3.5:397b".to_owned(),
            stt_api_key: "openai".to_owned(),
            password: "password".to_owned(),
            session_secret: [0; 32],
            github_token: "secret-token".to_owned(),
            git_name: "Blogger Test".to_owned(),
            git_email: "blogger@example.invalid".to_owned(),
            mcp_public_url: "https://mcp.example.invalid/mcp".to_owned(),
            mcp_issuer: "https://mcp.example.invalid".to_owned(),
            mcp_host: "mcp.example.invalid".to_owned(),
        };
        let repository = validate_repository(&site_root, &config).await.unwrap();
        assert_eq!(repository.root, temporary.root.canonicalize().unwrap());
        assert_eq!(repository.branch, "master");
        assert_eq!(repository.upstream, "origin/master");
        assert_eq!(repository.post_prefix, "site/content/post/");
        assert_eq!(repository.image_prefix, "site/static/images/");

        fs::rename(
            temporary.root.join("tracked.txt"),
            temporary.root.join("renamed.txt"),
        )
        .unwrap();
        assert_eq!(
            working_tree_changes(&repository, &config).await.unwrap(),
            vec![renamed("tracked.txt", "renamed.txt")]
        );
    }

    struct TemporaryRepository {
        root: PathBuf,
    }

    impl TemporaryRepository {
        fn new() -> Self {
            static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            loop {
                let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let root = std::env::temp_dir().join(format!(
                    "blogger-git-test-{}-{sequence}",
                    std::process::id()
                ));
                match fs::create_dir(&root) {
                    Ok(()) => return Self { root },
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("failed to create temporary repository: {error}"),
                }
            }
        }

        fn git(&self, args: &[&str]) {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&self.root)
                .env("GIT_AUTHOR_NAME", "Blogger Test")
                .env("GIT_AUTHOR_EMAIL", "blogger@example.invalid")
                .env("GIT_COMMITTER_NAME", "Blogger Test")
                .env("GIT_COMMITTER_EMAIL", "blogger@example.invalid")
                .env("GIT_TERMINAL_PROMPT", "0")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    impl Drop for TemporaryRepository {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
}
