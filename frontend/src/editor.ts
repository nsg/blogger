declare const require: {
  config: (options: Record<string, unknown>) => void;
  (deps: string[], callback: (...args: unknown[]) => void): void;
};

import { IModelDeltaDecoration } from "./types.js";
import * as S from "./state.js";
import {
  reconcileParagraphs,
  getParagraphAtLine,
  computeChangeRatio,
  canSuggest,
  requestSuggestion,
} from "./paragraphs.js";

type AppTheme = "dark" | "light";

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

const IMAGE_LINE_RE = /^!\[([^\]]*)\]\(([^)]+)\)\s*$/;

function parseImageLine(line: string): { alt: string; path: string } | null {
  const m = line.match(IMAGE_LINE_RE);
  return m ? { alt: m[1], path: m[2] } : null;
}

function showImageEditDialog(
  imagePath: string,
  onRename: (newName: string) => void,
  onDelete: () => void,
) {
  const currentName = imagePath.split("/").pop() || "";

  const overlay = document.createElement("div");
  overlay.className = "image-dialog-overlay";

  const dialog = document.createElement("div");
  dialog.className = "image-dialog";

  const label = document.createElement("label");
  label.className = "image-dialog-label";
  label.textContent = "Image filename";

  const input = document.createElement("input");
  input.type = "text";
  input.className = "image-dialog-input";
  input.value = currentName;

  const buttons = document.createElement("div");
  buttons.className = "image-dialog-buttons";

  const deleteBtn = document.createElement("button");
  deleteBtn.className = "image-dialog-btn delete";
  deleteBtn.textContent = "Delete";

  const cancelBtn = document.createElement("button");
  cancelBtn.className = "image-dialog-btn cancel";
  cancelBtn.textContent = "Cancel";

  const renameBtn = document.createElement("button");
  renameBtn.className = "image-dialog-btn confirm";
  renameBtn.textContent = "Rename";

  buttons.appendChild(deleteBtn);
  buttons.appendChild(cancelBtn);
  buttons.appendChild(renameBtn);
  dialog.appendChild(label);
  dialog.appendChild(input);
  dialog.appendChild(buttons);
  overlay.appendChild(dialog);
  document.body.appendChild(overlay);

  requestAnimationFrame(() => {
    overlay.classList.add("visible");
    input.focus();
    const dotIdx = input.value.lastIndexOf(".");
    input.setSelectionRange(0, dotIdx > 0 ? dotIdx : input.value.length);
  });

  function close() {
    overlay.classList.remove("visible");
    setTimeout(() => overlay.remove(), 150);
  }

  renameBtn.addEventListener("click", () => {
    const val = input.value.trim();
    if (val && val !== currentName) {
      close();
      onRename(val);
    } else {
      close();
    }
  });

  cancelBtn.addEventListener("click", close);

  deleteBtn.addEventListener("click", () => {
    close();
    onDelete();
  });

  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay) close();
  });

  input.addEventListener("keydown", (e: KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      const val = input.value.trim();
      if (val && val !== currentName) {
        close();
        onRename(val);
      } else {
        close();
      }
    } else if (e.key === "Escape") {
      close();
    }
  });
}

function getActiveTheme(): AppTheme {
  return document.documentElement.dataset.theme === "light" ? "light" : "dark";
}

