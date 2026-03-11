declare const marked: {
  parse: (src: string) => string;
};

import { ChatMessage } from "./types.js";
import * as S from "./state.js";

function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

function appendEditButtons(
  bubble: HTMLElement,
  edits: { old_text: string; new_text: string }[]
) {
  for (const edit of edits) {
    const container = document.createElement("div");
    container.className = "apply-fix-container";

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
      if (S.applyEditorEdit && S.applyEditorEdit(edit.old_text, edit.new_text)) {
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

export function initAssistant() {
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
      const editorText = S.getEditorValue();
      if (editorText.trim()) {
        userContent = `[Editor content]\n${editorText}\n[/Editor content]\n\n${text}`;
      }
    }

    history.push({ role: "user", content: userContent });

    addTypingIndicator();

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

  S.setPostSuggestion((content: string, edits?: { old_text: string; new_text: string }[]) => {
    const bubble = addMessageEl("ai", content, "suggestion");
    if (edits && edits.length > 0 && bubble) {
      appendEditButtons(bubble, edits);
    }
  });

  S.setShowFeedbackIndicator(() => {
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
  });

  S.setHideFeedbackIndicator(() => {
    const el = document.getElementById("feedback-indicator");
    if (el) el.remove();
  });
}
