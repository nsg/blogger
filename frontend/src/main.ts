declare const require: {
  config: (options: Record<string, unknown>) => void;
  (deps: string[], callback: (...args: unknown[]) => void): void;
};

declare const marked: {
  parse: (src: string) => string;
};

// --- Paragraph tracking types and state ---
interface ParagraphVersion { text: string; timestamp: number }
interface TrackedParagraph {
  id: string;
  currentText: string;
  history: ParagraphVersion[];
  startLine: number;
  endLine: number;
}

const paragraphMap: Map<string, TrackedParagraph> = new Map();
let currentParagraphId: string | null = null;
let anchorText: string | null = null;
let lastSuggestionTime = 0;
let debounceTimer: ReturnType<typeof setTimeout> | null = null;
let reconcileTimer: ReturnType<typeof setTimeout> | null = null;
let suggestionInFlight = false;
let lastSuggestedParaId: string | null = null;
let lastSuggestedVersion: string | null = null;
let postSuggestion: ((content: string, edits?: { old_text: string; new_text: string }[]) => void) | null = null;
let showFeedbackIndicator: (() => void) | null = null;
let hideFeedbackIndicator: (() => void) | null = null;

// --- Utility functions ---
function hashString(s: string): string {
  let h = 5381;
  for (let i = 0; i < s.length; i++) {
    h = ((h << 5) + h + s.charCodeAt(i)) | 0;
  }
  return (h >>> 0).toString(16);
}

interface ParsedParagraph { startLine: number; endLine: number; text: string }

function parseParagraphs(model: { getLineCount: () => number; getLineContent: (n: number) => string }): ParsedParagraph[] {
  const result: ParsedParagraph[] = [];
  const lineCount = model.getLineCount();
  let start = -1;
  let lines: string[] = [];

  for (let i = 1; i <= lineCount; i++) {
    const line = model.getLineContent(i);
    if (line.trim() === "") {
      if (start !== -1) {
        result.push({ startLine: start, endLine: i - 1, text: lines.join("\n") });
        start = -1;
        lines = [];
      }
    } else {
      if (start === -1) start = i;
      lines.push(line);
    }
  }
  if (start !== -1) {
    result.push({ startLine: start, endLine: lineCount, text: lines.join("\n") });
  }
  return result;
}

function getParagraphAtLine(lineNumber: number): string | null {
  for (const [id, p] of paragraphMap) {
    if (lineNumber >= p.startLine && lineNumber <= p.endLine) return id;
  }
  return null;
}

function computeChangeRatio(a: string, b: string): number {
  if (a === b) return 0;
  if (a.length === 0 || b.length === 0) return 1;
  const maxLen = Math.max(a.length, b.length);
  let common = 0;
  const shorter = a.length <= b.length ? a : b;
  const longer = a.length > b.length ? a : b;
  let j = 0;
  for (let i = 0; i < longer.length && j < shorter.length; i++) {
    if (longer[i] === shorter[j]) { common++; j++; }
  }
  return 1 - common / maxLen;
}

function canSuggest(): boolean {
  return !suggestionInFlight && Date.now() - lastSuggestionTime >= 30_000;
}

function reconcileParagraphs(model: { getLineCount: () => number; getLineContent: (n: number) => string }) {
  const parsed = parseParagraphs(model);
  const usedIds = new Set<string>();
  const existingEntries = Array.from(paragraphMap.values());

  let ei = 0;
  for (const pp of parsed) {
    let matched = false;
    // Greedy forward scan: find first unmatched existing paragraph that's similar
    for (let k = ei; k < existingEntries.length; k++) {
      const existing = existingEntries[k];
      if (usedIds.has(existing.id)) continue;
      const ratio = computeChangeRatio(existing.currentText, pp.text);
      if (ratio < 0.7) {
        // Match found
        usedIds.add(existing.id);
        existing.startLine = pp.startLine;
        existing.endLine = pp.endLine;
        if (existing.currentText !== pp.text) {
          existing.currentText = pp.text;
          existing.history.push({ text: pp.text, timestamp: Date.now() });
          if (existing.history.length > 5) existing.history.shift();
        }
        ei = k + 1;
        matched = true;
        break;
      }
    }
    if (!matched) {
      // New paragraph
      const id = hashString(pp.text) + "-" + pp.startLine;
      const entry: TrackedParagraph = {
        id,
        currentText: pp.text,
        history: [{ text: pp.text, timestamp: Date.now() }],
        startLine: pp.startLine,
        endLine: pp.endLine,
      };
      paragraphMap.set(id, entry);
      usedIds.add(id);
    }
  }

  // Remove deleted paragraphs
  for (const [id] of paragraphMap) {
    if (!usedIds.has(id)) paragraphMap.delete(id);
  }
}

