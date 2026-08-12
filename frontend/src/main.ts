import * as S from "./state.js";
import { initMonaco } from "./editor.js";
import { initAssistant } from "./assistant.js";
import { initThemeToggle, initPaneToggles, initDividerDrag, initResponsivePreviewPane, initWorkPaneTabs, initUrlBar, initPreview, initLogout } from "./ui.js";
import { initArchive } from "./archive.js";
import { initGitUi } from "./gitui.js";

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
initPaneToggles();
initResponsivePreviewPane();
initWorkPaneTabs();
initDividerDrag("divider-left", "pane-left", "pane-center");
initDividerDrag("divider-right", "pane-center", "pane-right");
initUrlBar();
initAssistant();
initPreview();
initLogout();

async function startEditor() {
  const documents = await initMonaco();
  const git = initGitUi(documents);
  initArchive(documents, git);
  await documents.initializeSelection();
}

void startEditor();
