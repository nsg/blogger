declare const require: {
  config: (options: Record<string, unknown>) => void;
  (deps: string[], callback: (...args: unknown[]) => void): void;
};

import { ApiError } from "./api.js";
import { api, jsonRequest, mutationEvent } from "./api.js";
import type { IModelDeltaDecoration, PostDocumentController, PostResponse, RecoverPostResponse, SaveResponse } from "./types.js";
import * as S from "./state.js";
import { field, modalButton, openModal, showModalError } from "./modal.js";
import {
  reconcileParagraphs,
  getParagraphAtLine,
  computeChangeRatio,
  canSuggest,
  requestSuggestion,
  requestVoiceCommand,
} from "./paragraphs.js";

type AppTheme = "dark" | "light";
type AudioContextWindow = Window & { webkitAudioContext?: typeof AudioContext };
type VoiceRecordingMode =
  | { kind: "dictation" }
  | { kind: "paragraph-command"; paragraphId: string };

const IMAGE_LINE_RE = /^!\[([^\]]*)\]\(([^)]+)\)\s*$/;
const FEEDBACK_ICON_SVG =
  '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z"/><path d="M20 3v4"/><path d="M22 5h-4"/></svg>';
const MIC_ICON_SVG =
  '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><path d="M12 19v3"/><path d="M8 22h8"/></svg>';

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
    document.removeEventListener("keydown", onDialogKeydown);
    overlay.classList.remove("visible");
    setTimeout(() => overlay.remove(), 150);
  }

  function onDialogKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") close();
  }
  document.addEventListener("keydown", onDialogKeydown);

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