async function requestSuggestion(paragraphId: string) {
  const para = paragraphMap.get(paragraphId);
  if (!para || para.history.length < 1 || !postSuggestion) return;
  if (suggestionInFlight) return;
  // Skip if we already gave feedback on this paragraph's current text
  if (paragraphId === lastSuggestedParaId && para.currentText === lastSuggestedVersion) return;

  suggestionInFlight = true;
  lastSuggestionTime = Date.now();
  if (showFeedbackIndicator) showFeedbackIndicator();

  const historyText = para.history
    .map((v, i) => `Version ${i + 1}: ${v.text}`)
    .join("\n\n");

  const messages = [
    {
      role: "system" as const,
      content:
        "You are a writing assistant providing brief paragraph-level feedback. You receive the editing history of a paragraph (oldest to newest). The LAST version is the writer's current text — do NOT suggest changes the writer has already made. Consider the editing direction and give 1-2 sentences of specific, actionable feedback about the current version only. If the paragraph contains factual claims (dates, statistics, names, events, scientific statements), use your web_search and web_fetch tools to verify them. Flag any inaccuracies with a brief correction and source. Also watch for bias: if the writing uses loaded language, presents only one side of a debate, omits key counterarguments, or makes sweeping generalizations, briefly point it out and suggest how to make the argument more balanced. When you have a concrete improvement, use the edit_paragraph tool to propose the exact text change — the writer will see an 'Apply fix' button. The old_text is matched as an exact case-sensitive substring search in the editor, so it must appear verbatim in the current paragraph text. The new_text replaces the first match. Provide your brief explanation in the text response and the precise edit via the tool. IMPORTANT: If the paragraph is already well-written and you have no actionable feedback, respond with exactly \"NOP\" and nothing else. Only speak when you have something worth changing. Never use em dashes in your writing or suggestions.",
    },
    {
      role: "user" as const,
      content: `[Full document]\n${getEditorValue()}\n[/Full document]\n\nParagraph history (${para.history.length} version${para.history.length > 1 ? "s" : ""}):\n\n${historyText}`,
    },
  ];

  try {
    const res = await fetch("/api/chat", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ messages }),
    });
    if (!res.ok) { suggestionInFlight = false; if (hideFeedbackIndicator) hideFeedbackIndicator(); return; }
    const data = await res.json();
    const reply =
      data?.message?.content || data?.choices?.[0]?.message?.content || "";
    // Extract edit_paragraph tool calls if present
    const toolCalls = data?.message?.tool_calls as
      | { function: { name: string; arguments: { old_text: string; new_text: string } } }[]
      | undefined;
    const edits = (toolCalls || [])
      .filter((tc) => tc.function?.name === "edit_paragraph")
      .map((tc) => tc.function.arguments);

    if (reply && reply.trim() !== "NOP" && postSuggestion) {
      lastSuggestedParaId = paragraphId;
      lastSuggestedVersion = para.currentText;
      const previewWords = para.currentText.split(/\s+/).slice(0, 6).join(" ");
      const ref = `<span class="suggestion-ref">Re: "${previewWords}..."</span>`;
      postSuggestion(ref + reply, edits.length > 0 ? edits : undefined);
    } else if (edits.length > 0 && postSuggestion) {
      // AI returned only a tool call with no text content
      lastSuggestedParaId = paragraphId;
      lastSuggestedVersion = para.currentText;
      const previewWords = para.currentText.split(/\s+/).slice(0, 6).join(" ");
      const ref = `<span class="suggestion-ref">Re: "${previewWords}..."</span>`;
      postSuggestion(ref + "Suggested edit:", edits);
    }
  } catch {
    // silently ignore suggestion errors
  }
  if (hideFeedbackIndicator) hideFeedbackIndicator();
  suggestionInFlight = false;
}

