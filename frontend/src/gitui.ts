import { ApiError, api, jsonRequest } from "./api.js";
import type { GitChange, GitPrepareResponse, GitPublishResponse, GitStatusResponse, GitSyncResponse, PostDocumentController } from "./types.js";
import { modalButton, openModal, showModalError, showToast } from "./modal.js";

export interface GitUiController { refresh(): Promise<void>; }

function kindMarker(kind: GitChange["kind"]) {
  return { added: "A", modified: "M", renamed: "R", deleted: "D" }[kind];
}

export function initGitUi(documents: PostDocumentController): GitUiController {
  const publish = document.getElementById("git-publish") as HTMLButtonElement;
  const sync = document.getElementById("git-sync") as HTMLButtonElement;
  const badge = document.getElementById("git-badge")!;
  let status: GitStatusResponse | null = null;
  let statusError = false;
  let refreshing = false;

  function updateControl() {
    const count = status?.changes.length ?? 0;
    const unpushed = status?.unpushed ?? false;
    const blocked = status?.repo_blocked;
    publish.classList.toggle("warning", statusError || Boolean(blocked));
    publish.classList.toggle("unpushed", unpushed);
    publish.disabled = Boolean(blocked) || (!statusError && !unpushed && count === 0);
    badge.hidden = count === 0 && !unpushed;
    badge.textContent = count > 0 ? (count > 9 ? "9+" : String(count)) : "";
    const label = statusError ? "Repository status unavailable" : unpushed ? "Retry push" : blocked
      ? `Publishing blocked by unfinished ${blocked}`
      : count ? `Commit and push ${count} changed file${count === 1 ? "" : "s"}` : "No uncommitted changes";
    publish.title = label;
    publish.setAttribute("aria-label", label);
  }

  async function refresh() {
    if (refreshing) return;
    refreshing = true;
    try {
      status = await api<GitStatusResponse>("/api/git/status");
      statusError = false;
    } catch (error) {
      statusError = true;
      if (error instanceof ApiError && error.status === 401) return;
    } finally {
      refreshing = false;
      updateControl();
    }
  }

  function renderFiles(container: HTMLElement, files: GitChange[]) {
    container.replaceChildren();
    const list = document.createElement("ul");
    list.className = "git-file-list";
    for (const file of files) {
      const item = document.createElement("li");
      const marker = document.createElement("span");
      marker.className = `git-kind ${file.kind}`;
      marker.textContent = kindMarker(file.kind);
      const path = document.createElement("span");
      path.textContent = file.path;
      item.append(marker, path);
      list.appendChild(item);
    }
    container.appendChild(list);
  }

  function showPushResult(result: GitPublishResponse) {
    if (result.status === "pushed") {
      showToast(`Committed ${result.commit.slice(0, 8)}`);
      window.dispatchEvent(new CustomEvent("blogger-git-complete"));
      void refresh();
      return;
    }
    showToast(`Committed, but push failed: ${result.error || "Unknown push error"}`, "warning", {
      label: "Retry push",
      run: () => void retryPush(),
    });
    window.dispatchEvent(new CustomEvent("blogger-git-complete"));
    void refresh();
  }

  async function retryPush() {
    publish.disabled = true;
    publish.title = "Retrying push…";
    try {
      showPushResult(await api<GitPublishResponse>("/api/git/retry-push", jsonRequest("POST")));
    } catch (error) {
      showToast(error instanceof Error ? error.message : "Push retry failed", "warning");
      await refresh();
    }
  }

  function openPublishDialog(prepared: GitPrepareResponse) {
    const modal = openModal("Commit and push", "git-dialog");
    const intro = document.createElement("p");
    intro.className = "modal-copy";
    intro.textContent = prepared.behind
      ? "Remote-only changes will be incorporated before this commit is pushed."
      : "The complete checkout changes below will be committed and pushed.";
    const files = document.createElement("div");
    renderFiles(files, prepared.files);
    const label = document.createElement("label");
    label.className = "modal-field";
    const caption = document.createElement("span");
    caption.className = "image-dialog-label";
    caption.textContent = "Commit subject";
    const subject = document.createElement("input");
    subject.className = "image-dialog-input";
    subject.value = prepared.subject;
    label.append(caption, subject);
    modal.body.append(intro, files, label);
    const cancel = modalButton("Cancel");
    const confirm = modalButton("Commit and push", "confirm");
    cancel.addEventListener("click", modal.close);

    let currentFiles = prepared.files;
    confirm.addEventListener("click", async () => {
      const commitSubject = subject.value.trim();
      if (!commitSubject) {
        showModalError(modal, "Enter a commit subject.");
        return;
      }
      confirm.disabled = true;
      cancel.disabled = true;
      subject.disabled = true;
      confirm.textContent = "Publishing…";
      try {
        const result = await api<GitPublishResponse>("/api/git/commit-push", jsonRequest("POST", {
          subject: commitSubject,
          files: currentFiles.map((file) => file.path),
        }));
        modal.close();
        showPushResult(result);
      } catch (error) {
        if (error instanceof ApiError && error.status === 409 && error.body.files && error.body.subject) {
          currentFiles = error.body.files;
          renderFiles(files, currentFiles);
          subject.value = error.body.subject;
          showModalError(modal, `${error.message}. Review the updated list and confirm again.`);
        } else {
          showModalError(modal, error instanceof Error ? error.message : "Publishing failed");
        }
        confirm.disabled = false;
        cancel.disabled = false;
        subject.disabled = false;
        confirm.textContent = "Commit and push";
      }
    });
    modal.actions.append(cancel, confirm);
    requestAnimationFrame(() => subject.focus());
  }

  async function preparePublish() {
    if (status?.unpushed) {
      await retryPush();
      return;
    }
    const flushed = await documents.flush();
    if (!flushed) {
      showToast("Resolve the current post save issue before publishing.", "warning");
      return;
    }
    publish.disabled = true;
    try {
      const prepared = await api<GitPrepareResponse>("/api/git/prepare", jsonRequest("POST"));
      if (prepared.files.length === 0) {
        showToast("There are no checkout changes to commit.");
      } else {
        openPublishDialog(prepared);
      }
    } catch (error) {
      if (error instanceof ApiError && error.status === 409 && error.body.unpushed) {
        showToast(error.message, "warning", { label: "Retry push", run: () => void retryPush() });
      } else if (error instanceof ApiError && error.status === 409 && error.body.overlapping_paths) {
        const modal = openModal("Manual Git resolution required");
        const text = document.createElement("p");
        text.className = "modal-copy";
        text.textContent = error.message;
        const paths = document.createElement("pre");
        paths.className = "git-overlap-paths";
        paths.textContent = [
          `Overlapping: ${error.body.overlapping_paths.join(", ")}`,
          `Local: ${(error.body.local_paths || []).join(", ")}`,
          `Remote: ${(error.body.remote_paths || []).join(", ")}`,
        ].join("\n");
        modal.body.append(text, paths);
        const close = modalButton("Close");
        close.addEventListener("click", modal.close);
        modal.actions.append(close);
      } else {
        showToast(error instanceof Error ? error.message : "Could not prepare publication", "warning");
      }
    } finally {
      await refresh();
    }
  }

  async function syncFromGitHub() {
    sync.disabled = true;
    sync.classList.add("busy");
    try {
      const result = await api<GitSyncResponse>("/api/git/sync", jsonRequest("POST"));
      showToast(result.updated ? "Synced from GitHub" : "Already up to date");
      window.dispatchEvent(new CustomEvent("blogger-git-complete", { detail: { sync: true } }));
      window.dispatchEvent(new CustomEvent("blogger-preview-refresh"));
      await documents.checkActiveRevision();
    } catch (error) {
      showToast(error instanceof Error ? error.message : "Sync failed", "warning");
    } finally {
      sync.disabled = false;
      sync.classList.remove("busy");
      await refresh();
    }
  }

  publish.addEventListener("click", () => void preparePublish());
  sync.addEventListener("click", () => void syncFromGitHub());
  window.addEventListener("focus", () => void refresh());
  window.addEventListener("blogger-mutation", () => void refresh());
  window.addEventListener("blogger-archive-open", () => void refresh());
  window.addEventListener("blogger-git-complete", () => void refresh());
  setInterval(() => {
    if (document.visibilityState === "visible") void refresh();
  }, 30_000);
  void refresh();
  return { refresh };
}
