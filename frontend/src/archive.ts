import { ApiError, api, jsonRequest, mutationEvent } from "./api.js";
import type { ArchivePost, CreatePostResponse, PostDocumentController, PostResponse, PostsResponse, RenamePostResponse, RenamePreviewResponse } from "./types.js";
import { field, modalButton, openModal, showModalError, showToast } from "./modal.js";
import type { GitUiController } from "./gitui.js";

const WIDTH_KEY = "blogger-archive-width";
const MIN_WIDTH = 220;
const MAX_WIDTH = 420;

function slugify(value: string) {
  return value.toLocaleLowerCase().normalize("NFKD").replace(/[\u0300-\u036f]/g, "")
    .replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "");
}

function localDateTimeValue(date = new Date()) {
  const local = new Date(date.getTime() - date.getTimezoneOffset() * 60_000);
  return local.toISOString().slice(0, 16);
}

function filename(path: string) {
  return (path.split("/").pop() || path).replace(/\.md$/, "");
}

function archiveDateParts(value: string | null): { year: number; month: number } | null {
  const match = value?.match(/^(\d{4})-(\d{2})-/);
  if (!match) return null;
  const year = Number(match[1]);
  const month = Number(match[2]) - 1;
  return month >= 0 && month <= 11 ? { year, month } : null;
}

export interface ArchiveController {
  open(): void;
  close(): void;
  refresh(): Promise<void>;
}

