type AppTheme = "dark" | "light";

const THEME_STORAGE_KEY = "blogger-theme";

function getPreferredTheme(): AppTheme {
  const stored = localStorage.getItem(THEME_STORAGE_KEY);
  if (stored === "dark" || stored === "light") return stored;
  return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

function applyTheme(theme: AppTheme) {
  document.documentElement.dataset.theme = theme;
  document.documentElement.style.colorScheme = theme;
  window.dispatchEvent(new CustomEvent("blogger-theme-change", { detail: { theme } }));
}

export function initThemeToggle() {
  const toggle = document.getElementById("theme-toggle") as HTMLButtonElement | null;
  const iconSun = document.getElementById("theme-icon-sun");
  const iconMoon = document.getElementById("theme-icon-moon");
  let theme = getPreferredTheme();

  function syncToggle() {
    applyTheme(theme);
    if (!toggle) return;
    const isLight = theme === "light";
    toggle.setAttribute("aria-pressed", String(isLight));
    toggle.setAttribute("aria-label", isLight ? "Switch to dark mode" : "Switch to light mode");
    toggle.title = isLight ? "Switch to dark mode" : "Switch to light mode";
    iconSun?.toggleAttribute("hidden", !isLight);
    iconMoon?.toggleAttribute("hidden", isLight);
  }

  syncToggle();

  toggle?.addEventListener("click", () => {
    theme = theme === "dark" ? "light" : "dark";
    localStorage.setItem(THEME_STORAGE_KEY, theme);
    syncToggle();
  });
}

export function initPaneToggles() {
  const leftPane = document.getElementById("pane-left")!;
  const rightPane = document.getElementById("pane-right")!;
  const dividerLeft = document.getElementById("divider-left")!;
  const dividerRight = document.getElementById("divider-right")!;
  const toggleLeft = document.getElementById("toggle-left")!;
  const toggleRight = document.getElementById("toggle-right")!;

  toggleLeft.classList.add("active");
  toggleRight.classList.add("active");

  toggleLeft.addEventListener("click", () => {
    const collapsed = leftPane.classList.toggle("collapsed");
    dividerLeft.style.display = collapsed ? "none" : "";
    toggleLeft.classList.toggle("active", !collapsed);
  });

  toggleRight.addEventListener("click", () => {
    const collapsed = rightPane.classList.toggle("collapsed");
    dividerRight.style.display = collapsed ? "none" : "";
    toggleRight.classList.toggle("active", !collapsed);
  });
}

const BLOG_PREVIEW_WIDTHS = [850, 800, 700, 450, 350];
const TARGET_EDITOR_WIDTH = 760;
const MIN_EDITOR_WIDTH = 700;
const MIN_ASSISTANT_WIDTH = 360;
const DIVIDER_WIDTH = 4;
const COMPACT_LAYOUT_WIDTH = 768;
const TABBED_LAYOUT_WIDTH = 1440;
const WIDE_ASSISTANT_WIDTH = 550;

function choosePreviewWidth(workspaceWidth: number, rightPaneVisible: boolean) {
  const reservedWidth =
    MIN_EDITOR_WIDTH +
    DIVIDER_WIDTH +
    (rightPaneVisible ? MIN_ASSISTANT_WIDTH + DIVIDER_WIDTH : 0);
  const availablePreviewWidth = workspaceWidth - reservedWidth;

  for (const width of BLOG_PREVIEW_WIDTHS) {
    if (width <= availablePreviewWidth) return width;
  }

  return BLOG_PREVIEW_WIDTHS[BLOG_PREVIEW_WIDTHS.length - 1];
}

export function initResponsivePreviewPane() {
  const workspace = document.querySelector(".workspace") as HTMLElement;
  const leftPane = document.getElementById("pane-left")!;
  const rightPane = document.getElementById("pane-right")!;
  const toggleLeft = document.getElementById("toggle-left")!;
  const toggleRight = document.getElementById("toggle-right")!;

  function applyPreviewWidth() {
    if (window.innerWidth <= COMPACT_LAYOUT_WIDTH || leftPane.classList.contains("collapsed")) {
      return;
    }

    const workspaceWidth = workspace.getBoundingClientRect().width;
    const rightPaneVisible = !rightPane.classList.contains("collapsed");
    const preferredAssistantWidth = rightPaneVisible
      ? Math.min(WIDE_ASSISTANT_WIDTH, Math.max(MIN_ASSISTANT_WIDTH, workspaceWidth * 0.28))
      : 0;

    const width = choosePreviewWidth(workspaceWidth, rightPaneVisible);

    leftPane.style.flex = `0 0 ${width}px`;
    leftPane.dataset.previewWidth = `${width}`;

    if (rightPaneVisible) {
      const remainingWidth =
        workspaceWidth - width - TARGET_EDITOR_WIDTH - DIVIDER_WIDTH * 2;
      const assistantWidth = Math.max(
        MIN_ASSISTANT_WIDTH,
        Math.min(preferredAssistantWidth, remainingWidth)
      );
      rightPane.style.flex = `0 0 ${assistantWidth}px`;
    }
  }

  applyPreviewWidth();
  window.addEventListener("resize", applyPreviewWidth);
  toggleLeft.addEventListener("click", applyPreviewWidth);
  toggleRight.addEventListener("click", applyPreviewWidth);
}

type WorkPane = "preview" | "editor" | "assistant";

export function initWorkPaneTabs() {
  const topbarCenter = document.querySelector(".topbar-center")!;
  const previewPane = document.getElementById("pane-left")!;
  const editorPane = document.getElementById("pane-center")!;
  const assistantPane = document.getElementById("pane-right")!;
  const dividerLeft = document.getElementById("divider-left")!;
  const dividerRight = document.getElementById("divider-right")!;
  const toggleLeft = document.getElementById("toggle-left")!;
  const toggleRight = document.getElementById("toggle-right")!;
  const assistantMessages = document.getElementById("assistant-messages")!;

  let activePane: WorkPane = "editor";
  let assistantHasActivity = false;
  let tabbedLayoutActive = false;
  let phoneLayoutActive = false;

  const tabs = document.createElement("div");
  tabs.className = "work-pane-tabs";
  tabs.setAttribute("role", "tablist");

  const previewTab = document.createElement("button");
  previewTab.className = "work-pane-tab preview-tab";
  previewTab.type = "button";
  previewTab.textContent = "Preview";
  previewTab.setAttribute("role", "tab");
  previewTab.setAttribute("aria-selected", "false");

  const editorTab = document.createElement("button");
  editorTab.className = "work-pane-tab active";
  editorTab.type = "button";
  editorTab.textContent = "Editor";
  editorTab.setAttribute("role", "tab");
  editorTab.setAttribute("aria-selected", "true");

  const assistantTab = document.createElement("button");
  assistantTab.className = "work-pane-tab";
  assistantTab.type = "button";
  assistantTab.textContent = "Assistant";
  assistantTab.setAttribute("role", "tab");
  assistantTab.setAttribute("aria-selected", "false");

  const activityDot = document.createElement("span");
  activityDot.className = "tab-activity-dot";
  activityDot.setAttribute("aria-hidden", "true");
  assistantTab.appendChild(activityDot);

  tabs.appendChild(previewTab);
  tabs.appendChild(editorTab);
  tabs.appendChild(assistantTab);
  topbarCenter.appendChild(tabs);

  function updateTabs() {
    const previewActive = activePane === "preview";
    const assistantActive = activePane === "assistant";
    previewTab.classList.toggle("active", previewActive);
    editorTab.classList.toggle("active", activePane === "editor");
    assistantTab.classList.toggle("active", assistantActive);
    assistantTab.classList.toggle("has-activity", assistantHasActivity);
    previewTab.setAttribute("aria-selected", String(previewActive));
    editorTab.setAttribute("aria-selected", String(activePane === "editor"));
    assistantTab.setAttribute("aria-selected", String(assistantActive));
  }

  function selectPane(pane: WorkPane) {
    activePane = pane;
    if (pane === "assistant") {
      assistantHasActivity = false;
    }

    if (tabbedLayoutActive) {
      previewPane.classList.toggle("tab-hidden", phoneLayoutActive && pane !== "preview");
      editorPane.classList.toggle("tab-hidden", pane !== "editor");
      assistantPane.classList.toggle("tab-hidden", pane !== "assistant");
    }

    updateTabs();
  }

  function applyLayout() {
    tabbedLayoutActive = window.innerWidth <= TABBED_LAYOUT_WIDTH;
    phoneLayoutActive = window.innerWidth <= COMPACT_LAYOUT_WIDTH;
    document.body.classList.toggle("work-tabs-active", tabbedLayoutActive);
    document.body.classList.toggle("phone-tabs-active", phoneLayoutActive);

    if (tabbedLayoutActive) {
      dividerRight.style.display = "none";
      toggleRight.hidden = true;
      assistantPane.classList.remove("collapsed");
      toggleRight.classList.add("active");
      editorPane.style.flex = "1 1 auto";
      assistantPane.style.flex = "1 1 auto";

      if (phoneLayoutActive) {
        dividerLeft.style.display = "none";
        toggleLeft.hidden = true;
        previewPane.classList.remove("collapsed");
        toggleLeft.classList.add("active");
        previewPane.style.flex = "1 1 auto";
      } else {
        dividerLeft.style.display = "";
        toggleLeft.hidden = false;
        previewPane.classList.remove("tab-hidden");
        if (activePane === "preview") activePane = "editor";
      }

      selectPane(activePane);
    } else {
      dividerLeft.style.display = "";
      dividerRight.style.display = "";
      toggleLeft.hidden = false;
      toggleRight.hidden = false;
      editorPane.style.flex = "";
      previewPane.classList.remove("tab-hidden");
      editorPane.classList.remove("tab-hidden");
      assistantPane.classList.remove("tab-hidden");
      assistantHasActivity = false;
      updateTabs();
    }
  }

  previewTab.addEventListener("click", () => selectPane("preview"));
  editorTab.addEventListener("click", () => selectPane("editor"));
  assistantTab.addEventListener("click", () => selectPane("assistant"));
  window.addEventListener("resize", applyLayout);

  const observer = new MutationObserver(() => {
    if (tabbedLayoutActive && activePane !== "assistant") {
      assistantHasActivity = true;
      updateTabs();
    }
  });
  observer.observe(assistantMessages, { childList: true, subtree: true });

  applyLayout();
}

export function initDividerDrag(
  dividerId: string,
  leftPaneId: string,
  rightPaneId: string
) {
  const divider = document.getElementById(dividerId)!;
  const leftPane = document.getElementById(leftPaneId)!;
  const workspace = document.querySelector(".workspace") as HTMLElement;

  let dragging = false;

  divider.addEventListener("mousedown", (e: MouseEvent) => {
    if (divider.offsetParent === null) return;

    e.preventDefault();
    dragging = true;
    divider.classList.add("dragging");
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";

    const iframes = document.querySelectorAll("iframe");
    iframes.forEach((f) => (f.style.pointerEvents = "none"));

    const onMove = (ev: MouseEvent) => {
      if (!dragging) return;
      const rect = workspace.getBoundingClientRect();
      const pct = ((ev.clientX - rect.left) / rect.width) * 100;

      if (dividerId === "divider-left") {
        const clamped = Math.max(15, Math.min(50, pct));
        leftPane.style.flex = `0 0 ${clamped}%`;
      } else {
        const rightEl = document.getElementById(rightPaneId)!;
        const rightPct = 100 - pct;
        const clamped = Math.max(15, Math.min(50, rightPct));
        rightEl.style.flex = `0 0 ${clamped}%`;
      }
    };

    const onUp = () => {
      dragging = false;
      divider.classList.remove("dragging");
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      iframes.forEach((f) => (f.style.pointerEvents = ""));
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };

    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  });
}

export function initUrlBar() {
  const input = document.getElementById("url-input") as HTMLInputElement;
  const goBtn = document.getElementById("url-go")!;
  const iframe = document.getElementById("ref-iframe") as HTMLIFrameElement;

  function navigate() {
    let url = input.value.trim();
    if (url && !url.startsWith("http://") && !url.startsWith("https://")) {
      url = "https://" + url;
      input.value = url;
    }
    if (url) iframe.src = url;
  }

  goBtn.addEventListener("click", navigate);
  input.addEventListener("keydown", (e: KeyboardEvent) => {
    if (e.key === "Enter") navigate();
  });
}

export async function initPreview() {
  const iframe = document.getElementById("ref-iframe") as HTMLIFrameElement;
  const input = document.getElementById("url-input") as HTMLInputElement;

  let previewUrl: string | null = null;

  for (let attempt = 0; attempt < 120; attempt++) {
    try {
      const res = await fetch("/api/preview");
      if (res.ok) {
        const data = await res.json();
        if (data.url) {
          previewUrl = data.url;
          iframe.src = data.url;
          input.value = data.url;
          break;
        }
      }
    } catch {
      // server not ready yet
    }
    await new Promise((r) => setTimeout(r, 500));
  }

  if (!previewUrl) return;

  let lastContentLength: string | null = null;

  setInterval(async () => {
    try {
      const res = await fetch("/api/preview-check");
      if (!res.ok) return;
      const data = await res.json();
      const cl = data.content_length as string | null;
      if (!cl) return;
      if (lastContentLength === null) {
        lastContentLength = cl;
      } else if (cl !== lastContentLength) {
        lastContentLength = cl;
        iframe.src = previewUrl!;
      }
    } catch {
      // ignore poll errors
    }
  }, 2000);
}
