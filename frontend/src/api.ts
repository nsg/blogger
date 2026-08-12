import type { ApiErrorBody } from "./types.js";

export class ApiError extends Error {
  constructor(public status: number, public body: ApiErrorBody) {
    super(body.error || `Request failed (${status})`);
  }
}

export async function api<T>(url: string, init?: RequestInit): Promise<T> {
  let response: Response;
  try {
    response = await fetch(url, init);
  } catch (error) {
    throw error instanceof Error ? error : new Error("Network request failed");
  }
  if (response.status === 401) {
    location.reload();
    throw new ApiError(401, { error: "Authentication expired" });
  }
  const body = await response.json().catch(() => ({})) as Partial<ApiErrorBody>;
  if (!response.ok) {
    throw new ApiError(response.status, {
      ...body,
      error: typeof body.error === "string" ? body.error : `Request failed (${response.status})`,
    });
  }
  return body as unknown as T;
}

export function jsonRequest(method: "POST", body?: unknown): RequestInit {
  return {
    method,
    headers: body === undefined ? undefined : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  };
}

export function mutationEvent(detail?: Record<string, unknown>) {
  window.dispatchEvent(new CustomEvent("blogger-mutation", { detail }));
}