function initMonaco() {
  require.config({
    paths: {
      vs: "https://cdnjs.cloudflare.com/ajax/libs/monaco-editor/0.52.2/min/vs",
    },
  });

  require(["vs/editor/editor.main"], () => {
    const monaco = ((window as unknown) as Record<string, unknown>).monaco as {
      editor: {
        create: (
          el: HTMLElement,
          opts: Record<string, unknown>
        ) => {
          getModel: () => {
            onDidChangeContent: (cb: () => void) => void;
            getValue: () => string;
            getLineCount: () => number;
            getLineContent: (lineNumber: number) => string;
            findMatches: (searchString: string, searchOnlyEditableRange: boolean, isRegex: boolean, matchCase: boolean, wordSeparators: string | null, captureMatches: boolean) => { range: { startLineNumber: number; startColumn: number; endLineNumber: number; endColumn: number } }[];
          };
          getPosition: () => { lineNumber: number; column: number } | null;
          onDidChangeCursorPosition: (cb: (e: { position: { lineNumber: number; column: number } }) => void) => void;
          executeEdits: (source: string, edits: { range: { startLineNumber: number; startColumn: number; endLineNumber: number; endColumn: number }; text: string }[]) => void;
          layout: () => void;
        };
        defineTheme: (name: string, data: Record<string, unknown>) => void;
      };
    };

    monaco.editor.defineTheme("nexus", {
      base: "vs-dark",
      inherit: true,
      rules: [
        { token: "keyword", foreground: "c084fc", fontStyle: "bold" },
        { token: "comment", foreground: "6b7280", fontStyle: "italic" },
        { token: "string", foreground: "34d399" },
        { token: "number", foreground: "f59e0b" },
        { token: "delimiter", foreground: "94a3b8" },
        { token: "tag", foreground: "f472b6" },
        { token: "attribute.name", foreground: "fb923c" },
        { token: "attribute.value", foreground: "34d399" },
        { token: "type", foreground: "38bdf8" },
        { token: "variable", foreground: "e5e7eb" },
        { token: "operator", foreground: "f472b6" },
        { token: "string.link", foreground: "38bdf8" },
        { token: "string.escape", foreground: "fbbf24" },
        { token: "keyword.markdown", foreground: "c084fc", fontStyle: "bold" },
        { token: "string.bold", foreground: "fb923c", fontStyle: "bold" },
        { token: "string.italic", foreground: "a78bfa", fontStyle: "italic" },
        { token: "variable.source", foreground: "67e8f9" },
      ],
      colors: {
        "editor.background": "#1a1a1a",
        "editor.foreground": "#d4d4d4",
        "editor.lineHighlightBackground": "#2a2a2a",
        "editorLineNumber.foreground": "#555555",
        "editorLineNumber.activeForeground": "#a0a0a0",
        "editor.selectionBackground": "#ffffff20",
        "editorCursor.foreground": "#d4d4d4",
        "editorIndentGuide.background": "#333333",
      },
    });

    const editor = monaco.editor.create(
      document.getElementById("editor-container")!,
      {
        value: getDefaultContent(),
        language: "markdown",
        theme: "nexus",
        fontFamily: "'Plus Jakarta Sans', sans-serif",
        fontSize: 15,
        lineHeight: 28,
        wordWrap: "on",
        minimap: { enabled: false },
        lineNumbers: "on",
        renderLineHighlight: "line",
        scrollBeyondLastLine: false,
        padding: { top: 20, bottom: 20 },
        overviewRulerLanes: 0,
        hideCursorInOverviewRuler: true,
        scrollbar: {
          verticalScrollbarSize: 6,
          horizontalScrollbarSize: 6,
        },
        contextmenu: true,
        automaticLayout: false,
        tabSize: 2,
      }
    );

    const model = editor.getModel();
    const wordCountEl = document.getElementById("word-count")!;

    function updateWordCount() {
      const text = model.getValue();
      const words = text
        .trim()
        .split(/\s+/)
        .filter((w: string) => w.length > 0).length;
      wordCountEl.textContent = `${words} word${words !== 1 ? "s" : ""}`;
    }

    model.onDidChangeContent(() => {
      updateWordCount();

      // Debounce paragraph reconciliation to avoid running expensive
      // line-by-line parsing + change-ratio computations on every keystroke
      if (reconcileTimer) clearTimeout(reconcileTimer);
      reconcileTimer = setTimeout(() => {
        reconcileParagraphs(model);

        // Check new-paragraph trigger: cursor on blank line, paragraph above is non-empty
        const pos = editor.getPosition();
        if (pos) {
          const curLine = model.getLineContent(pos.lineNumber);
          if (curLine.trim() === "" && pos.lineNumber > 1) {
            const aboveLine = model.getLineContent(pos.lineNumber - 1);
            if (aboveLine.trim() !== "") {
              const paraId = getParagraphAtLine(pos.lineNumber - 1);
              if (paraId && canSuggest()) {
                if (debounceTimer) clearTimeout(debounceTimer);
                debounceTimer = setTimeout(() => {
                  requestSuggestion(paraId);
                }, 2000);
              }
            }
          }


        }
      }, 300);
    });
    updateWordCount();

    // Initialize paragraph tracking
    reconcileParagraphs(model);
    const initPos = editor.getPosition();
    if (initPos) {
      currentParagraphId = getParagraphAtLine(initPos.lineNumber);
      const initPara = currentParagraphId ? paragraphMap.get(currentParagraphId) : null;
      anchorText = initPara ? initPara.currentText : null;
    }

    // Cursor-move trigger
    editor.onDidChangeCursorPosition((e: { position: { lineNumber: number; column: number } }) => {
      const newParaId = getParagraphAtLine(e.position.lineNumber);
      if (newParaId !== currentParagraphId && currentParagraphId) {
        const prevPara = paragraphMap.get(currentParagraphId);
        if (prevPara && anchorText !== null && !suggestionInFlight) {
          const ratio = computeChangeRatio(anchorText, prevPara.currentText);
          if (ratio > 0.05) {
            requestSuggestion(currentParagraphId);
          }
        }
      }
      if (newParaId !== currentParagraphId) {
        const newPara = newParaId ? paragraphMap.get(newParaId) : null;
        anchorText = newPara ? newPara.currentText : null;
      }
      currentParagraphId = newParaId;
    });

    window.addEventListener("resize", () => editor.layout());

    let lastObservedWidth = 0;
    let lastObservedHeight = 0;
    const resizeObserver = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (!entry) return;
      const { width, height } = entry.contentRect;
      if (Math.abs(width - lastObservedWidth) > 1 || Math.abs(height - lastObservedHeight) > 1) {
        lastObservedWidth = width;
        lastObservedHeight = height;
        editor.layout();
      }
    });
    resizeObserver.observe(document.getElementById("editor-container")!);

    getEditorValue = () => model.getValue();
    applyEditorEdit = (oldText: string, newText: string): boolean => {
      const matches = model.findMatches(oldText, false, false, true, null, true);
      if (matches.length === 0) return false;
      editor.executeEdits("ai-fix", [{ range: matches[0].range, text: newText }]);
      return true;
    };
  });
}

