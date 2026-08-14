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

  function renderFiles(container: HTMLElement, files: GitChange[], selected: Set<string>, onChange: () => void) {
    container.replaceChildren();
    const list = document.createElement("ul");
    list.className = "git-file-list";
    for (const file of files) {
      const item = document.createElement("li");
      const checkbox = document.createElement("input");
      checkbox.type = "checkbox";
      checkbox.checked = selected.has(file.path);
      checkbox.setAttribute("aria-label", `Include ${file.path}`);
      checkbox.addEventListener("change", () => {
        if (checkbox.checked) selected.add(file.path);
        else selected.delete(file.path);
        onChange();
      });
      const marker = document.createElement("span");
      marker.className = `git-kind ${file.kind}`;
      marker.textContent = kindMarker(file.kind);
      const path = document.createElement("span");
      path.textContent = file.path;
      item.append(checkbox, marker, path);
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
      ? "Select the changes to commit. Remote-only changes will be incorporated before it is pushed."
      : "Select the changes to commit and push. Unselected work stays in the checkout.";
    const files = document.createElement("div");
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
    let overallSubject = prepared.subject;
    let lastSuggestedSubject = prepared.subject;
    const selectedPaths = new Set(currentFiles.map((file) => file.path));
    const updateSelection = () => {
      const count = selectedPaths.size;
      const selectedFiles = currentFiles.filter((file) => selectedPaths.has(file.path));
      const nextSuggestion = count === currentFiles.length
        ? overallSubject
        : count === 1 ? selectedFiles[0].subject : undefined;
      if (nextSuggestion && subject.value === lastSuggestedSubject) {
        subject.value = nextSuggestion;
        lastSuggestedSubject = nextSuggestion;
      }
      confirm.disabled = count === 0;
      confirm.textContent = count === 0
        ? "Select a change"
        : `Commit and push ${count} change${count === 1 ? "" : "s"}`;
    };
    renderFiles(files, currentFiles, selectedPaths, updateSelection);
    updateSelection();
    confirm.addEventListener("click", async () => {
      const commitSubject = subject.value.trim();
      if (!commitSubject) {
        showModalError(modal, "Enter a commit subject.");
        return;
      }
      confirm.disabled = true;
      cancel.disabled = true;
      subject.disabled = true;
      files.querySelectorAll<HTMLInputElement>("input[type=checkbox]").forEach((input) => { input.disabled = true; });
      confirm.textContent = "Publishing…";
      try {
        const result = await api<GitPublishResponse>("/api/git/commit-push", jsonRequest("POST", {
          subject: commitSubject,
          files: [...selectedPaths],
        }));
        modal.close();
        showPushResult(result);
      } catch (error) {
        if (error instanceof ApiError && error.status === 409 && error.body.files && error.body.subject) {
          const selectedEverything = selectedPaths.size === currentFiles.length;
          currentFiles = error.body.files;
          const availablePaths = new Set(currentFiles.map((file) => file.path));
          for (const path of selectedPaths) {
            if (!availablePaths.has(path)) selectedPaths.delete(path);
          }
          if (selectedEverything) {
            for (const file of currentFiles) selectedPaths.add(file.path);
          }
          renderFiles(files, currentFiles, selectedPaths, updateSelection);
          overallSubject = error.body.subject;
          lastSuggestedSubject = error.body.subject;
          subject.value = error.body.subject;
          showModalError(modal, `${error.message}. Review the updated list and confirm again.`);
        } else {
          showModalError(modal, error instanceof Error ? error.message : "Publishing failed");
        }
        updateSelection();
        cancel.disabled = false;
        subject.disabled = false;
        files.querySelectorAll<HTMLInputElement>("input[type=checkbox]").forEach((input) => { input.disabled = false; });
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
