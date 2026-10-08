import type { AccessStatus, Bootstrap } from "./types";

export class ApiError extends Error {
  constructor(
    message: string,
    public code?: number | string,
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
  const error = value && typeof value === "object" ? (
    value as { error?: string | { message?: string; code?: number | string } }
  ).error : undefined;
  if (!response.ok || error) {
    if (error && typeof error === "object" && error.code === "access_locked")
      window.dispatchEvent(new Event("dsh-access-locked"));
    throw new ApiError(
      typeof error === "string"
        ? error
        : error?.message || `请求失败 (${response.status})`,
      error && typeof error === "object" ? error.code : response.status,
    );
  }
  return value as T;
}
export const bootstrap = (signal?: AbortSignal) =>
  request<Bootstrap>("/api/harness/bootstrap", { signal });
const isRecord = (value: unknown): value is Record<string, unknown> =>
  !!value && typeof value === "object" && !Array.isArray(value);

export async function accessStatus(signal?: AbortSignal): Promise<AccessStatus> {
  const value = await request<unknown>("/api/access", { signal });
  if (!isRecord(value) ||
      typeof value.enabled !== "boolean" || typeof value.unlocked !== "boolean" ||
      typeof value.token !== "string" || !value.token ||
      !isRecord(value.profile) || typeof value.profile.username !== "string" || typeof value.profile.badge_id !== "string" ||
      !isRecord(value.startup) || typeof value.startup.enabled !== "boolean" ||
      typeof value.startup.reduced_motion !== "boolean" || typeof value.startup.sound !== "boolean" ||
      !(value.startup.override_enabled == null || typeof value.startup.override_enabled === "boolean")) {
    throw new ApiError("本地服务返回了无效的访问状态，请重新连接。");
  }
  return value as AccessStatus;
}
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
