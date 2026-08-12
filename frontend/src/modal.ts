export interface ModalHandle {
  overlay: HTMLDivElement;
  dialog: HTMLDivElement;
  body: HTMLDivElement;
  actions: HTMLDivElement;
  error: HTMLDivElement;
  close(): void;
}

export function openModal(titleText: string, className = ""): ModalHandle {
  const overlay = document.createElement("div");
  overlay.className = "image-dialog-overlay modal-overlay";

  const dialog = document.createElement("div");
  dialog.className = `image-dialog generic-modal ${className}`.trim();
  dialog.setAttribute("role", "dialog");
  dialog.setAttribute("aria-modal", "true");
  dialog.setAttribute("aria-label", titleText);

  const title = document.createElement("h2");
  title.className = "modal-title";
  title.textContent = titleText;
  const body = document.createElement("div");
  body.className = "modal-body";
  const error = document.createElement("div");
  error.className = "modal-error";
  error.hidden = true;
  error.setAttribute("role", "alert");
  const actions = document.createElement("div");
  actions.className = "image-dialog-buttons modal-actions";

  dialog.append(title, body, error, actions);
  overlay.appendChild(dialog);
  document.body.appendChild(overlay);

  const close = () => {
    document.removeEventListener("keydown", onKeydown);
    overlay.classList.remove("visible");
    window.setTimeout(() => overlay.remove(), 150);
  };
  const onKeydown = (event: KeyboardEvent) => {
    if (event.key === "Escape") close();
  };
  document.addEventListener("keydown", onKeydown);
  overlay.addEventListener("mousedown", (event) => {
    if (event.target === overlay) close();
  });
  requestAnimationFrame(() => overlay.classList.add("visible"));

  return { overlay, dialog, body, actions, error, close };
}

export function modalButton(label: string, variant: "cancel" | "confirm" | "delete" = "cancel") {
  const button = document.createElement("button");
  button.type = "button";
  button.className = `image-dialog-btn ${variant}`;
  button.textContent = label;
  return button;
}

export function field(labelText: string, input: HTMLInputElement) {
  const label = document.createElement("label");
  label.className = "modal-field";
  const caption = document.createElement("span");
  caption.className = "image-dialog-label";
  caption.textContent = labelText;
  input.classList.add("image-dialog-input");
  label.append(caption, input);
  return label;
}

export function showModalError(handle: ModalHandle, message: string) {
  handle.error.textContent = message;
  handle.error.hidden = false;
}

export function showToast(message: string, kind: "normal" | "warning" = "normal", action?: { label: string; run: () => void }) {
  const toast = document.createElement("div");
  toast.className = `app-toast ${kind}`;
  toast.setAttribute("role", "status");
  const text = document.createElement("span");
  text.textContent = message;
  toast.appendChild(text);
  if (action) {
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = action.label;
    button.addEventListener("click", () => {
      toast.remove();
      action.run();
    });
    toast.appendChild(button);
  }
  document.body.appendChild(toast);
  requestAnimationFrame(() => toast.classList.add("visible"));
  window.setTimeout(() => {
    toast.classList.remove("visible");
    window.setTimeout(() => toast.remove(), 180);
  }, action ? 10000 : 4000);
}
