import { useState, type FormEvent } from "react";
import { ArrowRightIcon, LockClosedIcon } from "@radix-ui/react-icons";
import { Emblem } from "./Emblem";
import { errorText, post } from "./api";
import type { AccessStatus } from "./types";

export function AccessEntry({ status, onUnlocked, connectionError, checking = false, onRetry }: {
  status: AccessStatus;
  onUnlocked: () => Promise<void>;
  connectionError?: string;
  checking?: boolean;
  onRetry?: () => Promise<void>;
}) {
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  async function unlock(event: FormEvent) {
    event.preventDefault();
    if (busy || !password) return;
    setBusy(true);
    setError("");
    try {
      const result = await post<AccessStatus>("/api/access/unlock", status.token, { password });
      if (!result.unlocked) throw new Error("访问尚未解锁，请重新输入密码。");
      setPassword("");
      // The parent re-reads the backend rather than trusting this UI result.
      await onUnlocked();
    } catch (failure) {
      setError(errorText(failure));
    } finally {
      setBusy(false);
    }
  }
  return (
    <main className="access-entry">
      <span className="access-coordinate">DSH / ACCESS CONTROL</span>
      <div className="access-entry-frame">
        <div className="access-entry-brand"><Emblem size={76} /><span>DEEPSEEK HARNESS<small>OPERATOR VERIFICATION</small></span></div>
        <h1>访问身份确认</h1>
        <div className="access-entry-record">
          <div><span>访问身份</span><strong>{status.profile.username}</strong></div>
          <div><span>档案编号</span><strong>{status.profile.badge_id}</strong></div>
        </div>
        <form onSubmit={(event) => void unlock(event)}>
          <label htmlFor="access-entry-password"><LockClosedIcon />输入密码</label>
          <input id="access-entry-password" type="password" autoComplete="current-password" autoFocus required maxLength={128} value={password} onChange={(event) => setPassword(event.target.value)} disabled={busy} aria-describedby="access-entry-hint" />
          <p id="access-entry-hint">验证后进入本机工作台。</p>
          {error && <p className="access-entry-error" role="alert">{error}</p>}
          <button className="access-entry-submit" disabled={busy || !password}>{busy ? "正在验证…" : "验证并进入"}<ArrowRightIcon /></button>
        </form>
        {connectionError && <div className="access-entry-reconnect">
          <p className="access-entry-error" role="alert">{connectionError}</p>
          <button type="button" disabled={busy || checking} onClick={() => void onRetry?.()}>{checking ? "正在重新检查…" : "重新检查访问状态"}</button>
        </div>}
        <div className="access-entry-footer"><span>LOCAL PROFILE / ACCESS VERIFICATION</span><span>03</span></div>
      </div>
    </main>
  );
}
