import { useEffect, useRef, useState, type FormEvent } from "react";
import { LockClosedIcon, LockOpen1Icon } from "@radix-ui/react-icons";
import { accessStatus, errorText, isAbort, post } from "./api";
import type { AccessStatus } from "./types";

export function AccessPasswordSettings({ token }: { token: string }) {
  const [status, setStatus] = useState<AccessStatus>();
  const [currentPassword, setCurrentPassword] = useState("");
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);
  const alive = useRef(true);
  const operation = useRef(false);
  useEffect(() => {
    const abort = new AbortController();
    alive.current = true;
    accessStatus(abort.signal).then((result) => {
      if (!abort.signal.aborted) setStatus(result);
    }).catch((failure) => {
      if (!abort.signal.aborted && !isAbort(failure)) setError(errorText(failure));
    });
    return () => { alive.current = false; abort.abort(); };
  }, [token]);

  async function save(remove = false) {
    if (!status || operation.current) return;
    setError("");
    setNotice("");
    if (status.enabled && !currentPassword) {
      setError("请先输入当前密码。");
      return;
    }
    if (!remove && (Array.from(password).length < 8 || Array.from(password).length > 128)) {
      setError("访问密码需要 8–128 个字符。");
      return;
    }
    if (!remove && password !== confirmation) {
      setError("两次输入的新密码不一致。");
      return;
    }
    operation.current = true;
    setBusy(true);
    try {
      const result = await post<AccessStatus>("/api/access/password", token, {
        ...(status.enabled ? { current_password: currentPassword } : {}),
        password: remove ? "" : password,
      });
      if (!alive.current) return;
      setStatus(result);
      setCurrentPassword("");
      setPassword("");
      setConfirmation("");
      setNotice(result.enabled ? "访问密码已保存。当前窗口保持解锁，新的访问会话需要验证。" : "访问密码已移除。");
    } catch (failure) {
      if (alive.current) setError(errorText(failure));
    } finally {
      operation.current = false;
      if (alive.current) setBusy(false);
    }
  }
  async function lock() {
    if (operation.current) return;
    operation.current = true;
    setBusy(true);
    setError("");
    try {
      await post<AccessStatus>("/api/access/lock", token, {});
      window.dispatchEvent(new Event("dsh-access-locked"));
    } catch (failure) {
      if (alive.current) setError(errorText(failure));
    } finally {
      operation.current = false;
      if (alive.current) setBusy(false);
    }
  }
  return (
    <section className="sc-section sc-access-settings" aria-label="访问密码设置">
      <div className="sc-access-heading">
        <div><span className="eyebrow">ACCESS CONTROL / 03</span><h3>访问密码</h3></div>
        <span className={`sc-access-state ${status?.enabled ? "enabled" : ""}`}>{status?.enabled ? <LockClosedIcon /> : <LockOpen1Icon />}{status ? status.enabled ? "已启用" : "未设置" : "读取中"}</span>
      </div>
      <p className="sc-description">网页与桌面端共用访问密码；终端启动不需要验证。界面不会读取或显示已保存的密码。</p>
      <form onSubmit={(event: FormEvent) => { event.preventDefault(); void save(); }}>
        {status?.enabled && <label className="sc-field">当前密码<input className="sc-input" type="password" autoComplete="current-password" value={currentPassword} onChange={(event) => setCurrentPassword(event.target.value)} maxLength={128} disabled={busy} /></label>}
        <label className="sc-field">{status?.enabled ? "新密码" : "设置密码"}<input className="sc-input" type="password" autoComplete="new-password" value={password} onChange={(event) => setPassword(event.target.value)} maxLength={128} placeholder="8–128 个字符" disabled={busy || !status} /></label>
        <label className="sc-field">确认新密码<input className="sc-input" type="password" autoComplete="new-password" value={confirmation} onChange={(event) => setConfirmation(event.target.value)} maxLength={128} disabled={busy || !status} /></label>
        <div className="sc-form-actions sc-access-actions">
          <button className="sc-button sc-primary" type="submit" disabled={busy || !status}>{busy ? "正在处理…" : status?.enabled ? "更新密码" : "启用密码"}</button>
          {status?.enabled && <button className="sc-button sc-danger" type="button" disabled={busy} onClick={() => void save(true)}>移除密码</button>}
          {status?.enabled && <button className="sc-button" type="button" disabled={busy} onClick={() => void lock()}>立即锁定</button>}
        </div>
      </form>
      {error && <p className="sc-access-error" role="alert">{error}</p>}
      {notice && <p className="sc-access-notice" role="status">{notice}</p>}
    </section>
  );
}
