import { TrackedParagraph } from "./types.js";

export const paragraphMap: Map<string, TrackedParagraph> = new Map();
export let currentParagraphId: string | null = null;
export let anchorText: string | null = null;
export let lastSuggestionTime = 0;
export let debounceTimer: ReturnType<typeof setTimeout> | null = null;
export let reconcileTimer: ReturnType<typeof setTimeout> | null = null;
export let suggestionInFlight = false;
export let lastSuggestedParaId: string | null = null;
export let lastSuggestedVersion: string | null = null;
export let processingParagraphId: string | null = null;
export const nopParagraphs = new Set<string>();

export let postSuggestion: ((content: string, edits?: { old_text: string; new_text: string }[]) => void) | null = null;
export let showFeedbackIndicator: (() => void) | null = null;
export let hideFeedbackIndicator: (() => void) | null = null;
export let onProcessingChanged: (() => void) | null = null;
export let getEditorValue: () => string = () => "";
export let applyEditorEdit: ((oldText: string, newText: string) => boolean) | null = null;

export function setCurrentParagraphId(id: string | null) { currentParagraphId = id; }
export function setAnchorText(text: string | null) { anchorText = text; }
export function setLastSuggestionTime(t: number) { lastSuggestionTime = t; }
export function setDebounceTimer(t: ReturnType<typeof setTimeout> | null) { debounceTimer = t; }
export function setReconcileTimer(t: ReturnType<typeof setTimeout> | null) { reconcileTimer = t; }
export function setSuggestionInFlight(v: boolean) { suggestionInFlight = v; }
export function setLastSuggestedParaId(id: string | null) { lastSuggestedParaId = id; }
export function setLastSuggestedVersion(v: string | null) { lastSuggestedVersion = v; }
export function setProcessingParagraphId(id: string | null) { processingParagraphId = id; }
export function setPostSuggestion(fn: typeof postSuggestion) { postSuggestion = fn; }
export function setShowFeedbackIndicator(fn: typeof showFeedbackIndicator) { showFeedbackIndicator = fn; }
export function setHideFeedbackIndicator(fn: typeof hideFeedbackIndicator) { hideFeedbackIndicator = fn; }
export function setOnProcessingChanged(fn: typeof onProcessingChanged) { onProcessingChanged = fn; }
export function setGetEditorValue(fn: () => string) { getEditorValue = fn; }
export function setApplyEditorEdit(fn: typeof applyEditorEdit) { applyEditorEdit = fn; }
