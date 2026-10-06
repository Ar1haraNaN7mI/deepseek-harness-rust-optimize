import type { Bootstrap } from "./types";

export class ApiError extends Error {
  constructor(
    message: string,
    public code?: number,
  ) {
    super(message);
    this.name = "ApiError";
  }
}
export async function request<T>(
  path: string,
  options: RequestInit = {},
): Promise<T> {
  const response = await fetch(path, {
    ...options,
    credentials: "same-origin",
  });
  let value: unknown;
  try {
    value = await response.json();
  } catch {
    throw new ApiError(`本地服务返回了无法读取的响应 (${response.status})`);
  }
  const error = (
    value as { error?: string | { message?: string; code?: number } }
  ).error;
  if (!response.ok || error) {
    throw new ApiError(
      typeof error === "string"
        ? error
        : error?.message || `请求失败 (${response.status})`,
      typeof error === "object" ? error.code : response.status,
    );
  }
  return value as T;
}
export const bootstrap = (signal?: AbortSignal) =>
  request<Bootstrap>("/api/harness/bootstrap", { signal });
export function post<T>(
  path: string,
  token: string,
  body: unknown,
  signal?: AbortSignal,
) {
  return request<T>(path, {
    method: "POST",
    signal,
    headers: { "Content-Type": "application/json", "X-DSH-Token": token },
    body: JSON.stringify(body),
  });
}
export async function rpc<T>(
  token: string,
  method: string,
  params: Record<string, unknown> = {},
  signal?: AbortSignal,
): Promise<T> {
  return (
    await post<{ result: T }>(
      "/api/harness/rpc",
      token,
      { method, params },
      signal,
    )
  ).result;
}
export const errorText = (error: unknown) =>
  error instanceof Error ? error.message : String(error);
export const isAbort = (error: unknown) =>
  error instanceof DOMException && error.name === "AbortError";
