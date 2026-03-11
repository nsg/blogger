import { ParsedParagraph } from "./types.js";
import * as S from "./state.js";

export function hashString(s: string): string {
  let h = 5381;
  for (let i = 0; i < s.length; i++) {
    h = ((h << 5) + h + s.charCodeAt(i)) | 0;
  }
  return (h >>> 0).toString(16);
}

export function parseParagraphs(model: { getLineCount: () => number; getLineContent: (n: number) => string }): ParsedParagraph[] {
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

export function getParagraphAtLine(lineNumber: number): string | null {
  for (const [id, p] of S.paragraphMap) {
    if (lineNumber >= p.startLine && lineNumber <= p.endLine) return id;
  }
  return null;
}

export function computeChangeRatio(a: string, b: string): number {
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

export function canSuggest(): boolean {
  return !S.suggestionInFlight && Date.now() - S.lastSuggestionTime >= 30_000;
}

export function reconcileParagraphs(model: { getLineCount: () => number; getLineContent: (n: number) => string }) {
  const parsed = parseParagraphs(model);
  const usedIds = new Set<string>();
  const existingEntries = Array.from(S.paragraphMap.values());

  let ei = 0;
  for (const pp of parsed) {
    let matched = false;
    for (let k = ei; k < existingEntries.length; k++) {
      const existing = existingEntries[k];
      if (usedIds.has(existing.id)) continue;
      const ratio = computeChangeRatio(existing.currentText, pp.text);
      if (ratio < 0.7) {
        usedIds.add(existing.id);
        existing.startLine = pp.startLine;
        existing.endLine = pp.endLine;
        if (existing.currentText !== pp.text) {
          existing.currentText = pp.text;
          existing.history.push({ text: pp.text, timestamp: Date.now() });
          if (existing.history.length > 5) existing.history.shift();
          S.nopParagraphs.delete(existing.id);
        }
        ei = k + 1;
        matched = true;
        break;
      }
    }
    if (!matched) {
      const id = hashString(pp.text) + "-" + pp.startLine;
      S.paragraphMap.set(id, {
        id,
        currentText: pp.text,
        history: [{ text: pp.text, timestamp: Date.now() }],
        startLine: pp.startLine,
        endLine: pp.endLine,
      });
      usedIds.add(id);
    }
  }

  for (const [id] of S.paragraphMap) {
    if (!usedIds.has(id)) { S.paragraphMap.delete(id); S.nopParagraphs.delete(id); }
  }
}

export async function requestSuggestion(paragraphId: string) {
  const para = S.paragraphMap.get(paragraphId);
  if (!para || para.history.length < 1 || !S.postSuggestion) return;
  if (S.suggestionInFlight) return;
  if (paragraphId === S.lastSuggestedParaId && para.currentText === S.lastSuggestedVersion) return;

  S.setSuggestionInFlight(true);
  S.setProcessingParagraphId(paragraphId);
  if (S.onProcessingChanged) S.onProcessingChanged();
  S.setLastSuggestionTime(Date.now());
  if (S.showFeedbackIndicator) S.showFeedbackIndicator();

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
      content: `[Full document]\n${S.getEditorValue()}\n[/Full document]\n\nParagraph history (${para.history.length} version${para.history.length > 1 ? "s" : ""}):\n\n${historyText}`,
    },
  ];

  try {
    const res = await fetch("/api/chat", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ messages }),
    });
    if (!res.ok) { S.setSuggestionInFlight(false); S.setProcessingParagraphId(null); if (S.onProcessingChanged) S.onProcessingChanged(); if (S.hideFeedbackIndicator) S.hideFeedbackIndicator(); return; }
    const data = await res.json();
    const reply =
      data?.message?.content || data?.choices?.[0]?.message?.content || "";
    const toolCalls = data?.message?.tool_calls as
      | { function: { name: string; arguments: { old_text: string; new_text: string } } }[]
      | undefined;
    const edits = (toolCalls || [])
      .filter((tc) => tc.function?.name === "edit_paragraph")
      .map((tc) => tc.function.arguments);

    if (reply && reply.trim() !== "NOP" && S.postSuggestion) {
      S.setLastSuggestedParaId(paragraphId);
      S.setLastSuggestedVersion(para.currentText);
      S.nopParagraphs.delete(paragraphId);
      const previewWords = para.currentText.split(/\s+/).slice(0, 6).join(" ");
      const ref = `<span class="suggestion-ref">Re: "${previewWords}..."</span>`;
      S.postSuggestion(ref + reply, edits.length > 0 ? edits : undefined);
    } else if (edits.length > 0 && S.postSuggestion) {
      S.setLastSuggestedParaId(paragraphId);
      S.setLastSuggestedVersion(para.currentText);
      S.nopParagraphs.delete(paragraphId);
      const previewWords = para.currentText.split(/\s+/).slice(0, 6).join(" ");
      const ref = `<span class="suggestion-ref">Re: "${previewWords}..."</span>`;
      S.postSuggestion(ref + "Suggested edit:", edits);
    } else if (reply && reply.trim() === "NOP") {
      S.setLastSuggestedParaId(paragraphId);
      S.setLastSuggestedVersion(para.currentText);
      S.nopParagraphs.add(paragraphId);
    }
  } catch {
    // silently ignore suggestion errors
  }
  if (S.hideFeedbackIndicator) S.hideFeedbackIndicator();
  S.setSuggestionInFlight(false);
  S.setProcessingParagraphId(null);
  if (S.onProcessingChanged) S.onProcessingChanged();
}