let getEditorValue: () => string = () => "";
let applyEditorEdit: ((oldText: string, newText: string) => boolean) | null = null;

function appendEditButtons(
  bubble: HTMLElement,
  edits: { old_text: string; new_text: string }[]
) {
  for (const edit of edits) {
    const container = document.createElement("div");
    container.className = "apply-fix-container";

    // Preview of old → new
    const preview = document.createElement("div");
    preview.className = "apply-fix-preview";
    preview.innerHTML =
      `<span class="fix-old">${escapeHtml(edit.old_text)}</span>` +
      `<span class="fix-arrow">\u2192</span>` +
      `<span class="fix-new">${escapeHtml(edit.new_text)}</span>`;

    const btn = document.createElement("button");
    btn.className = "apply-fix-btn";
    btn.textContent = "Apply fix";
    btn.addEventListener("click", () => {
      if (applyEditorEdit && applyEditorEdit(edit.old_text, edit.new_text)) {
        btn.textContent = "Applied";
        btn.disabled = true;
        btn.classList.add("applied");
      } else {
        btn.textContent = "Text not found";
        btn.classList.add("failed");
        setTimeout(() => {
          btn.textContent = "Apply fix";
          btn.classList.remove("failed");
        }, 2000);
      }
    });

    container.appendChild(preview);
    container.appendChild(btn);
    bubble.appendChild(container);
  }
}