export function initMonaco() {
  require.config({
    paths: {
      vs: "https://cdnjs.cloudflare.com/ajax/libs/monaco-editor/0.52.2/min/vs",
    },
  });

  require(["vs/editor/editor.main"], async () => {
    const monaco = ((window as unknown) as Record<string, unknown>).monaco as {
      editor: {
        create: (
          el: HTMLElement,
          opts: Record<string, unknown>
        ) => {
          getModel: () => {
            onDidChangeContent: (cb: () => void) => void;
            getValue: () => string;
            setValue: (value: string) => void;
            getLineCount: () => number;
            getLineContent: (lineNumber: number) => string;
            findMatches: (searchString: string, searchOnlyEditableRange: boolean, isRegex: boolean, matchCase: boolean, wordSeparators: string | null, captureMatches: boolean) => { range: { startLineNumber: number; startColumn: number; endLineNumber: number; endColumn: number } }[];
          };
          getPosition: () => { lineNumber: number; column: number } | null;
          onDidChangeCursorPosition: (cb: (e: { position: { lineNumber: number; column: number } }) => void) => void;
          onMouseDown: (cb: (e: { target: { type: number; position: { lineNumber: number; column: number } | null } }) => void) => void;
          deltaDecorations: (oldDecorations: string[], newDecorations: IModelDeltaDecoration[]) => string[];
          executeEdits: (source: string, edits: { range: { startLineNumber: number; startColumn: number; endLineNumber: number; endColumn: number }; text: string }[]) => void;
          focus: () => void;
          layout: () => void;
        };
        defineTheme: (name: string, data: Record<string, unknown>) => void;
        setTheme: (themeName: string) => void;
        MouseTargetType: Record<string, number>;
      };
      languages: {
        setMonarchTokensProvider: (languageId: string, provider: Record<string, unknown>) => void;
      };
      Range: new (startLine: number, startCol: number, endLine: number, endCol: number) => { startLineNumber: number; startColumn: number; endLineNumber: number; endColumn: number };
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
        { token: "string.link", foreground: "7b8a9e" },
        { token: "string.escape", foreground: "fbbf24" },
        { token: "keyword.markdown", foreground: "c084fc", fontStyle: "bold" },
        { token: "string.bold", foreground: "fb923c", fontStyle: "bold" },
        { token: "string.italic", foreground: "a78bfa", fontStyle: "italic" },
        { token: "variable.source", foreground: "67e8f9" },
        { token: "frontmatter.delimiter", foreground: "6b7280" },
        { token: "frontmatter.key", foreground: "fb923c" },
        { token: "frontmatter.section", foreground: "38bdf8" },
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
        "editorBracketHighlight.foreground1": "#d4d4d4",
        "editorBracketHighlight.foreground2": "#d4d4d4",
        "editorBracketHighlight.foreground3": "#d4d4d4",
        "editorBracketHighlight.foreground4": "#d4d4d4",
        "editorBracketHighlight.foreground5": "#d4d4d4",
        "editorBracketHighlight.foreground6": "#d4d4d4",
      },
    });

    monaco.editor.defineTheme("nexus-light", {
      base: "vs",
      inherit: true,
      rules: [
        { token: "keyword", foreground: "4f46e5", fontStyle: "bold" },
        { token: "comment", foreground: "8b8172", fontStyle: "italic" },
        { token: "string", foreground: "047857" },
        { token: "number", foreground: "b45309" },
        { token: "delimiter", foreground: "7c7366" },
        { token: "tag", foreground: "be185d" },
        { token: "attribute.name", foreground: "c2410c" },
        { token: "attribute.value", foreground: "047857" },
        { token: "type", foreground: "0369a1" },
        { token: "variable", foreground: "23201c" },
        { token: "operator", foreground: "be185d" },
        { token: "string.link", foreground: "6b6258" },
        { token: "string.escape", foreground: "b45309" },
        { token: "keyword.markdown", foreground: "4f46e5", fontStyle: "bold" },
        { token: "string.bold", foreground: "c2410c", fontStyle: "bold" },
        { token: "string.italic", foreground: "7c3aed", fontStyle: "italic" },
        { token: "variable.source", foreground: "0891b2" },
        { token: "frontmatter.delimiter", foreground: "8b8172" },
        { token: "frontmatter.key", foreground: "c2410c" },
        { token: "frontmatter.section", foreground: "0369a1" },
      ],
      colors: {
        "editor.background": "#fffdf8",
        "editor.foreground": "#23201c",
        "editor.lineHighlightBackground": "#f7f3ea",
        "editorLineNumber.foreground": "#a99f90",
        "editorLineNumber.activeForeground": "#5d574e",
        "editor.selectionBackground": "#4f46e526",
        "editorCursor.foreground": "#23201c",
        "editorIndentGuide.background": "#ddd6c7",
        "editorBracketHighlight.foreground1": "#23201c",
        "editorBracketHighlight.foreground2": "#23201c",
        "editorBracketHighlight.foreground3": "#23201c",
        "editorBracketHighlight.foreground4": "#23201c",
        "editorBracketHighlight.foreground5": "#23201c",
        "editorBracketHighlight.foreground6": "#23201c",
      },
    });

    monaco.languages.setMonarchTokensProvider("markdown", {
      defaultToken: "",
      tokenPostfix: ".md",

      tokenizer: {
        root: [
          [/^\+\+\+\s*$/, { token: "frontmatter.delimiter", next: "@frontmatter" }],
          [/^\s*```\s*([\w+#-]*)\s*$/, { token: "string", next: "@codeblock" }],
          [/^#{1,6}\s.*$/, "keyword.markdown"],
          [/^\s*(---+|===+|\*\*\*+)\s*$/, "keyword.markdown"],
          [/^\s*>+/, "comment"],
          [/^\s*([\*\-+])\s/, "keyword.markdown"],
          [/^\s*\d+\.\s/, "keyword.markdown"],
          [/!?\[(?:[^\[\]]|\[[^\]]*\])*\]\(/, { token: "string.link", next: "@linkUrl" }],
          [/`[^`]+`/, "string"],
          [/\*\*([^*]|\*(?!\*))+\*\*/, "string.bold"],
          [/__([^_]|_(?!_))+__/, "string.bold"],
          [/\*[^*]+\*/, "string.italic"],
          [/_[^_]+_/, "string.italic"],
          [/https?:\/\/[^\s>\]]+/, "string.link"],
          [/<\/?\w+[^>]*>/, "tag"],
        ],
        frontmatter: [
          [/^\+\+\+\s*$/, { token: "frontmatter.delimiter", next: "@pop" }],
          [/^\s*\[[^\]]*\]/, "frontmatter.section"],
          [/^[\w.-]+/, "frontmatter.key"],
          [/=/, "operator"],
          [/"[^"]*"/, "string"],
          [/'[^']*'/, "string"],
          [/\d{4}-\d{2}-\d{2}[T\d:+.-]*/, "number"],
          [/\b(true|false)\b/, "keyword"],
          [/\d+/, "number"],
          [/\[/, "delimiter"],
          [/\]/, "delimiter"],
          [/,/, "delimiter"],
        ],
        linkUrl: [
          [/[^)]+/, "string.link"],
          [/\)/, { token: "string.link", next: "@pop" }],
        ],
        codeblock: [
          [/^\s*```\s*$/, { token: "string", next: "@pop" }],
          [/.*$/, "variable.source"],
        ],
      },
    });

    let initialContent = getDefaultContent();
    let hasFile = false;
    let canSync = false;
    let documentRevision = 0;
    try {
      const res = await fetch("/api/initial-content");
      if (res.ok) {
        const data = await res.json();
        if (data.path !== null && typeof data.content === "string") {
          initialContent = data.content;
          hasFile = true;
        }
        if (typeof data.revision === "number") {
          documentRevision = data.revision;
          canSync = true;
        }
      }
    } catch {
      // use default content
    }

    let saveTimer: ReturnType<typeof setTimeout> | null = null;
    let saving = false;
    let localDirty = false;
    let applyingRemoteContent = false;
    let lastSavedContent = initialContent;

    async function autoSave(content: string) {
      if (saving) return;
      saving = true;
      try {
        const res = await fetch("/api/save", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ content, revision: documentRevision }),
        });
        if (res.ok) {
          const data = await res.json();
          if (typeof data.revision === "number") {
            documentRevision = data.revision;
          }
          lastSavedContent = content;
          localDirty = model.getValue() !== content;
        }
      } catch {
        // silently ignore save errors
      }
      saving = false;

      if (canSync && localDirty && model.getValue() !== lastSavedContent) {
        if (saveTimer) clearTimeout(saveTimer);
        saveTimer = setTimeout(() => autoSave(model.getValue()), 1_000);
      }
    }

    const editor = monaco.editor.create(
      document.getElementById("editor-container")!,
      {
        value: initialContent,
        language: "markdown",
        theme: getActiveTheme() === "light" ? "nexus-light" : "nexus",
        fontFamily: "'Plus Jakarta Sans', sans-serif",
        fontSize: 15,
        lineHeight: 28,
        wordWrap: "on",
        minimap: { enabled: false },
        glyphMargin: true,
        glyphMarginWidth: 16,
        folding: true,
        showFoldingControls: "always",
        lineNumbers: "on",
        lineNumbersMinChars: 2,
        lineDecorationsWidth: 0,
        renderLineHighlight: "all",
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
    const voiceBtn = document.getElementById("voice-record") as HTMLButtonElement | null;
    const voiceStatus = document.getElementById("voice-status");

    window.addEventListener("blogger-theme-change", (event: Event) => {
      const theme = (event as CustomEvent<{ theme: AppTheme }>).detail.theme;
      monaco.editor.setTheme(theme === "light" ? "nexus-light" : "nexus");
    });

    function setVoiceStatus(message: string) {
      if (voiceStatus) voiceStatus.textContent = message;
    }

    function chooseRecordingMimeType(): string {
      const candidates = [
        "audio/webm;codecs=opus",
        "audio/webm",
        "audio/mp4",
        "audio/mpeg",
      ];
      return candidates.find((type) => MediaRecorder.isTypeSupported(type)) || "";
    }

    function recordingExtension(mimeType: string): string {
      if (mimeType.includes("mp4")) return "m4a";
      if (mimeType.includes("mpeg")) return "mp3";
      return "webm";
    }

    function insertTranscript(text: string) {
      const trimmed = text.trim();
      if (!trimmed) return;

      const pos = editor.getPosition();
      if (!pos) return;

      const line = model.getLineContent(pos.lineNumber);
      const before = line.slice(0, Math.max(0, pos.column - 1));
      const after = line.slice(Math.max(0, pos.column - 1));
      const prefix = before && !/\s$/.test(before) && !/^[.,!?;:)]/.test(trimmed) ? " " : "";
      const suffix = after && !/^\s/.test(after) && !/[([{]$/.test(trimmed) ? " " : "";
      const range = new monaco.Range(pos.lineNumber, pos.column, pos.lineNumber, pos.column);

      editor.executeEdits("voice-dictation", [{ range, text: `${prefix}${trimmed}${suffix}` }]);
      editor.focus();
    }

    function initVoiceInput() {
      if (!voiceBtn) return;
      const button = voiceBtn;
      if (!navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === "undefined") {
        button.disabled = true;
        button.title = "Dictation is not supported in this browser";
        button.setAttribute("aria-label", "Dictation is not supported in this browser");
        return;
      }

      let recorder: MediaRecorder | null = null;
      let stream: MediaStream | null = null;
      let chunks: Blob[] = [];

      async function transcribe(blob: Blob, mimeType: string) {
        button.disabled = true;
        button.classList.remove("recording");
        button.classList.add("transcribing");
        setVoiceStatus("Transcribing...");

        try {
          const form = new FormData();
          form.append("audio", blob, `dictation.${recordingExtension(mimeType)}`);
          const res = await fetch("/api/transcribe", { method: "POST", body: form });
          const data = await res.json().catch(() => ({}));
          if (!res.ok) {
            throw new Error(data.error || "Transcription failed");
          }
          insertTranscript(typeof data.text === "string" ? data.text : "");
          setVoiceStatus("");
        } catch (err) {
          const message = err instanceof Error ? err.message : "Transcription failed";
          setVoiceStatus(message);
        } finally {
          button.disabled = false;
          button.classList.remove("transcribing");
          button.title = "Start dictation";
          button.setAttribute("aria-label", "Start dictation");
        }
      }

      function resetRecordingUi() {
        button.classList.remove("recording");
        button.querySelector(".voice-icon-mic")?.removeAttribute("hidden");
        button.querySelector(".voice-icon-stop")?.setAttribute("hidden", "");
      }

      function stopRecording() {
        if (recorder && recorder.state !== "inactive") {
          recorder.stop();
        }
      }

      async function startRecording() {
        chunks = [];
        const mimeType = chooseRecordingMimeType();
        stream = await navigator.mediaDevices.getUserMedia({ audio: true });
        recorder = new MediaRecorder(stream, mimeType ? { mimeType } : undefined);

        recorder.addEventListener("dataavailable", (event: BlobEvent) => {
          if (event.data.size > 0) chunks.push(event.data);
        });

        recorder.addEventListener("stop", () => {
          stream?.getTracks().forEach((track) => track.stop());
          stream = null;
          resetRecordingUi();

          const blob = new Blob(chunks, { type: mimeType || "audio/webm" });
          chunks = [];
          if (blob.size === 0) {
            setVoiceStatus("");
            return;
          }
          void transcribe(blob, mimeType || "audio/webm");
        });

        recorder.start();
        button.classList.add("recording");
        button.title = "Stop dictation";
        button.setAttribute("aria-label", "Stop dictation");
        button.querySelector(".voice-icon-mic")?.setAttribute("hidden", "");
        button.querySelector(".voice-icon-stop")?.removeAttribute("hidden");
        setVoiceStatus("Recording");
      }

      button.addEventListener("click", () => {
        if (recorder && recorder.state === "recording") {
          stopRecording();
          return;
        }

        startRecording().catch((err) => {
          stream?.getTracks().forEach((track) => track.stop());
          stream = null;
          recorder = null;
          resetRecordingUi();
          const message = err instanceof Error ? err.message : "Could not start dictation";
          setVoiceStatus(message);
        });
      });
    }

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

      if (canSync && !applyingRemoteContent) {
        localDirty = true;
        if (saveTimer) clearTimeout(saveTimer);
        saveTimer = setTimeout(() => autoSave(model.getValue()), 5_000);
      }

      if (S.reconcileTimer) clearTimeout(S.reconcileTimer);
      S.setReconcileTimer(setTimeout(() => {
        reconcileParagraphs(model);
        updateGutterIcons();

        const pos = editor.getPosition();
        if (pos) {
          const curLine = model.getLineContent(pos.lineNumber);
          if (curLine.trim() === "" && pos.lineNumber > 1) {
            const aboveLine = model.getLineContent(pos.lineNumber - 1);
            if (aboveLine.trim() !== "") {
              const paraId = getParagraphAtLine(pos.lineNumber - 1);
              if (paraId && canSuggest()) {
                if (S.debounceTimer) clearTimeout(S.debounceTimer);
                S.setDebounceTimer(setTimeout(() => {
                  requestSuggestion(paraId);
                }, 2000));
              }
            }
          }
        }
      }, 300));
    });
    updateWordCount();
    initVoiceInput();

    async function pollDocumentState() {
      if (!canSync || localDirty || saving) return;

      try {
        const res = await fetch("/api/document-state");
        if (!res.ok) return;
        const data = await res.json();
        if (typeof data.revision !== "number" || data.revision <= documentRevision) return;
        if (typeof data.content !== "string" || data.content === model.getValue()) {
          documentRevision = data.revision;
          return;
        }

        applyingRemoteContent = true;
        model.setValue(data.content);
        applyingRemoteContent = false;
        documentRevision = data.revision;
        lastSavedContent = data.content;
        localDirty = false;
      } catch {
        // keep the local editor usable if polling fails
      } finally {
        applyingRemoteContent = false;
      }
    }

    setInterval(pollDocumentState, 2_000);

    reconcileParagraphs(model);
    const initPos = editor.getPosition();
    if (initPos) {
      S.setCurrentParagraphId(getParagraphAtLine(initPos.lineNumber));
      const initPara = S.currentParagraphId ? S.paragraphMap.get(S.currentParagraphId) : null;
      S.setAnchorText(initPara ? initPara.currentText : null);
    }

    let gutterDecorations: string[] = [];
    function updateGutterIcons() {
      const decorations: IModelDeltaDecoration[] = [];
      for (const [id, para] of S.paragraphMap) {
        if (para.startLine === para.endLine && parseImageLine(model.getLineContent(para.startLine))) continue;
        const isProcessing = id === S.processingParagraphId;
        const isNop = S.nopParagraphs.has(id);
        let className = "paragraph-action-icon";
        if (isProcessing) className += " paragraph-action-processing";
        else if (isNop) className += " paragraph-action-nop";
        let hoverMsg = "Get AI feedback on this paragraph";
        if (isProcessing) hoverMsg = "Analyzing paragraph...";
        else if (isNop) hoverMsg = "AI: paragraph looks good";
        decorations.push({
          range: new monaco.Range(para.startLine, 1, para.startLine, 1),
          options: {
            glyphMarginClassName: className,
            glyphMarginHoverMessage: { value: hoverMsg },
          },
        });
      }
      const lineCount = model.getLineCount();
      for (let ln = 1; ln <= lineCount; ln++) {
        const line = model.getLineContent(ln);
        if (parseImageLine(line)) {
          decorations.push({
            range: new monaco.Range(ln, 1, ln, 1),
            options: {
              glyphMarginClassName: "image-action-icon",
              glyphMarginHoverMessage: { value: "Edit image" },
            },
          });
        }
      }
      gutterDecorations = editor.deltaDecorations(gutterDecorations, decorations);
    }
    updateGutterIcons();
    S.setOnProcessingChanged(() => updateGutterIcons());

    editor.onMouseDown((e: { target: { type: number; position: { lineNumber: number; column: number } | null } }) => {
      if (e.target.type === 2 && e.target.position) {
        const ln = e.target.position.lineNumber;
        const line = model.getLineContent(ln);
        const img = parseImageLine(line);
        if (img) {
          showImageEditDialog(
            img.path,
            (newName) => {
              fetch("/api/rename-image", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ old_path: img.path, new_name: newName }),
              })
                .then((r) => (r.ok ? r.json() : Promise.reject(r.statusText)))
                .then((data: { path: string }) => {
                  const newLine = `![${img.alt}](${data.path})`;
                  const range = new monaco.Range(ln, 1, ln, line.length + 1);
                  editor.executeEdits("rename-image", [{ range, text: newLine }]);
                })
                .catch(() => {});
            },
            () => {
              fetch("/api/delete-image", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ path: img.path }),
              })
                .then(() => {
                  const startLn = ln > 1 && model.getLineContent(ln - 1).trim() === "" ? ln - 1 : ln;
                  const endLn = ln < model.getLineCount() && model.getLineContent(ln + 1).trim() === "" ? ln + 1 : ln;
                  const range = new monaco.Range(startLn, 1, endLn, model.getLineContent(endLn).length + 1);
                  editor.executeEdits("delete-image", [{ range, text: "" }]);
                })
                .catch(() => {});
            },
          );
          return;
        }
        const paraId = getParagraphAtLine(ln);
        if (paraId) {
          requestSuggestion(paraId);
        }
      }
    });

    editor.onDidChangeCursorPosition((e: { position: { lineNumber: number; column: number } }) => {
      const newParaId = getParagraphAtLine(e.position.lineNumber);
      if (newParaId !== S.currentParagraphId && S.currentParagraphId) {
        const prevPara = S.paragraphMap.get(S.currentParagraphId);
        if (prevPara && S.anchorText !== null && !S.suggestionInFlight) {
          const ratio = computeChangeRatio(S.anchorText, prevPara.currentText);
          if (ratio > 0.05) {
            requestSuggestion(S.currentParagraphId);
          }
        }
      }
      if (newParaId !== S.currentParagraphId) {
        const newPara = newParaId ? S.paragraphMap.get(newParaId) : null;
        S.setAnchorText(newPara ? newPara.currentText : null);
      }
      S.setCurrentParagraphId(newParaId);
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

    const editorEl = document.getElementById("editor-container")!;
    document.addEventListener("paste", (e: Event) => {
      if (!editorEl.contains(document.activeElement)) return;

      const ce = e as ClipboardEvent;
      const items = ce.clipboardData?.items;
      if (!items) return;

      let imageItem: DataTransferItem | null = null;
      for (let i = 0; i < items.length; i++) {
        if (items[i].type.startsWith("image/")) {
          imageItem = items[i];
          break;
        }
      }
      if (!imageItem) return;

      e.preventDefault();
      e.stopPropagation();
      const file = imageItem.getAsFile();
      if (!file) return;

      const pos = editor.getPosition();
      if (!pos) return;

      const ext = file.type.split("/")[1]?.replace("jpeg", "jpg") || "png";
      const defaultName = file.name && file.name !== "image.png" ? file.name : `paste.${ext}`;

      const placeholder = "![uploading...]()";
      const range = new monaco.Range(pos.lineNumber, pos.column, pos.lineNumber, pos.column);
      editor.executeEdits("paste-image", [{ range, text: placeholder }]);

      const form = new FormData();
      form.append("image", file, defaultName);

      fetch("/api/upload-image", { method: "POST", body: form })
        .then((res) => (res.ok ? res.json() : Promise.reject(res.statusText)))
        .then((data: { path: string }) => {
          const mdLink = `![](${data.path})`;
          const matches = model.findMatches(placeholder, false, false, true, null, true);
          if (matches.length > 0) {
            editor.executeEdits("paste-image", [{ range: matches[0].range, text: mdLink }]);
          }
        })
        .catch(() => {
          const matches = model.findMatches(placeholder, false, false, true, null, true);
          if (matches.length > 0) {
            editor.executeEdits("paste-image", [{ range: matches[0].range, text: "![upload failed]()" }]);
          }
        });
    }, { capture: true });

    S.setGetEditorValue(() => model.getValue());
    S.setApplyEditorEdit((oldText: string, newText: string): boolean => {
      const matches = model.findMatches(oldText, false, false, true, null, true);
      if (matches.length === 0) return false;
      editor.executeEdits("ai-fix", [{ range: matches[0].range, text: newText }]);
      return true;
    });
  });
}
