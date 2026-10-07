import { useEffect, useSyncExternalStore } from "react";

export type UiPreferences = {
  theme: "system" | "light" | "dark";
  accent: string;
  density: "comfortable" | "compact";
  fontSize: "small" | "medium" | "large";
  sendKey: "enter" | "mod-enter";
  reducedMotion: "system" | "reduce";
  notifyOnCompletion: boolean;
  notifyOnApproval: boolean;
  notificationSound: boolean;
  desktopNotifications: boolean;
  showThinking: boolean;
  showToolDetails: boolean;
  pet: "none" | "cat";
};

export const defaultUiPreferences: UiPreferences = {
  theme: "system", accent: "#3679af", density: "comfortable",
  fontSize: "medium", sendKey: "enter", reducedMotion: "system",
  notifyOnCompletion: true, notifyOnApproval: true,
  notificationSound: false, desktopNotifications: false,
  showThinking: false, showToolDetails: false, pet: "none",
};
const key = "dsh.web.interface.v1";
const listeners = new Set<() => void>();
let snapshot: UiPreferences | undefined;

export function sanitizeUiPreferences(input: unknown): UiPreferences {
  const result = { ...defaultUiPreferences };
  if (!input || typeof input !== "object") return result;
  const value = input as Record<string, unknown>;
  const choices = {
    theme: ["system", "light", "dark"], density: ["comfortable", "compact"],
    fontSize: ["small", "medium", "large"], sendKey: ["enter", "mod-enter"],
    reducedMotion: ["system", "reduce"], pet: ["none", "cat"],
  };
  for (const [name, options] of Object.entries(choices)) {
    if (options.includes(value[name] as string)) Object.assign(result, { [name]: value[name] });
  }
  for (const name of ["notifyOnCompletion", "notifyOnApproval", "notificationSound", "desktopNotifications", "showThinking", "showToolDetails"] as const)
    if (typeof value[name] === "boolean") result[name] = value[name];
  if (typeof value.accent === "string" && /^#[\da-f]{6}$/i.test(value.accent)) result.accent = value.accent;
  return result;
}
export function readUiPreferences(): UiPreferences {
  if (!snapshot) {
    try { snapshot = sanitizeUiPreferences(JSON.parse(localStorage.getItem(key) || "{}")); }
    catch { snapshot = { ...defaultUiPreferences }; }
  }
  return snapshot;
}
function emit() { for (const listener of listeners) listener(); }
export function updateUiPreferences(patch: Partial<UiPreferences>) {
  const next = sanitizeUiPreferences({ ...readUiPreferences(), ...patch });
  // Storage failure must reach the form; never show a saved state without a write.
  localStorage.setItem(key, JSON.stringify(next));
  snapshot = next;
  emit();
}
export function resetUiPreferences() {
  localStorage.removeItem(key);
  snapshot = { ...defaultUiPreferences };
  emit();
}
function subscribe(listener: () => void) {
  listeners.add(listener);
  if (listeners.size === 1) window.addEventListener("storage", handleStorage);
  return () => {
    listeners.delete(listener);
    if (!listeners.size) window.removeEventListener("storage", handleStorage);
  };
}
function handleStorage(event: StorageEvent) {
  if (event.key === key || event.key === null) { snapshot = undefined; emit(); }
}
export function useUiPreferences(): [UiPreferences, (patch: Partial<UiPreferences>) => void] {
  return [useSyncExternalStore(subscribe, readUiPreferences, () => defaultUiPreferences), updateUiPreferences];
}
export function useInterfaceEffects() {
  const [preferences] = useUiPreferences();
  useEffect(() => {
    const system = window.matchMedia("(prefers-color-scheme: dark)");
    const root = document.documentElement;
    const apply = () => {
      root.dataset.theme = preferences.theme === "system" ? (system.matches ? "dark" : "light") : preferences.theme;
      root.dataset.density = preferences.density;
      root.dataset.fontSize = preferences.fontSize;
      root.dataset.motion = preferences.reducedMotion;
      root.style.setProperty("--blue", preferences.accent);
    };
    apply();
    system.addEventListener("change", apply);
    return () => system.removeEventListener("change", apply);
  }, [preferences]);
}

export function shouldSend(event: { key: string; shiftKey: boolean; ctrlKey: boolean; metaKey: boolean; altKey: boolean; isComposing: boolean }, mode: UiPreferences["sendKey"]) {
  return event.key === "Enter" && !event.shiftKey && !event.altKey && !event.isComposing &&
    (mode === "enter" ? !event.ctrlKey && !event.metaKey : event.ctrlKey || event.metaKey);
}