function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function getDefaultContent(): string {
  return `# Welcome to Blogger

Start writing here. This editor supports **Markdown** with full syntax highlighting.

## Features

- Rich markdown editing powered by Monaco
- Reference pane on the left for browsing sources
- Resizable panels — drag the dividers

---

> "The first draft is just you telling yourself the story."
> — Terry Pratchett

Happy writing.
`;
}

function initPaneToggles() {
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

function initDividerDrag(
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

function initUrlBar() {
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

interface ChatMessage {
  role: "user" | "assistant" | "system";
  content: string;
}

function initAssistant() {
  const messagesEl = document.getElementById("assistant-messages")!;
  const input = document.getElementById("assistant-input") as HTMLTextAreaElement;
  const sendBtn = document.getElementById("assistant-send") as HTMLButtonElement;
  const includeCtx = document.getElementById("include-context") as HTMLInputElement;

  const history: ChatMessage[] = [
    {
      role: "system",
      content:
        "You are a helpful writing assistant embedded in a writing tool. Help the user improve, edit, brainstorm, and refine their writing. Be concise and direct. When the user's editor content is provided, reference it as needed. You have access to web_search and web_fetch tools — use them to research topics, verify facts, or find sources when it would improve your response. You also have an edit_paragraph tool — use it to propose concrete edits to the user's text. The old_text is matched as an exact case-sensitive substring search in the editor, so it must appear verbatim in the current text. The new_text replaces the first match. The user will see an 'Apply fix' button. Never use em dashes in your writing or suggestions.",
    },
  ];

  function addMessageEl(role: "user" | "ai", content: string, cssClass?: string) {
    const wrapper = document.createElement("div");
    wrapper.className = `assistant-msg ${role}`;

    const label = document.createElement("span");
    label.className = "msg-label";
    label.textContent = role === "user" ? "You" : "Qwen 3.5";

    const bubble = document.createElement("div");
    bubble.className = `msg-bubble${cssClass ? ` ${cssClass}` : ""}`;
    if (role === "ai" && (!cssClass || cssClass === "suggestion")) {
      bubble.classList.add("markdown-body");
      bubble.innerHTML = marked.parse(content);
    } else {
      bubble.textContent = content;
    }

    wrapper.appendChild(label);
    wrapper.appendChild(bubble);
    messagesEl.appendChild(wrapper);

    requestAnimationFrame(() => {
      wrapper.classList.add("visible");
      messagesEl.scrollTop = messagesEl.scrollHeight;
    });

    return bubble;
  }

  function addTypingIndicator(): HTMLElement {
    const wrapper = document.createElement("div");
    wrapper.className = "assistant-msg ai";
    wrapper.id = "typing-indicator";

    const label = document.createElement("span");
    label.className = "msg-label";
    label.textContent = "Qwen 3.5";

    const bubble = document.createElement("div");
    bubble.className = "msg-bubble thinking";
    bubble.innerHTML =
      '<div class="typing-dots"><span></span><span></span><span></span></div>';

    wrapper.appendChild(label);
    wrapper.appendChild(bubble);
    messagesEl.appendChild(wrapper);

    requestAnimationFrame(() => {
      wrapper.classList.add("visible");
      messagesEl.scrollTop = messagesEl.scrollHeight;
    });

    return wrapper;
  }

  function removeTypingIndicator() {
    const el = document.getElementById("typing-indicator");
    if (el) el.remove();
  }

  async function send() {
    const text = input.value.trim();
    if (!text) return;

    input.value = "";
    input.style.height = "auto";
    sendBtn.disabled = true;

    addMessageEl("user", text);

    let userContent = text;
    if (includeCtx.checked) {
      const editorText = getEditorValue();
      if (editorText.trim()) {
        userContent = `[Editor content]\n${editorText}\n[/Editor content]\n\n${text}`;
      }
    }

    history.push({ role: "user", content: userContent });

    const indicator = addTypingIndicator();

    try {
      const res = await fetch("/api/chat", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ messages: history }),
      });

      removeTypingIndicator();

      if (!res.ok) {
        const err = await res.json().catch(() => ({ error: "Unknown error" }));
        addMessageEl("ai", err.error || `Error ${res.status}`, "error");
        sendBtn.disabled = false;
        return;
      }

      const data = await res.json();
      const reply =
        data?.message?.content || data?.choices?.[0]?.message?.content || "";

      // Extract edit_paragraph tool calls
      const toolCalls = data?.message?.tool_calls as
        | { function: { name: string; arguments: { old_text: string; new_text: string } } }[]
        | undefined;
      const edits = (toolCalls || [])
        .filter((tc) => tc.function?.name === "edit_paragraph")
        .map((tc) => tc.function.arguments);

      history.push({ role: "assistant", content: reply || "Suggested edit:" });
      const bubble = addMessageEl("ai", reply || "Suggested edit:");

      if (edits.length > 0 && bubble) {
        appendEditButtons(bubble, edits);
      }
    } catch (e) {
      removeTypingIndicator();
      addMessageEl("ai", `Network error: ${e}`, "error");
    }

    sendBtn.disabled = false;
  }

  sendBtn.addEventListener("click", send);

  input.addEventListener("keydown", (e: KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  });

  input.addEventListener("input", () => {
    input.style.height = "auto";
    input.style.height = Math.min(input.scrollHeight, 120) + "px";
  });

  // Expose postSuggestion at module scope for auto-suggestions
  postSuggestion = (content: string, edits?: { old_text: string; new_text: string }[]) => {
    const bubble = addMessageEl("ai", content, "suggestion");
    if (edits && edits.length > 0 && bubble) {
      appendEditButtons(bubble, edits);
    }
  };

  showFeedbackIndicator = () => {
    if (document.getElementById("feedback-indicator")) return;
    const wrapper = document.createElement("div");
    wrapper.className = "assistant-msg ai";
    wrapper.id = "feedback-indicator";

    const label = document.createElement("span");
    label.className = "msg-label";
    label.textContent = "Qwen 3.5";

    const bubble = document.createElement("div");
    bubble.className = "msg-bubble feedback-queued";
    bubble.innerHTML = '<span class="feedback-pulse"></span> Analyzing paragraph\u2026';

    wrapper.appendChild(label);
    wrapper.appendChild(bubble);
    messagesEl.appendChild(wrapper);

    requestAnimationFrame(() => {
      wrapper.classList.add("visible");
      messagesEl.scrollTop = messagesEl.scrollHeight;
    });
  };

  hideFeedbackIndicator = () => {
    const el = document.getElementById("feedback-indicator");
    if (el) el.remove();
  };
}

// Expose debug state for testing
(window as unknown as Record<string, unknown>).__debug = {
  get paragraphMap() { return paragraphMap; },
  get currentParagraphId() { return currentParagraphId; },
  get anchorText() { return anchorText; },
  get suggestionInFlight() { return suggestionInFlight; },
  get lastSuggestionTime() { return lastSuggestionTime; },
  get canSuggest() { return !suggestionInFlight && Date.now() - lastSuggestionTime >= 30_000; },
};

initMonaco();
initPaneToggles();
initDividerDrag("divider-left", "pane-left", "pane-center");
initDividerDrag("divider-right", "pane-center", "pane-right");
initUrlBar();
initAssistant();
