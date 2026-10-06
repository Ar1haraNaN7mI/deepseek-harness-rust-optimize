export type WebPreferences = { enabled?: boolean; next: boolean };
const key = "dsh.web.startup.v1";
export function readPreferences(): WebPreferences {
  try {
    const value = JSON.parse(localStorage.getItem(key) || "{}");
    return {
      enabled: typeof value.enabled === "boolean" ? value.enabled : undefined,
      next: value.next === true,
    };
  } catch {
    return { next: false };
  }
}
export function savePreferences(value: WebPreferences) {
  localStorage.setItem(key, JSON.stringify(value));
}
let initialPlayback: boolean | undefined;
export function playbackDecision(
  preference: WebPreferences,
  serverEnabled: boolean,
  override?: boolean | null,
) {
  return override ?? (preference.next || (preference.enabled ?? serverEnabled));
}
export function consumeInitialPlayback(
  serverEnabled = false,
  override?: boolean | null,
) {
  if (typeof window === "undefined") return false;
  if (initialPlayback !== undefined) return initialPlayback;
  const preference = readPreferences();
  initialPlayback = playbackDecision(preference, serverEnabled, override);
  if (preference.next) {
    try {
      savePreferences({ ...preference, next: false });
    } catch {
      /* A blocked store still allows this visit. */
    }
  }
  return initialPlayback;
}
