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
            getLineCount: () => number;
            getLineContent: (lineNumber: number) => string;
            findMatches: (searchString: string, searchOnlyEditableRange: boolean, isRegex: boolean, matchCase: boolean, wordSeparators: string | null, captureMatches: boolean) => { range: { startLineNumber: number; startColumn: number; endLineNumber: number; endColumn: number } }[];
          };
          getPosition: () => { lineNumber: number; column: number } | null;
          onDidChangeCursorPosition: (cb: (e: { position: { lineNumber: number; column: number } }) => void) => void;
          onMouseDown: (cb: (e: { target: { type: number; position: { lineNumber: number; column: number } | null } }) => void) => void;
          deltaDecorations: (oldDecorations: string[], newDecorations: IModelDeltaDecoration[]) => string[];
          executeEdits: (source: string, edits: { range: { startLineNumber: number; startColumn: number; endLineNumber: number; endColumn: number }; text: string }[]) => void;
          layout: () => void;
        };
        defineTheme: (name: string, data: Record<string, unknown>) => void;
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
    try {
      const res = await fetch("/api/initial-content");
      if (res.ok) {
        const data = await res.json();
        if (data.content) {
          initialContent = data.content;
          hasFile = true;
        }
      }
    } catch {
      // use default content
    }

    let saveTimer: ReturnType<typeof setTimeout> | null = null;
    let saving = false;
    let zolaUrl: string | null = null;
    let recoveryTimer: ReturnType<typeof setTimeout> | null = null;
    const previewIframe = document.getElementById("ref-iframe") as HTMLIFrameElement | null;

    async function autoSave(content: string) {
      if (saving) return;
      saving = true;
      try {
        await fetch("/api/save", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ content }),
        });
        if (previewIframe && previewIframe.src.includes("localhost:1111")) {
          zolaUrl = previewIframe.src;
          previewIframe.src = "placeholder.html";
          if (recoveryTimer) clearTimeout(recoveryTimer);
          const poll = async (retries: number) => {
            if (retries <= 0 || !zolaUrl) { previewIframe.src = zolaUrl || ""; return; }
            try {
              await fetch(zolaUrl!, { mode: "no-cors" });
              previewIframe.src = zolaUrl!;
              zolaUrl = null;
            } catch {
              recoveryTimer = setTimeout(() => poll(retries - 1), 400);
            }
          };
          recoveryTimer = setTimeout(() => poll(30), 800);
        }
      } catch {
        // silently ignore save errors
      }
      saving = false;
    }

    const editor = monaco.editor.create(
      document.getElementById("editor-container")!,
      {
        value: initialContent,
        language: "markdown",
        theme: "nexus",
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

      if (hasFile) {
        if (saveTimer) clearTimeout(saveTimer);
        saveTimer = setTimeout(() => autoSave(model.getValue()), 20_000);
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
        const isProcessing = id === S.processingParagraphId;
        const isNop = S.nopParagraphs.has(id);
        let className = "paragraph-action-icon";
        if (isProcessing) className += " paragraph-action-processing";
        else if (isNop) className += " paragraph-action-nop";
        let hoverMsg = "Get AI feedback on this paragraph";
        if (isProcessing) hoverMsg = "Analyzing paragraph…";
        else if (isNop) hoverMsg = "AI: paragraph looks good";
        decorations.push({
          range: new monaco.Range(para.startLine, 1, para.startLine, 1),
          options: {
            glyphMarginClassName: className,
            glyphMarginHoverMessage: { value: hoverMsg },
          },
        });
      }
      gutterDecorations = editor.deltaDecorations(gutterDecorations, decorations);
    }
    updateGutterIcons();
    S.setOnProcessingChanged(() => updateGutterIcons());

    editor.onMouseDown((e: { target: { type: number; position: { lineNumber: number; column: number } | null } }) => {
      if (e.target.type === 2 && e.target.position) {
        const paraId = getParagraphAtLine(e.target.position.lineNumber);
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

    S.setGetEditorValue(() => model.getValue());
    S.setApplyEditorEdit((oldText: string, newText: string): boolean => {
      const matches = model.findMatches(oldText, false, false, true, null, true);
      if (matches.length === 0) return false;
      editor.executeEdits("ai-fix", [{ range: matches[0].range, text: newText }]);
      return true;
    });
  });
}
