import * as S from "./state.js";
import { initMonaco } from "./editor.js";
import { initAssistant } from "./assistant.js";
import { initThemeToggle, initPaneToggles, initDividerDrag, initResponsivePreviewPane, initWorkPaneTabs, initUrlBar, initPreview } from "./ui.js";

// Expose debug state for testing
(window as unknown as Record<string, unknown>).__debug = {
  get paragraphMap() { return S.paragraphMap; },
  get currentParagraphId() { return S.currentParagraphId; },
  get anchorText() { return S.anchorText; },
  get suggestionInFlight() { return S.suggestionInFlight; },
  get lastSuggestionTime() { return S.lastSuggestionTime; },
  get canSuggest() { return !S.suggestionInFlight && Date.now() - S.lastSuggestionTime >= 30_000; },
};

initThemeToggle();
initMonaco();
initPaneToggles();
initResponsivePreviewPane();
initWorkPaneTabs();
initDividerDrag("divider-left", "pane-left", "pane-center");
initDividerDrag("divider-right", "pane-center", "pane-right");
initUrlBar();
initAssistant();
initPreview();