export function initMonaco(): Promise<PostDocumentController> {
  return new Promise((resolve) => {
  require.config({
    paths: {
      vs: "https://cdnjs.cloudflare.com/ajax/libs/monaco-editor/0.52.2/min/vs",
    },
  });

  require(["vs/editor/editor.main"], async () => {
    const monaco = ((window as unknown) as Record<string, unknown>).monaco as any;
    /* Monaco is loaded at runtime from its AMD distribution, so the small API
       surface below is intentionally runtime-typed instead of depending on the
       editor package at compile time. */
    /*
    const monacoTyped = monaco as {
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
          onDidScrollChange: (cb: () => void) => void;
          onDidLayoutChange: (cb: () => void) => void;
          getScrollTop: () => number;
          getTopForLineNumber: (lineNumber: number) => number;
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
    */

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

    const editorContainer = document.getElementById("editor-container")!;
    const editor = monaco.editor.create(
      editorContainer,
      {
        model: null,
        theme: getActiveTheme() === "light" ? "nexus-light" : "nexus",
        fontFamily: "'Plus Jakarta Sans', sans-serif",
        fontSize: 15,
        lineHeight: 28,
        wordWrap: "on",
        minimap: { enabled: false },
        glyphMargin: true,
        glyphMarginWidth: 48,
        folding: true,
        showFoldingControls: "always",
        lineNumbers: "on",
        lineNumbersMinChars: 5,
        lineDecorationsWidth: 24,
        renderLineHighlight: "all",
        scrollBeyondLastLine: false,
        padding: { top: 20, bottom: 112 },
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

    let activeModel: any = null;
    const model = new Proxy({}, {
      get(_target, property) {
        const value = activeModel?.[property];
        return typeof value === "function" ? value.bind(activeModel) : value;
      },
    }) as any;
    const wordCountEl = document.getElementById("word-count")!;
    const emptyState = document.getElementById("editor-empty")!;
    const postTitleEl = document.getElementById("editor-post-title")!;
    const saveStateEl = document.getElementById("save-state")!;
    const saveRetry = document.getElementById("save-retry") as HTMLButtonElement;
    const banner = document.getElementById("editor-banner")!;
    const bannerTitle = document.getElementById("editor-banner-title")!;
    const bannerMessage = document.getElementById("editor-banner-message")!;
    const bannerActions = document.getElementById("editor-banner-actions")!;
    const includeContext = document.getElementById("include-context") as HTMLInputElement;
    const voiceBtn = document.getElementById("voice-record") as HTMLButtonElement | null;
    let voiceSupported = true;
    const voiceStatus = document.getElementById("voice-status");
    const voiceVisualizer = document.getElementById("voice-visualizer") as HTMLElement | null;
    const paragraphActionsLayer = document.createElement("div");
    paragraphActionsLayer.className = "paragraph-actions-layer";
    editorContainer.appendChild(paragraphActionsLayer);
    if (voiceVisualizer && voiceVisualizer.parentElement !== editorContainer) {
      editorContainer.appendChild(voiceVisualizer);
    }

    function syncVoiceViewportInset() {
      const viewport = window.visualViewport;
      const bottomInset = viewport
        ? Math.max(0, window.innerHeight - viewport.height - viewport.offsetTop)
        : 0;
      editorContainer.style.setProperty("--voice-bottom-inset", `${Math.ceil(bottomInset)}px`);
    }

    syncVoiceViewportInset();
    window.visualViewport?.addEventListener("resize", syncVoiceViewportInset);
    window.visualViewport?.addEventListener("scroll", syncVoiceViewportInset);
    window.addEventListener("resize", syncVoiceViewportInset);

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

    function applyModelEdit(targetModel: any, source: string, range: any, text: string) {
      if (!targetModel || targetModel.isDisposed?.()) return;
      if (targetModel === activeModel) {
        editor.executeEdits(source, [{ range, text }]);
      } else {
        targetModel.pushEditOperations([], [{ range, text }], () => null);
      }
    }

    function insertTranscript(text: string, targetModel: any, pos: { lineNumber: number; column: number } | null) {
      const trimmed = text.trim();
      if (!trimmed || !targetModel || !pos) return;

      const line = targetModel.getLineContent(pos.lineNumber);
      const before = line.slice(0, Math.max(0, pos.column - 1));
      const after = line.slice(Math.max(0, pos.column - 1));
      const prefix = before && !/\s$/.test(before) && !/^[.,!?;:)]/.test(trimmed) ? " " : "";
      const suffix = after && !/^\s/.test(after) && !/[([{]$/.test(trimmed) ? " " : "";
      const range = new monaco.Range(pos.lineNumber, pos.column, pos.lineNumber, pos.column);

      applyModelEdit(targetModel, "voice-dictation", range, `${prefix}${trimmed}${suffix}`);
      if (targetModel === activeModel) editor.focus();
    }

    let voiceCommandParagraphId: string | null = null;
    let startParagraphVoiceCommand: (paragraphId: string) => void = () => {};

    function initVoiceInput() {
      if (!voiceBtn) return;
      const button = voiceBtn;
      if (!navigator.mediaDevices?.getUserMedia || typeof MediaRecorder === "undefined") {
        voiceSupported = false;
        button.disabled = true;
        button.title = "Dictation is not supported in this browser";
        button.setAttribute("aria-label", "Dictation is not supported in this browser");
        return;
      }

      let recorder: MediaRecorder | null = null;
      let stream: MediaStream | null = null;
      let chunks: Blob[] = [];
      let recordingMode: VoiceRecordingMode = { kind: "dictation" };
      let audioContext: AudioContext | null = null;
      let analyser: AnalyserNode | null = null;
      let sourceNode: MediaStreamAudioSourceNode | null = null;
      let voiceFrame: number | null = null;
      let timeData: Uint8Array<ArrayBuffer> | null = null;
      let dictationTarget: { document: OpenDocument | null; position: { lineNumber: number; column: number } | null } | null = null;
      let commandDocument: OpenDocument | null = null;
      const voiceBars = voiceVisualizer
        ? Array.from(voiceVisualizer.querySelectorAll<HTMLElement>(".voice-wave-bar"))
        : [];

      function getAudioContextCtor() {
        return window.AudioContext || (window as AudioContextWindow).webkitAudioContext;
      }

      function primeVoiceAudioContext() {
        if (!voiceVisualizer || voiceBars.length === 0 || audioContext) return;
        const AudioContextCtor = getAudioContextCtor();
        if (!AudioContextCtor) return;

        audioContext = new AudioContextCtor();
        void audioContext.resume();
      }

      function setVoiceVisualizerActive(active: boolean) {
        voiceVisualizer?.classList.toggle("active", active);
        voiceVisualizer?.classList.toggle("analyzing", false);
        if (!active) {
          voiceVisualizer?.style.setProperty("--voice-level", "0");
          voiceBars.forEach((bar) => bar.style.setProperty("--level", "0.12"));
        }
      }

      function stopVoiceVisualizer() {
        if (voiceFrame !== null) {
          cancelAnimationFrame(voiceFrame);
          voiceFrame = null;
        }
        sourceNode?.disconnect();
        sourceNode = null;
        analyser = null;
        timeData = null;
        void audioContext?.close();
        audioContext = null;
        setVoiceVisualizerActive(false);
      }

      async function startVoiceVisualizer(inputStream: MediaStream): Promise<boolean> {
        if (voiceFrame !== null) {
          cancelAnimationFrame(voiceFrame);
          voiceFrame = null;
        }
        sourceNode?.disconnect();
        sourceNode = null;
        analyser = null;
        timeData = null;
        setVoiceVisualizerActive(false);

        if (!voiceVisualizer || voiceBars.length === 0) return false;

        const AudioContextCtor = getAudioContextCtor();
        if (!AudioContextCtor) return false;

        try {
          if (!audioContext) audioContext = new AudioContextCtor();
          await audioContext.resume();
          if (audioContext.state !== "running") {
            await new Promise<void>((resolve) => {
              const timeout = window.setTimeout(resolve, 250);
              audioContext?.addEventListener("statechange", () => {
                window.clearTimeout(timeout);
                resolve();
              }, { once: true });
            });
          }

          if (audioContext.state !== "running") return false;

          analyser = audioContext.createAnalyser();
          analyser.fftSize = 512;
          analyser.smoothingTimeConstant = 0.74;
          sourceNode = audioContext.createMediaStreamSource(inputStream);
          sourceNode.connect(analyser);
          timeData = new Uint8Array(analyser.fftSize);
          setVoiceVisualizerActive(true);
          voiceVisualizer.classList.add("analyzing");
        } catch {
          stopVoiceVisualizer();
          return false;
        }

        const tick = () => {
          if (!analyser || !timeData) return;
          const data = timeData;

          analyser.getByteTimeDomainData(data);
          let sumSquares = 0;
          for (let i = 0; i < data.length; i += 1) {
            const centered = (data[i] - 128) / 128;
            sumSquares += centered * centered;
          }
          const rms = Math.sqrt(sumSquares / data.length);
          const level = Math.min(1, Math.max(0.03, rms * 7.5));
          voiceVisualizer.style.setProperty("--voice-level", level.toFixed(3));

          const binWidth = Math.max(1, Math.floor(data.length / voiceBars.length));
          voiceBars.forEach((bar, index) => {
            const start = index * binWidth;
            const end = Math.min(data.length, start + binWidth);
            let peak = 0;
            for (let i = start; i < end; i += 1) {
              peak = Math.max(peak, Math.abs((data[i] - 128) / 128));
            }
            const mirrored = 1 - Math.abs(index - (voiceBars.length - 1) / 2) / ((voiceBars.length - 1) / 2);
            const shaped = Math.min(1, 0.08 + peak * 2.2 + level * 0.52 + mirrored * level * 0.28);
            bar.style.setProperty("--level", shaped.toFixed(3));
          });

          voiceFrame = requestAnimationFrame(tick);
        };

        tick();
        return true;
      }

      async function transcribe(blob: Blob, mimeType: string, mode: VoiceRecordingMode) {
        button.disabled = true;
        button.classList.remove("recording");
        button.classList.add("transcribing");
        setVoiceStatus(mode.kind === "paragraph-command" ? "Transcribing command..." : "Transcribing...");

        try {
          const form = new FormData();
          form.append("audio", blob, `dictation.${recordingExtension(mimeType)}`);
          const res = await fetch("/api/transcribe", { method: "POST", body: form });
          if (res.status === 401) {
            location.reload();
            return;
          }
          const data = await res.json().catch(() => ({}));
          if (!res.ok) {
            throw new Error(data.error || "Transcription failed");
          }
          const text = typeof data.text === "string" ? data.text : "";
          if (mode.kind === "paragraph-command") {
            if (commandDocument === activeDocument) await requestVoiceCommand(mode.paragraphId, text);
          } else {
            insertTranscript(text, dictationTarget?.document?.model, dictationTarget?.position ?? null);
          }
          setVoiceStatus("");
        } catch (err) {
          const message = err instanceof Error ? err.message : "Transcription failed";
          setVoiceStatus(message);
        } finally {
          button.disabled = !activeDocument || !voiceSupported;
          button.classList.remove("transcribing");
          button.classList.remove("command-mode");
          voiceCommandParagraphId = null;
          commandDocument = null;
          updateGutterIcons();
          button.title = "Start dictation";
          button.setAttribute("aria-label", "Start dictation");
        }
      }

      function resetRecordingUi() {
        button.classList.remove("recording");
        button.classList.remove("command-mode");
        button.querySelector(".voice-icon-mic")?.removeAttribute("hidden");
        button.querySelector(".voice-icon-stop")?.setAttribute("hidden", "");
        voiceCommandParagraphId = null;
        updateGutterIcons();
        setVoiceVisualizerActive(false);
      }

      function stopRecording() {
        if (recorder && recorder.state !== "inactive") {
          recorder.stop();
        }
      }

      async function startRecording() {
        chunks = [];
        dictationTarget = recordingMode.kind === "dictation"
          ? { document: activeDocument, position: editor.getPosition() }
          : null;
        const mimeType = chooseRecordingMimeType();
        primeVoiceAudioContext();
        stream = await navigator.mediaDevices.getUserMedia({ audio: true });
        recorder = new MediaRecorder(stream, mimeType ? { mimeType } : undefined);

        recorder.addEventListener("dataavailable", (event: BlobEvent) => {
          if (event.data.size > 0) chunks.push(event.data);
        });

        recorder.addEventListener("stop", () => {
          stream?.getTracks().forEach((track) => track.stop());
          stream = null;
          stopVoiceVisualizer();
          resetRecordingUi();

          const blob = new Blob(chunks, { type: mimeType || "audio/webm" });
          chunks = [];
          if (blob.size === 0) {
            setVoiceStatus("");
            return;
          }
          void transcribe(blob, mimeType || "audio/webm", recordingMode);
        });

        const visualizerStarted = await startVoiceVisualizer(stream);
        recorder.start();
        button.classList.add("recording");
        button.classList.toggle("command-mode", recordingMode.kind === "paragraph-command");
        button.title = recordingMode.kind === "paragraph-command" ? "Stop voice command" : "Stop dictation";
        button.setAttribute("aria-label", button.title);
        button.querySelector(".voice-icon-mic")?.setAttribute("hidden", "");
        button.querySelector(".voice-icon-stop")?.removeAttribute("hidden");
        setVoiceStatus(
          recordingMode.kind === "paragraph-command"
            ? (visualizerStarted ? "Recording command" : "Recording command (audio levels unavailable)")
            : (visualizerStarted ? "Recording" : "Recording (audio levels unavailable)")
        );
      }

      startParagraphVoiceCommand = (paragraphId: string) => {
        if (recorder && recorder.state === "recording") {
          stopRecording();
          return;
        }

        voiceCommandParagraphId = paragraphId;
        commandDocument = activeDocument;
        recordingMode = { kind: "paragraph-command", paragraphId };
        updateGutterIcons();
        startRecording().catch((err) => {
          stream?.getTracks().forEach((track) => track.stop());
          stream = null;
          recorder = null;
          stopVoiceVisualizer();
          resetRecordingUi();
          const message = err instanceof Error ? err.message : "Could not start voice command";
          setVoiceStatus(message);
        });
      };

      button.addEventListener("click", () => {
        if (recorder && recorder.state === "recording") {
          stopRecording();
          return;
        }

        voiceCommandParagraphId = null;
        commandDocument = null;
        updateGutterIcons();
        recordingMode = { kind: "dictation" };
        startRecording().catch((err) => {
          stream?.getTracks().forEach((track) => track.stop());
          stream = null;
          recorder = null;
          stopVoiceVisualizer();
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

    interface OpenDocument {
      path: string;
      model: any;
      revision: string;
      url: string;
      title: string;
      lastSaved: string;
      dirty: boolean;
      saving: boolean;
      applying: boolean;
      deleted: boolean;
      conflict: boolean;
      blocked: boolean;
      failed: boolean;
      saveTimer: ReturnType<typeof setTimeout> | null;
      retryTimer: ReturnType<typeof setTimeout> | null;
      retryDelay: number;
      savePromise: Promise<boolean> | null;
      viewState: unknown;
      decorations: string[];
    }

    const documents = new Map<string, OpenDocument>();
    let activeDocument: OpenDocument | null = null;
    let archiveOpener: () => void = () => {};

    function setSaveState(state: "unsaved" | "saving" | "saved" | "failed") {
      const labels = {
        unsaved: "Unsaved changes",
        saving: "Saving…",
        saved: "Saved",
        failed: "Save failed — retrying",
      };
      saveStateEl.textContent = labels[state];
      saveStateEl.className = `save-state ${state}`;
      saveRetry.hidden = state !== "failed";
    }

    function clearBanner() {
      banner.hidden = true;
      bannerTitle.textContent = "";
      bannerMessage.textContent = "";
      bannerActions.replaceChildren();
    }

    function bannerButton(label: string, run: () => void) {
      const button = document.createElement("button");
      button.type = "button";
      button.textContent = label;
      button.addEventListener("click", run);
      bannerActions.appendChild(button);
    }

    function showConflict(doc: OpenDocument, message = "Your unsaved text is preserved. Choose which version to keep.") {
      doc.conflict = true;
      banner.hidden = false;
      banner.className = "editor-banner conflict";
      bannerTitle.textContent = "This post changed on disk";
      bannerMessage.textContent = message;
      bannerActions.replaceChildren();
      bannerButton("Reload from disk", () => void reloadFromDisk(doc));
      bannerButton("Overwrite", () => {
        doc.conflict = false;
        clearBanner();
        void saveDocument(doc, "overwrite");
      });
    }

    function oldSlug(path: string) {
      return (path.split("/").pop() || "post").replace(/\.md$/, "");
    }

    function showDeleted(doc: OpenDocument) {
      doc.deleted = true;
      if (doc.saveTimer) clearTimeout(doc.saveTimer);
      if (doc.retryTimer) clearTimeout(doc.retryTimer);
      if (doc !== activeDocument) return;
      banner.hidden = false;
      banner.className = "editor-banner deleted";
      bannerTitle.textContent = "File deleted on disk";
      bannerMessage.textContent = "Your unsaved text is still here. Discard it or recover it as a new post.";
      bannerActions.replaceChildren();
      bannerButton("Discard and close", () => {
        documents.delete(doc.path);
        if (doc === activeDocument) clearSelection(true);
        doc.model.dispose();
      });
      bannerButton("Save as new post", () => openRecoveryDialog(doc));
      setSaveState("unsaved");
    }

    function showCollision(doc: OpenDocument, error: ApiError) {
      doc.blocked = true;
      banner.hidden = false;
      banner.className = "editor-banner collision";
      bannerTitle.textContent = error.body.error;
      bannerMessage.textContent = [error.body.conflicting_url, error.body.conflicting_path].filter(Boolean).join(" · ");
      bannerActions.replaceChildren();
      setSaveState("unsaved");
    }

    function handleModelChange(doc: OpenDocument) {
      if (doc === activeDocument) updateWordCount();
      if (!doc.applying) {
        doc.dirty = doc.model.getValue() !== doc.lastSaved;
        if (doc.blocked) doc.blocked = false;
        if (doc === activeDocument) {
          if (doc.deleted) showDeleted(doc);
          else if (doc.conflict) showConflict(doc);
          else clearBanner();
          setSaveState(doc.failed ? "failed" : doc.dirty ? "unsaved" : "saved");
        }
        if (doc.saveTimer) clearTimeout(doc.saveTimer);
        if (doc.dirty && !doc.deleted && !doc.conflict) {
          doc.saveTimer = setTimeout(() => void saveDocument(doc), 5_000);
        }
      }

      if (S.reconcileTimer) clearTimeout(S.reconcileTimer);
      S.setReconcileTimer(setTimeout(() => {
        if (doc !== activeDocument) return;
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
    }

    function scheduleRetry(doc: OpenDocument) {
      if (doc.retryTimer || doc.deleted || doc.conflict || doc.blocked) return;
      doc.retryTimer = setTimeout(() => {
        doc.retryTimer = null;
        void saveDocument(doc);
      }, doc.retryDelay);
      doc.retryDelay = Math.min(doc.retryDelay * 2, 15_000);
    }

    async function saveDocument(doc: OpenDocument, revision = doc.revision): Promise<boolean> {
      if (doc.deleted || doc.conflict || doc.blocked || !doc.dirty) return !doc.dirty;
      if (doc.saving && doc.savePromise) return doc.savePromise;
      if (doc.saveTimer) clearTimeout(doc.saveTimer);
      doc.saveTimer = null;
      doc.saving = true;
      if (doc === activeDocument) setSaveState("saving");
      const content = doc.model.getValue();
      const pending = (async () => {
        try {
          const result = await api<SaveResponse>("/api/post/save", jsonRequest("POST", {
            path: doc.path,
            content,
            base_revision: revision,
          }));
          doc.revision = result.revision;
          doc.url = result.url;
          doc.lastSaved = content;
          doc.dirty = doc.model.getValue() !== content;
          doc.retryDelay = 1500;
          doc.failed = false;
          if (doc.retryTimer) clearTimeout(doc.retryTimer);
          doc.retryTimer = null;
          if (doc === activeDocument) {
            setSaveState(doc.dirty ? "unsaved" : "saved");
            window.dispatchEvent(new CustomEvent("blogger-post-selected", { detail: { path: doc.path, url: doc.url } }));
          }
          mutationEvent({ kind: "post-save" });
          if (doc.dirty) doc.saveTimer = setTimeout(() => void saveDocument(doc), 1_000);
          return true;
        } catch (error) {
          if (error instanceof ApiError && error.status === 409) {
            if (error.body.deleted) showDeleted(doc);
            else if (doc === activeDocument) showConflict(doc);
            return false;
          }
          if (error instanceof ApiError && error.status === 422) {
            if (doc === activeDocument) showCollision(doc, error);
            return false;
          }
          if (doc === activeDocument) setSaveState("failed");
          doc.failed = true;
          scheduleRetry(doc);
          return false;
        } finally {
          doc.saving = false;
          doc.savePromise = null;
        }
      })();
      doc.savePromise = pending;
      return pending;
    }

    async function flushDocument(doc: OpenDocument): Promise<boolean> {
      if (doc.saveTimer) clearTimeout(doc.saveTimer);
      doc.saveTimer = null;
      while (doc.saving && doc.savePromise) await doc.savePromise;
      while (doc.dirty && !doc.deleted && !doc.conflict && !doc.blocked) {
        if (!(await saveDocument(doc))) return false;
      }
      return !doc.dirty;
    }

    async function reloadFromDisk(doc: OpenDocument) {
      try {
        const loaded = await api<PostResponse>(`/api/post?path=${encodeURIComponent(doc.path)}`);
        doc.applying = true;
        const view = doc === activeDocument ? editor.saveViewState() : doc.viewState;
        doc.model.setValue(loaded.content);
        doc.applying = false;
        doc.revision = loaded.revision;
        doc.url = loaded.url;
        doc.title = loaded.title;
        doc.lastSaved = loaded.content;
        doc.dirty = false;
        doc.conflict = false;
        doc.deleted = false;
        doc.failed = false;
        if (doc.retryTimer) clearTimeout(doc.retryTimer);
        doc.retryTimer = null;
        doc.decorations = [];
        clearBanner();
        if (doc === activeDocument) {
          if (view) editor.restoreViewState(view);
          setSaveState("saved");
          postTitleEl.textContent = doc.title;
          updateWordCount();
          reconcileParagraphs(model);
          updateGutterIcons();
          window.dispatchEvent(new CustomEvent("blogger-post-selected", { detail: { path: doc.path, url: doc.url } }));
        }
      } catch (error) {
        if (error instanceof ApiError && error.status === 404) showDeleted(doc);
      }
    }

    function openRecoveryDialog(doc: OpenDocument) {
      const modal = openModal("Save as new post");
      const slug = document.createElement("input");
      slug.type = "text";
      slug.value = `${oldSlug(doc.path)}-recovered`;
      modal.body.append(field("New slug", slug));
      const cancel = modalButton("Cancel");
      const save = modalButton("Recover post", "confirm");
      cancel.addEventListener("click", modal.close);
      save.addEventListener("click", async () => {
        save.disabled = true;
        try {
          const recovered = await api<RecoverPostResponse>("/api/post/recover", jsonRequest("POST", {
            content: doc.model.getValue(), slug: slug.value.trim(),
          }));
          const content = doc.model.getValue();
          documents.delete(doc.path);
          if (doc === activeDocument) {
            activeDocument = null;
            activeModel = null;
            editor.setModel(null);
          }
          doc.model.dispose();
          modal.close();
          const loaded: PostResponse = {
            ...recovered,
            content,
            title: doc.title,
          };
          await openPost(recovered.path, loaded, true);
          mutationEvent({ kind: "post-recover" });
        } catch (error) {
          showModalError(modal, error instanceof Error ? error.message : "Recovery failed");
          save.disabled = false;
        }
      });
      modal.actions.append(cancel, save);
      requestAnimationFrame(() => slug.focus());
    }

    async function confirmDiscard(): Promise<boolean> {
      return new Promise((done) => {
        const modal = openModal("Unsaved changes");
        let settled = false;
        const message = document.createElement("p");
        message.textContent = "This post cannot be saved right now. Stay here, or discard the unsaved changes and switch posts.";
        modal.body.appendChild(message);
        const stay = modalButton("Stay");
        const discard = modalButton("Discard and switch", "delete");
        const finish = (choice: boolean) => {
          if (settled) return;
          settled = true;
          modal.close();
          done(choice);
        };
        const observer = new MutationObserver(() => {
          if (!modal.overlay.isConnected) {
            observer.disconnect();
            finish(false);
          }
        });
        observer.observe(document.body, { childList: true });
        stay.addEventListener("click", () => finish(false));
        discard.addEventListener("click", () => finish(true));
        modal.actions.append(discard, stay);
      });
    }

    async function openPost(path: string, supplied?: PostResponse, force = false): Promise<boolean> {
      if (activeDocument?.path === path) return true;
      if (activeDocument?.dirty && !force) {
        const saved = await flushDocument(activeDocument);
        if (!saved && !(await confirmDiscard())) return false;
        if (activeDocument.dirty) {
          activeDocument.applying = true;
          activeDocument.model.setValue(activeDocument.lastSaved);
          activeDocument.applying = false;
          activeDocument.dirty = false;
          activeDocument.conflict = false;
          activeDocument.deleted = false;
          activeDocument.blocked = false;
          activeDocument.failed = false;
          if (activeDocument.retryTimer) clearTimeout(activeDocument.retryTimer);
          activeDocument.retryTimer = null;
        }
      }
      let doc = documents.get(path);
      try {
        const loaded = supplied ?? await api<PostResponse>(`/api/post?path=${encodeURIComponent(path)}`);
        if (!doc) {
          const postModel = monaco.editor.createModel(loaded.content, "markdown", monaco.Uri.parse(`inmemory://post/${loaded.path}`));
          doc = {
            path: loaded.path, model: postModel, revision: loaded.revision, url: loaded.url,
            title: loaded.title, lastSaved: loaded.content, dirty: false, saving: false,
            applying: false, deleted: false, conflict: false, blocked: false,
            failed: false,
            saveTimer: null, retryTimer: null, retryDelay: 1500, savePromise: null, viewState: null,
            decorations: [],
          };
          documents.set(path, doc);
          postModel.onDidChangeContent(() => handleModelChange(doc!));
        } else if (!doc.dirty && doc.revision !== loaded.revision) {
          doc.applying = true;
          doc.model.setValue(loaded.content);
          doc.applying = false;
          doc.revision = loaded.revision;
          doc.lastSaved = loaded.content;
          doc.url = loaded.url;
          doc.title = loaded.title;
          doc.conflict = false;
          doc.deleted = false;
          doc.blocked = false;
        }
        if (doc && !doc.dirty) {
          doc.conflict = false;
          doc.deleted = false;
          doc.blocked = false;
        }
      } catch (error) {
        if (error instanceof ApiError && error.status === 404) {
          if (localStorage.getItem("blogger-selected-post") === path) localStorage.removeItem("blogger-selected-post");
          clearSelection(true);
          return false;
        }
        throw error;
      }
      if (activeDocument) activeDocument.viewState = editor.saveViewState();
      activeDocument = doc!;
      activeModel = doc!.model;
      editor.setModel(activeModel);
      gutterDecorations = doc!.decorations;
      if (doc!.viewState) editor.restoreViewState(doc!.viewState);
      emptyState.hidden = true;
      editorContainer.classList.remove("empty");
      includeContext.disabled = false;
      if (voiceBtn) voiceBtn.disabled = !voiceSupported;
      postTitleEl.textContent = doc!.title;
      localStorage.setItem("blogger-selected-post", doc!.path);
      setSaveState(doc!.failed ? "failed" : doc!.dirty ? "unsaved" : "saved");
      clearBanner();
      S.paragraphMap.clear();
      S.nopParagraphs.clear();
      S.setCurrentParagraphId(null);
      S.setAnchorText(null);
      reconcileParagraphs(model);
      updateWordCount();
      updateGutterIcons();
      editor.layout();
      editor.focus();
      window.dispatchEvent(new CustomEvent("blogger-post-selected", { detail: { path: doc!.path, url: doc!.url } }));
      return true;
    }

    function clearSelection(openArchive = false) {
      if (activeDocument) activeDocument.viewState = editor.saveViewState();
      activeDocument = null;
      activeModel = null;
      editor.setModel(null);
      localStorage.removeItem("blogger-selected-post");
      editorContainer.classList.add("empty");
      emptyState.hidden = false;
      postTitleEl.textContent = "";
      wordCountEl.textContent = "0 words";
      includeContext.disabled = true;
      if (voiceBtn) voiceBtn.disabled = true;
      saveStateEl.textContent = "";
      saveRetry.hidden = true;
      clearBanner();
      S.paragraphMap.clear();
      paragraphActionsLayer.replaceChildren();
      window.dispatchEvent(new CustomEvent("blogger-post-selected", { detail: { path: null, url: null } }));
      if (openArchive) archiveOpener();
    }

    async function checkActiveRevision() {
      const doc = activeDocument;
      if (!doc || doc.deleted || doc.saving) return;
      try {
        const loaded = await api<PostResponse>(`/api/post?path=${encodeURIComponent(doc.path)}`);
        if (loaded.revision === doc.revision) return;
        if (doc.dirty) showConflict(doc, "The disk version changed while you were editing. Your text has not been replaced.");
        else await reloadFromDisk(doc);
      } catch (error) {
        if (error instanceof ApiError && error.status === 404) {
          if (doc.dirty) showDeleted(doc);
          else clearSelection(true);
        }
      }
    }

    initVoiceInput();
    setInterval(() => void checkActiveRevision(), 3_000);

    let gutterDecorations: string[] = [];
    function renderParagraphActions() {
      paragraphActionsLayer.replaceChildren();
      if (!activeModel) return;
      const scrollTop = editor.getScrollTop();
      const lineHeight = 28;

      for (const [id, para] of S.paragraphMap) {
        if (para.startLine === para.endLine && parseImageLine(model.getLineContent(para.startLine))) continue;

        const top = editor.getTopForLineNumber(para.startLine) - scrollTop + Math.max(0, (lineHeight - 28) / 2);
        if (top < -28 || top > editorContainer.clientHeight) continue;

        const group = document.createElement("div");
        group.className = "paragraph-action-group";
        group.style.top = `${top}px`;
        group.dataset.paragraphId = id;

        const feedbackBtn = document.createElement("button");
        feedbackBtn.type = "button";
        feedbackBtn.className = "paragraph-action-btn feedback";
        feedbackBtn.title = S.nopParagraphs.has(id) ? "AI: paragraph looks good" : "Get AI feedback on this paragraph";
        feedbackBtn.setAttribute("aria-label", feedbackBtn.title);
        feedbackBtn.innerHTML = FEEDBACK_ICON_SVG;
        if (S.nopParagraphs.has(id)) feedbackBtn.classList.add("nop");
        if (S.processingParagraphId === id) feedbackBtn.classList.add("processing");
        feedbackBtn.addEventListener("click", (event) => {
          event.preventDefault();
          event.stopPropagation();
          requestSuggestion(id);
        });

        const commandBtn = document.createElement("button");
        commandBtn.type = "button";
        commandBtn.className = "paragraph-action-btn command";
        commandBtn.title = "Speak an instruction for this paragraph";
        commandBtn.setAttribute("aria-label", commandBtn.title);
        commandBtn.innerHTML = MIC_ICON_SVG;
        if (voiceCommandParagraphId === id) commandBtn.classList.add("listening");
        commandBtn.addEventListener("click", (event) => {
          event.preventDefault();
          event.stopPropagation();
          startParagraphVoiceCommand(id);
        });

        group.appendChild(feedbackBtn);
        group.appendChild(commandBtn);
        paragraphActionsLayer.appendChild(group);
      }
    }

    function updateGutterIcons() {
      if (!activeModel) {
        gutterDecorations = editor.deltaDecorations(gutterDecorations, []);
        renderParagraphActions();
        return;
      }
      const decorations: IModelDeltaDecoration[] = [];
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
      if (activeDocument) activeDocument.decorations = gutterDecorations;
      renderParagraphActions();
    }
    S.setOnProcessingChanged(() => updateGutterIcons());

    editor.onMouseDown((e: {
      target: {
        type: number;
        position: { lineNumber: number; column: number } | null;
        element?: HTMLElement | null;
      };
    }) => {
      if (!activeModel) return;
      if (e.target.type === 2 && e.target.position) {
        const ln = e.target.position.lineNumber;
        const line = model.getLineContent(ln);
        const operationDocument = activeDocument;
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
                .then((r) => {
                  if (r.status === 401) location.reload();
                  return r.ok ? r.json() : Promise.reject(r.statusText);
                })
                .then((data: { path: string }) => {
                  mutationEvent({ kind: "image-rename" });
                  const newLine = `![${img.alt}](${data.path})`;
                  const range = new monaco.Range(ln, 1, ln, line.length + 1);
                  applyModelEdit(operationDocument?.model, "rename-image", range, newLine);
                })
                .catch(() => {});
            },
            () => {
              fetch("/api/delete-image", {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ path: img.path }),
              })
                .then((response) => {
                  if (response.status === 401) location.reload();
                  if (!response.ok) throw new Error(response.statusText);
                  mutationEvent({ kind: "image-delete" });
                  const operationModel = operationDocument?.model;
                  if (!operationModel || operationModel.isDisposed?.()) return;
                  const startLn = ln > 1 && operationModel.getLineContent(ln - 1).trim() === "" ? ln - 1 : ln;
                  const endLn = ln < operationModel.getLineCount() && operationModel.getLineContent(ln + 1).trim() === "" ? ln + 1 : ln;
                  const range = new monaco.Range(startLn, 1, endLn, operationModel.getLineContent(endLn).length + 1);
                  applyModelEdit(operationModel, "delete-image", range, "");
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

    editor.onDidScrollChange(() => renderParagraphActions());
    editor.onDidLayoutChange(() => renderParagraphActions());
    window.addEventListener("resize", () => {
      editor.layout();
      renderParagraphActions();
    });

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
      const uploadDocument = activeDocument;
      const uploadModel = uploadDocument?.model;

      const ext = file.type.split("/")[1]?.replace("jpeg", "jpg") || "png";
      const defaultName = file.name && file.name !== "image.png" ? file.name : `paste.${ext}`;

      const placeholder = "![uploading...]()";
      const range = new monaco.Range(pos.lineNumber, pos.column, pos.lineNumber, pos.column);
      applyModelEdit(uploadModel, "paste-image", range, placeholder);

      const form = new FormData();
      form.append("image", file, defaultName);

      fetch("/api/upload-image", { method: "POST", body: form })
        .then((res) => {
          if (res.status === 401) location.reload();
          return res.ok ? res.json() : Promise.reject(res.statusText);
        })
        .then((data: { path: string }) => {
          mutationEvent({ kind: "image-upload" });
          const mdLink = `![](${data.path})`;
          const targetModel = uploadDocument?.model;
          if (!targetModel || targetModel.isDisposed?.()) return;
          const matches = targetModel.findMatches(placeholder, false, false, true, null, true);
          if (matches.length > 0) {
            applyModelEdit(targetModel, "paste-image", matches[0].range, mdLink);
          }
        })
        .catch(() => {
          const targetModel = uploadDocument?.model;
          if (!targetModel || targetModel.isDisposed?.()) return;
          const matches = targetModel.findMatches(placeholder, false, false, true, null, true);
          if (matches.length > 0) {
            applyModelEdit(targetModel, "paste-image", matches[0].range, "![upload failed]()");
          }
        });
    }, { capture: true });

    S.setGetEditorValue(() => activeModel ? model.getValue() : "");
    S.setApplyEditorEdit((oldText: string, newText: string): boolean => {
      if (!activeModel) return false;
      const matches = model.findMatches(oldText, false, false, true, null, true);
      if (matches.length === 0) return false;
      editor.executeEdits("ai-fix", [{ range: matches[0].range, text: newText }]);
      return true;
    });
    saveRetry.addEventListener("click", () => {
      if (activeDocument) {
        if (activeDocument.retryTimer) clearTimeout(activeDocument.retryTimer);
        activeDocument.retryTimer = null;
        void saveDocument(activeDocument);
      }
    });

    function renameDocument(oldPath: string, newPath: string, url: string) {
      const doc = documents.get(oldPath);
      if (!doc) return;
      const content = doc.model.getValue();
      const wasActive = doc === activeDocument;
      if (wasActive) {
        doc.viewState = editor.saveViewState();
        editor.setModel(null);
      }
      const replacement = monaco.editor.createModel(content, "markdown", monaco.Uri.parse(`inmemory://post/${newPath}`));
      doc.model.dispose();
      documents.delete(oldPath);
      doc.path = newPath;
      doc.url = url;
      doc.model = replacement;
      doc.failed = false;
      if (doc.retryTimer) clearTimeout(doc.retryTimer);
      doc.retryTimer = null;
      doc.decorations = [];
      replacement.onDidChangeContent(() => handleModelChange(doc));
      documents.set(newPath, doc);
      if (wasActive) {
        activeModel = replacement;
        editor.setModel(replacement);
        gutterDecorations = [];
        if (doc.viewState) editor.restoreViewState(doc.viewState);
        localStorage.setItem("blogger-selected-post", newPath);
        window.dispatchEvent(new CustomEvent("blogger-post-selected", { detail: { path: newPath, url } }));
        updateGutterIcons();
      }
    }

    function disposeDocument(path: string) {
      const doc = documents.get(path);
      if (!doc) return;
      if (doc.saveTimer) clearTimeout(doc.saveTimer);
      if (doc.retryTimer) clearTimeout(doc.retryTimer);
      documents.delete(path);
      doc.deleted = true;
      if (doc === activeDocument) clearSelection(false);
      doc.model.dispose();
    }

    const controller: PostDocumentController = {
      async initializeSelection() {
        const remembered = localStorage.getItem("blogger-selected-post");
        if (remembered) {
          try {
            if (await openPost(remembered)) return;
          } catch {
            // The archive remains available when the remembered post cannot load.
          }
        }
        clearSelection(true);
      },
      openPost,
      async flush(path) {
        const doc = path ? documents.get(path) : activeDocument;
        if (!doc) return true;
        return flushDocument(doc);
      },
      isDirty(path) { return documents.get(path)?.dirty ?? false; },
      async getRevision(path) {
        const doc = documents.get(path);
        if (doc) return doc.revision;
        return (await api<PostResponse>(`/api/post?path=${encodeURIComponent(path)}`)).revision;
      },
      getActivePath() { return activeDocument?.path ?? null; },
      renameDocument,
      disposeDocument,
      clearSelection,
      checkActiveRevision,
      setArchiveOpener(opener) { archiveOpener = opener; },
    };
    clearSelection(false);
    resolve(controller);
  });
  });
}
