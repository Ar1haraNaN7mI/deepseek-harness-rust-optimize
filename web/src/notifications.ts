import { readUiPreferences } from "./uiPreferences";
import type { Envelope } from "./types";

export type HarnessNotice = { id: number; title: string; sessionId: string };
let soundContext: AudioContext | undefined;

// Audio is unlocked by a deliberate interaction; never substitute system TTS.
export function unlockNotificationAudio() {
  if (!readUiPreferences().notificationSound) return;
  try {
    soundContext ??= new AudioContext();
    void soundContext.resume().catch(() => {});
  } catch { /* Browsers without Web Audio keep visual notifications. */ }
}
function chime() {
  if (!soundContext || soundContext.state !== "running") return;
  const gain = soundContext.createGain();
  const tone = soundContext.createOscillator();
  const now = soundContext.currentTime;
  tone.frequency.setValueAtTime(660, now);
  tone.frequency.exponentialRampToValueAtTime(880, now + 0.12);
  gain.gain.setValueAtTime(0.001, now);
  gain.gain.exponentialRampToValueAtTime(0.06, now + 0.02);
  gain.gain.exponentialRampToValueAtTime(0.001, now + 0.23);
  tone.connect(gain); gain.connect(soundContext.destination);
  tone.onended = () => { tone.disconnect(); gain.disconnect(); };
  tone.start(); tone.stop(now + 0.25);
}
export function noticeForEvent(event: Envelope, finalState?: string | null): HarnessNotice | undefined {
  const preferences = readUiPreferences();
  const id = event.payload.session_id;
  if (typeof id !== "string") return;
  if (event.event_type === "agent.done" && finalState === "completed" && preferences.notifyOnCompletion)
    return { id: event.sequence, title: "DSH 任务已完成", sessionId: id };
  if (event.event_type === "agent.approval_needed" && preferences.notifyOnApproval)
    return { id: event.sequence, title: "DSH 任务需要你的授权", sessionId: id };
}
export function deliverNotice(notice: HarnessNotice) {
  const preferences = readUiPreferences();
  if (preferences.notificationSound) chime();
  if (preferences.desktopNotifications && document.hidden && "Notification" in window && Notification.permission === "granted") {
    try {
      const notification = new Notification(notice.title, {
        body: "打开本机 Harness 查看。", tag: `dsh-${notice.sessionId}`, silent: true,
      });
      notification.onclick = () => { window.focus(); notification.close(); };
    } catch { /* Some platforms require a service worker; in-app notice remains. */ }
  }
}