export function initArchive(documents: PostDocumentController, git: GitUiController): ArchiveController {
  const panel = document.getElementById("archive-panel")!;
  const toggle = document.getElementById("archive-toggle") as HTMLButtonElement;
  const divider = document.getElementById("archive-divider")!;
  const backdrop = document.getElementById("archive-backdrop")!;
  const content = document.getElementById("archive-content")!;
  const refreshButton = document.getElementById("archive-refresh") as HTMLButtonElement;
  const newButton = document.getElementById("new-post") as HTMLButtonElement;
  const emptyOpen = document.getElementById("empty-open-archive") as HTMLButtonElement;
  let posts: ArchivePost[] = [];
  let loaded = false;
  let loading = false;
  let selectedPath: string | null = documents.getActivePath();
  const collapsedGroups = new Set<string>();

  const storedWidth = Number(localStorage.getItem(WIDTH_KEY));
  panel.style.setProperty("--archive-width", `${Number.isFinite(storedWidth) ? Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, storedWidth)) : 260}px`);
  panel.classList.remove("open");

  function phone() { return window.matchMedia("(max-width: 768px)").matches; }

  function open() {
    panel.classList.add("open");
    document.body.classList.toggle("archive-drawer-open", phone());
    toggle.setAttribute("aria-expanded", "true");
    toggle.setAttribute("aria-label", "Close posts");
    toggle.title = "Close posts";
    window.dispatchEvent(new CustomEvent("blogger-archive-open"));
    window.setTimeout(() => window.dispatchEvent(new Event("resize")), 190);
    void refresh();
  }

  function close() {
    panel.classList.remove("open");
    document.body.classList.remove("archive-drawer-open");
    toggle.setAttribute("aria-expanded", "false");
    toggle.setAttribute("aria-label", "Open posts");
    toggle.title = "Open posts";
    window.setTimeout(() => window.dispatchEvent(new Event("resize")), 190);
  }

  function groupToggle(label: string, key: string, depth: "year" | "month") {
    const button = document.createElement("button");
    button.type = "button";
    button.className = `archive-group-toggle ${depth}`;
    const collapsed = collapsedGroups.has(key);
    button.setAttribute("aria-expanded", String(!collapsed));
    button.innerHTML = `<span class="archive-chevron" aria-hidden="true">⌄</span><span></span>`;
    button.lastElementChild!.textContent = label;
    button.classList.toggle("collapsed", collapsed);
    button.addEventListener("click", () => {
      if (collapsedGroups.has(key)) collapsedGroups.delete(key); else collapsedGroups.add(key);
      render();
    });
    return button;
  }

  function showPostMenu(post: ArchivePost, anchor: HTMLElement) {
    document.querySelector(".post-action-popover")?.remove();
    const menu = document.createElement("div");
    menu.className = "post-action-popover";
    menu.setAttribute("role", "menu");
    const rename = document.createElement("button");
    rename.type = "button";
    rename.textContent = "Rename…";
    const remove = document.createElement("button");
    remove.type = "button";
    remove.textContent = "Delete…";
    remove.className = "danger";
    rename.addEventListener("click", () => { menu.remove(); openRename(post); });
    remove.addEventListener("click", () => { menu.remove(); openDelete(post); });
    menu.append(rename, remove);
    document.body.appendChild(menu);
    const rect = anchor.getBoundingClientRect();
    menu.style.top = `${Math.min(window.innerHeight - menu.offsetHeight - 8, rect.bottom + 2)}px`;
    menu.style.left = `${Math.max(8, rect.right - menu.offsetWidth)}px`;
    const dismiss = (event: MouseEvent) => {
      if (!menu.contains(event.target as Node) && event.target !== anchor) {
        menu.remove();
        document.removeEventListener("mousedown", dismiss);
      }
    };
    setTimeout(() => document.addEventListener("mousedown", dismiss), 0);
  }

  function postRow(post: ArchivePost) {
    const row = document.createElement("div");
    row.className = "archive-post-row";
    row.classList.toggle("selected", post.path === selectedPath);
    const select = document.createElement("button");
    select.type = "button";
    select.className = "archive-post-select";
    select.title = post.path;
    const title = document.createElement("span");
    title.className = "archive-post-title";
    title.textContent = post.title;
    select.appendChild(title);
    if (post.draft) {
      const draft = document.createElement("span");
      draft.className = "draft-badge";
      draft.textContent = "Draft";
      select.appendChild(draft);
    }
    select.addEventListener("click", async () => {
      try {
        if (await documents.openPost(post.path)) {
          selectedPath = post.path;
          render();
          if (phone()) close();
        }
      } catch (error) {
        showToast(error instanceof Error ? error.message : "Could not open post", "warning");
      }
    });
    const action = document.createElement("button");
    action.type = "button";
    action.className = "post-action-button";
    action.setAttribute("aria-label", `Actions for ${post.title}`);
    action.title = "Post actions";
    action.textContent = "⋮";
    action.addEventListener("click", () => showPostMenu(post, action));
    row.append(select, action);
    return row;
  }

  function render() {
    content.replaceChildren();
    if (!loaded) {
      const skeleton = document.createElement("div");
      skeleton.className = "archive-skeleton";
      for (let index = 0; index < 5; index++) skeleton.appendChild(document.createElement("span"));
      content.appendChild(skeleton);
      return;
    }
    if (posts.length === 0) {
      const empty = document.createElement("div");
      empty.className = "archive-state";
      empty.textContent = "No blog posts found";
      content.appendChild(empty);
      return;
    }
    const sorted = [...posts].sort((a, b) => (b.date || "").localeCompare(a.date || "") || a.title.localeCompare(b.title));
    const dated = sorted.filter((post) => !post.unsorted && archiveDateParts(post.date));
    const unsorted = sorted.filter((post) => !dated.includes(post));
    const byYear = new Map<number, Map<number, ArchivePost[]>>();
    for (const post of dated) {
      const date = archiveDateParts(post.date)!;
      const year = date.year;
      const month = date.month;
      if (!byYear.has(year)) byYear.set(year, new Map());
      if (!byYear.get(year)!.has(month)) byYear.get(year)!.set(month, []);
      byYear.get(year)!.get(month)!.push(post);
    }
    for (const year of [...byYear.keys()].sort((a, b) => b - a)) {
      const yearKey = `year:${year}`;
      content.appendChild(groupToggle(String(year), yearKey, "year"));
      if (collapsedGroups.has(yearKey)) continue;
      const months = byYear.get(year)!;
      for (const month of [...months.keys()].sort((a, b) => b - a)) {
        const monthKey = `${yearKey}:month:${month}`;
        const label = new Intl.DateTimeFormat(undefined, { month: "long" }).format(new Date(year, month, 1));
        content.appendChild(groupToggle(label, monthKey, "month"));
        if (!collapsedGroups.has(monthKey)) months.get(month)!.forEach((post) => content.appendChild(postRow(post)));
      }
    }
    if (unsorted.length) {
      const key = "unsorted";
      content.appendChild(groupToggle("Unsorted", key, "year"));
      if (!collapsedGroups.has(key)) unsorted.forEach((post) => content.appendChild(postRow(post)));
    }
    document.querySelector(".archive-post-row.selected")?.scrollIntoView({ block: "nearest" });
  }

  function expandSelected(path: string | null) {
    const post = posts.find((item) => item.path === path);
    if (!post || post.unsorted || !post.date) {
      if (post) collapsedGroups.delete("unsorted");
      return;
    }
    const date = archiveDateParts(post.date);
    if (!date) return;
    const yearKey = `year:${date.year}`;
    collapsedGroups.delete(yearKey);
    collapsedGroups.delete(`${yearKey}:month:${date.month}`);
  }

  async function refresh() {
    if (loading) return;
    loading = true;
    refreshButton.classList.add("busy");
    try {
      const result = await api<PostsResponse>("/api/posts");
      posts = result.posts;
      loaded = true;
      render();
    } catch (error) {
      if (!loaded) {
        content.replaceChildren();
        const state = document.createElement("div");
        state.className = "archive-state error";
        const message = document.createElement("p");
        message.textContent = error instanceof Error ? error.message : "Could not scan blog posts";
        const retry = document.createElement("button");
        retry.type = "button";
        retry.textContent = "Retry";
        retry.addEventListener("click", () => void refresh());
        state.append(message, retry);
        content.appendChild(state);
      } else {
        showToast(error instanceof Error ? error.message : "Archive refresh failed", "warning");
      }
    } finally {
      loading = false;
      refreshButton.classList.remove("busy");
    }
  }

  function openNewPost() {
    const modal = openModal("New post");
    const title = document.createElement("input");
    const slug = document.createElement("input");
    const date = document.createElement("input");
    const draft = document.createElement("input");
    title.type = "text";
    slug.type = "text";
    date.type = "datetime-local";
    date.value = localDateTimeValue();
    draft.type = "checkbox";
    draft.checked = true;
    const draftLabel = document.createElement("label");
    draftLabel.className = "modal-checkbox";
    draftLabel.append(draft, document.createTextNode("Save as draft"));
    modal.body.append(field("Title", title), field("Slug", slug), field("Date and time", date), draftLabel);
    let slugEdited = false;
    slug.addEventListener("input", () => { slugEdited = true; });
    title.addEventListener("input", () => { if (!slugEdited) slug.value = slugify(title.value); });
    const cancel = modalButton("Cancel");
    const create = modalButton("Create post", "confirm");
    cancel.addEventListener("click", modal.close);
    create.addEventListener("click", async () => {
      create.disabled = true;
      try {
        const result = await api<CreatePostResponse>("/api/post/create", jsonRequest("POST", {
          title: title.value.trim(), slug: slug.value.trim(),
          date: new Date(date.value).toISOString(), draft: draft.checked,
        }));
        modal.close();
        const loaded: PostResponse = { ...result, title: title.value.trim() };
        await documents.openPost(result.path, loaded);
        selectedPath = result.path;
        mutationEvent({ kind: "post-create" });
        await Promise.all([refresh(), git.refresh()]);
        if (phone()) close();
      } catch (error) {
        showModalError(modal, error instanceof Error ? error.message : "Could not create post");
        create.disabled = false;
      }
    });
    modal.actions.append(cancel, create);
    requestAnimationFrame(() => title.focus());
  }

  function openRename(post: ArchivePost) {
    const modal = openModal("Rename post");
    const input = document.createElement("input");
    input.type = "text";
    input.value = filename(post.path);
    const preview = document.createElement("div");
    preview.className = "rename-preview";
    modal.body.append(field("Filename", input), preview);
    let timer: ReturnType<typeof setTimeout> | null = null;
    const updatePreview = () => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(async () => {
        try {
          const result = await api<RenamePreviewResponse>(`/api/post/rename-preview?path=${encodeURIComponent(post.path)}&new_filename=${encodeURIComponent(input.value)}`);
          preview.textContent = `${result.old_url} → ${result.new_url}${result.url_changes ? " · Public URL will change" : ""}`;
          preview.classList.toggle("warning", result.url_changes);
        } catch (error) {
          preview.textContent = error instanceof Error ? error.message : "Preview unavailable";
          preview.classList.add("warning");
        }
      }, 300);
    };
    input.addEventListener("input", updatePreview);
    updatePreview();
    const cancel = modalButton("Cancel");
    const rename = modalButton("Rename", "confirm");
    cancel.addEventListener("click", modal.close);
    rename.addEventListener("click", async () => {
      rename.disabled = true;
      try {
        if (!(await documents.flush(post.path))) throw new Error("Resolve unsaved changes before renaming.");
        const revision = await documents.getRevision(post.path);
        const result = await api<RenamePostResponse>("/api/post/rename", jsonRequest("POST", {
          path: post.path, new_filename: input.value.trim(), base_revision: revision,
        }));
        documents.renameDocument(post.path, result.path, result.url);
        if (selectedPath === post.path) selectedPath = result.path;
        modal.close();
        mutationEvent({ kind: "post-rename" });
        await Promise.all([refresh(), git.refresh()]);
      } catch (error) {
        showModalError(modal, error instanceof Error ? error.message : "Could not rename post");
        rename.disabled = false;
      }
    });
    modal.actions.append(cancel, rename);
    requestAnimationFrame(() => input.focus());
  }

  function openDelete(post: ArchivePost) {
    const modal = openModal("Delete post");
    const copy = document.createElement("p");
    copy.className = "modal-copy";
    copy.textContent = `Delete “${post.title}” (${post.path})? This will permanently remove the Markdown file. An uncommitted post may not be recoverable.`;
    modal.body.appendChild(copy);
    const cancel = modalButton("Cancel");
    const remove = modalButton("Delete post", "delete");
    cancel.addEventListener("click", modal.close);
    if (documents.isDirty(post.path)) {
      const note = document.createElement("p");
      note.className = "modal-error static";
      note.textContent = "This post has unsaved changes in this browser. Save or discard them before deleting.";
      modal.body.appendChild(note);
      remove.disabled = true;
    }
    remove.addEventListener("click", async () => {
      remove.disabled = true;
      try {
        const revision = await documents.getRevision(post.path);
        await api<Record<string, never>>("/api/post/delete", jsonRequest("POST", { path: post.path, base_revision: revision }));
        modal.close();
        posts = posts.filter((item) => item.path !== post.path);
        documents.disposeDocument(post.path);
        if (selectedPath === post.path) {
          selectedPath = null;
          documents.clearSelection(true);
        }
        render();
        mutationEvent({ kind: "post-delete" });
        await git.refresh();
      } catch (error) {
        showModalError(modal, error instanceof Error ? error.message : "Could not delete post");
        remove.disabled = false;
      }
    });
    modal.actions.append(cancel, remove);
  }

  toggle.addEventListener("click", () => panel.classList.contains("open") ? close() : open());
  backdrop.addEventListener("click", close);
  emptyOpen.addEventListener("click", open);
  refreshButton.addEventListener("click", () => { void refresh(); void git.refresh(); });
  newButton.addEventListener("click", openNewPost);
  document.addEventListener("keydown", (event) => { if (event.key === "Escape" && phone() && panel.classList.contains("open")) close(); });
  window.addEventListener("resize", () => { if (!phone()) document.body.classList.remove("archive-drawer-open"); });
  window.addEventListener("blogger-post-selected", (event: Event) => {
    selectedPath = (event as CustomEvent<{ path: string | null }>).detail.path;
    expandSelected(selectedPath);
    if (loaded) render();
  });
  window.addEventListener("blogger-mutation", () => void refresh());
  window.addEventListener("blogger-git-complete", () => void refresh());

  divider.addEventListener("mousedown", (event) => {
    if (phone() || !panel.classList.contains("open")) return;
    event.preventDefault();
    const move = (moveEvent: MouseEvent) => {
      const width = Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, moveEvent.clientX));
      panel.style.setProperty("--archive-width", `${width}px`);
    };
    const up = () => {
      const width = Math.round(panel.getBoundingClientRect().width);
      localStorage.setItem(WIDTH_KEY, String(width));
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  });

  render();
  const controller = { open, close, refresh };
  documents.setArchiveOpener(open);
  return controller;
}
