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

  for (let attempt = 0; attempt < 120; attempt++) {
    try {
      const res = await fetch("/api/preview");
      if (res.ok) {
        const data = await res.json();
        if (data.url) {
          iframe.src = data.url;
          input.value = data.url;
          return;
        }
      }
    } catch {
      // server not ready yet
    }
    await new Promise((r) => setTimeout(r, 500));
  }
}
