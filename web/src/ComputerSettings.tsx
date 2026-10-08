import { useEffect, useRef, useState } from "react";
import { CheckIcon, CursorArrowIcon, DesktopIcon, ReloadIcon } from "@radix-ui/react-icons";
import { errorText, isAbort, rpc } from "./api";
import "./computer-settings.css";

type NativeId = string | number;
type Rect = { x: number; y: number; width: number; height: number };
type ComputerStatus = { enabled: boolean; supported: boolean; reason?: string | null; tools: string[]; model_input: string; native: boolean };
type NativeWindow = { window_id: NativeId; title: string; pid: number; rect: Rect; foreground: boolean };
type NativeNode = { node_id: NativeId; parent_id?: NativeId | null; role: string; name: string; value?: string | null; bounds?: Rect; enabled: boolean; offscreen: boolean; password: boolean; patterns: string[]; background_patterns?: string[] };
type IndependentPointer = { x: number; y: number; visible: boolean };
type Recognition = { status: "ok" | "unavailable" | "error"; language?: string | null; text: string; lines: { text: string; bounds: Rect; words: { text: string; bounds: Rect }[] }[]; error?: string | null; truncated?: boolean };
type Snapshot = {
  window_id: NativeId; snapshot_id: string; rect: Rect; nodes: NativeNode[]; truncated: boolean;
  expires_in_ms?: number;
  screenshot?: { data_url: string; width: number; height: number; mime: string } | null;
  screenshot_error?: string;
  pointer?: IndependentPointer;
  recognition?: Recognition;
  input_target?: { node_id: string; window_id: NativeId; class: string; delivery: string; bounds: Rect } | null;
};
type ActionResult = { performed: string; delivery?: "verified" | "unverified"; refresh_required?: boolean; observation_error?: string; pointer?: IndependentPointer };

const roleLabels: Record<string, string> = { Button: "按钮", Edit: "输入框", Text: "文字", Window: "窗口", MenuItem: "菜单项", CheckBox: "复选框", ComboBox: "下拉选择", ListItem: "列表项", TabItem: "标签页", Hyperlink: "链接" };
const keys: [string, string][] = [["CTRL+A", "Ctrl + A · 全选"], ["Home", "移到开头"], ["End", "移到末尾"], ["SHIFT+HOME", "选到开头"], ["SHIFT+END", "选到末尾"], ["ArrowLeft", "← · 向左"], ["ArrowRight", "→ · 向右"], ["SHIFT+LEFT", "向左选择"], ["SHIFT+RIGHT", "向右选择"], ["Backspace", "删除前一个字符"], ["Delete", "删除后一个字符"]];
const pattern = (node: NativeNode | undefined, name: string) => node?.patterns.some((value) => value.toLowerCase().replace(/pattern$/, "") === name.toLowerCase()) ?? false;
const backgroundPattern = (node: NativeNode | undefined, name: string) => node?.background_patterns?.includes(name) ?? false;
const permits = (node: NativeNode | undefined, name: string) => pattern(node, name) || backgroundPattern(node, name);

export function ComputerSettings({ token, onChanged }: { token: string; onChanged?: () => void }) {
  const [status, setStatus] = useState<ComputerStatus>();
  const [windows, setWindows] = useState<NativeWindow[]>([]);
  const [windowId, setWindowId] = useState("");
  const [snapshot, setSnapshot] = useState<Snapshot>();
  const [pointer, setPointer] = useState<IndependentPointer>();
  const [deadline, setDeadline] = useState(0);
  const [stale, setStale] = useState(true);
  const [nodeId, setNodeId] = useState("");
  const [text, setText] = useState("");
  const [key, setKey] = useState("CTRL+A");
  const [busy, setBusy] = useState("读取电脑操作状态");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const generation = useRef(0);
  const operation = useRef(false);
  const currentRequest = useRef<AbortController | undefined>(undefined);

  useEffect(() => {
    const serial = ++generation.current;
    const controller = new AbortController();
    currentRequest.current = controller;
    operation.current = true;
    setBusy("读取电脑操作状态"); setStatus(undefined); setWindows([]); setWindowId(""); setSnapshot(undefined); setPointer(undefined); setStale(true); setNodeId(""); setError("");
    void (async () => {
      try {
        const result = await rpc<ComputerStatus>(token, "computer/status", {}, controller.signal);
        if (controller.signal.aborted || serial !== generation.current) return;
        setStatus(result);
        if (result.enabled && result.supported) {
          const list = await rpc<{ windows: NativeWindow[] }>(token, "computer/windows", {}, controller.signal);
          if (!controller.signal.aborted && serial === generation.current) setWindows(list.windows);
        }
      } catch (failure) {
        if (!controller.signal.aborted && serial === generation.current && !isAbort(failure)) setError(errorText(failure));
      } finally {
        if (serial === generation.current) { operation.current = false; setBusy(""); }
      }
    })();
    return () => { ++generation.current; controller.abort(); currentRequest.current?.abort(); operation.current = false; };
  }, [token]);

  useEffect(() => {
    if (!snapshot || stale) return;
    const timer = setTimeout(() => setStale(true), Math.max(0, deadline - Date.now()));
    return () => clearTimeout(timer);
  }, [snapshot, deadline, stale]);

  async function run(label: string, work: (signal: AbortSignal) => Promise<void>) {
    if (operation.current) return;
    const serial = generation.current;
    const controller = new AbortController();
    currentRequest.current = controller;
    operation.current = true;
    setBusy(label); setError(""); setNotice("");
    try { await work(controller.signal); }
    catch (failure) {
      if (!controller.signal.aborted && serial === generation.current && !isAbort(failure)) setError(errorText(failure));
    } finally {
      if (serial === generation.current) { operation.current = false; setBusy(""); }
    }
  }
  const selectedWindow = windows.find((item) => String(item.window_id) === windowId);
  const active = !!status?.enabled && !!status.supported;
  const current = active && !!snapshot && String(snapshot.window_id) === windowId && !stale;
  const inputTarget = snapshot?.input_target && String(snapshot.input_target.window_id) === windowId && snapshot.input_target.node_id === "background" ? snapshot.input_target : undefined;
  const node: NativeNode | undefined = nodeId === "background" && inputTarget ? { node_id: "background", role: "Window", name: "画面中已选位置", enabled: true, offscreen: false, password: false, patterns: [], background_patterns: ["type_text", "key", "scroll"] } : snapshot?.nodes.find((item) => String(item.node_id) === nodeId);
  const actionable = current && !!node && node.enabled && !node.offscreen;
  const canInvoke = actionable && pattern(node, "Invoke");
  const canSetValue = actionable && !node?.password && (pattern(node, "Value") || pattern(node, "set_value"));
  const canTypeText = actionable && !node?.password && permits(node, "type_text");
  const canKey = actionable && !node?.password && permits(node, "key");
  const canScroll = actionable && !node?.password && permits(node, "scroll");
  const unverifiedTarget = !!node && !node.password && !!node.background_patterns?.length && !node.patterns.length;
  const keySupported = pattern(node, "key") || !key.includes("+");
  const blocked = !!busy;

  async function readSnapshot(window: NativeWindow, signal: AbortSignal) {
    const started = Date.now();
    const result = await rpc<Snapshot>(token, "computer/observe", { window_id: window.window_id }, signal);
    if (signal.aborted) return;
    if (String(result.window_id) !== String(window.window_id) || !result.snapshot_id || !Array.isArray(result.nodes)) throw new Error("窗口画面未能匹配，请重新读取。");
    setSnapshot(result); setPointer(result.pointer); setNodeId("");
    const expiry = started + Math.max(0, Math.min(result.expires_in_ms ?? 60000, 60000) - 5000);
    setDeadline(expiry); setStale(Date.now() >= expiry);
  }
  async function act(action: string, extra: Record<string, unknown> = {}) {
    if (!selectedWindow || !snapshot || !current || blocked) return;
    if (node?.password && (action === "type_text" || action === "set_value")) return;
    if (["type_text", "key", "scroll"].includes(action) && (!actionable || !node || !permits(node, action) || node.password)) return;
    if (action === "key" && !keySupported) return;
    const captured = snapshot;
    const target = selectedWindow;
    await run("执行窗口操作", async (signal) => {
      // Any operation consumes this observation, including unsuccessful attempts.
      setStale(true); setNodeId("");
      const result = await rpc<ActionResult>(token, "computer/act", { window_id: target.window_id, snapshot_id: captured.snapshot_id, action, ...extra }, signal);
      if (signal.aborted) return;
      const unverified = result?.delivery === "unverified" && result.performed === "background_messages_sent";
      const selected = action === "click" && result?.performed === "select_control";
      if (!unverified && !selected && result?.performed !== action && !(action === "click" && result?.performed === "invoke")) throw new Error("未能确认这次窗口操作的执行结果，请刷新画面核对。");
      setNotice(unverified ? "后台操作已发送，正在读取画面核对。" : selected ? "已选择后台输入位置，正在读取新的窗口画面。" : "操作已完成，正在读取新的窗口画面。");
      if (action === "set_value" || action === "type_text") setText("");
      const reconcileTarget = async () => {
        const list = await rpc<{ windows: NativeWindow[] }>(token, "computer/windows", {}, signal);
        if (signal.aborted) return false;
        setWindows(list.windows);
        if (list.windows.some((item) => String(item.window_id) === String(target.window_id))) return true;
        setSnapshot(undefined); setPointer(undefined); setWindowId(""); setStale(true);
        setError(""); setNotice(unverified ? "后台操作已发送，目标窗口现已关闭或不再可见。" : "操作已完成，目标窗口已关闭或不再可见。");
        return false;
      };
      try {
        if (result.refresh_required && !await reconcileTarget()) return;
        await readSnapshot(target, signal);
        if (!signal.aborted) setNotice(unverified ? "后台操作已发送，画面已更新；请核对目标应用是否响应。" : selected ? "已选择后台输入位置，窗口画面已更新。" : "操作已完成，窗口画面已更新。");
      } catch (failure) {
        if (signal.aborted) return;
        setSnapshot(undefined); setPointer(undefined); setStale(true);
        // A close action may succeed just before our follow-up observation.
        // Preserve that success and remove the obsolete target if it vanished.
        try { if (!await reconcileTarget()) return; } catch { /* Report the original observation failure. */ }
        if (!signal.aborted) {
          setNotice("");
          setError(`${unverified ? "后台操作已发送" : "操作已执行"}，但读取新画面失败：${errorText(failure)}。请刷新窗口后重新读取。`);
        }
      }
    });
  }
  const screenshot = snapshot?.screenshot?.data_url;
  const safeScreenshot = typeof screenshot === "string" && /^data:image\/(png|jpeg|webp);base64,[A-Za-z0-9+/=\s]+$/.test(screenshot) ? screenshot : undefined;
  const visiblePointer = pointer?.visible && Number.isFinite(pointer.x) && Number.isFinite(pointer.y) && snapshot && snapshot.rect.width > 0 && snapshot.rect.height > 0;
  const readable = snapshot?.nodes.map((item) => `${item.name || "（未命名）"} · ${roleLabels[item.role] || item.role}${item.password ? " · 密码输入框" : item.value ? `\n  ${item.value}` : ""}${!item.enabled ? " · 已禁用" : ""}${item.offscreen ? " · 当前不可见" : ""}`).join("\n") || "当前窗口未提供可读取的控件。";

  return <section className="sc-section cu-settings" aria-label="DSH 电脑操作">
    <p className="sc-description">DSH 使用桌面上的独立指针在后台操作，不移动你的鼠标。本机文字识别与窗口控件一起帮助模型理解画面。标准 Windows 控件可直接操作，其他应用的后台响应以更新后的画面为准。</p>
    <div className="sc-row">
      <div className="sc-row-copy"><span className="sc-row-title">启用电脑操作</span><p>启用后，当前 DSH 模型可以使用同一组本机电脑工具。</p></div>
      <button type="button" role="switch" className="sc-switch" aria-label="启用电脑操作" aria-checked={status?.enabled ?? false} disabled={blocked || !status || (!status.supported && !status.enabled)} onClick={() => void run("保存电脑操作设置", async (signal) => {
        const result = await rpc<ComputerStatus>(token, "computer/setEnabled", { enabled: !status?.enabled }, signal);
        if (signal.aborted) return;
        setStatus(result); setSnapshot(undefined); setPointer(undefined); setNodeId(""); setStale(true); setWindowId(""); setWindows([]);
        if (result.enabled && result.supported) {
          const list = await rpc<{ windows: NativeWindow[] }>(token, "computer/windows", {}, signal);
          if (!signal.aborted) setWindows(list.windows);
        }
        if (!signal.aborted) { setNotice(result.enabled ? "电脑操作已启用，模型工具已接通。" : "电脑操作已关闭。"); onChanged?.(); }
      })}><span /></button>
    </div>
    <div className={`cu-status-line ${active ? "is-ready" : ""}`}><span />{!status ? "正在读取本机支持状态…" : !status.supported ? status.reason || "当前系统不支持本机电脑操作。" : active ? "Windows 本机窗口 · 已启用" : "Windows 本机窗口 · 已关闭"}</div>
    {status && <div className="cu-tool-state">模型可用电脑工具：{active ? status.tools.length : 0} 个{active && status.tools.length > 0 && <details><summary>查看工具</summary><code>{status.tools.join(" · ")}</code></details>}</div>}
    {error && <p className="cu-error" role="alert">{error}</p>}
    {notice && <p className="cu-notice" role="status">{notice}</p>}
    {busy && <p className="sc-description" role="status">{busy}…</p>}
    {!status && error && <button className="sc-button" disabled={blocked} onClick={() => void run("读取电脑操作状态", async (signal) => {
      const result = await rpc<ComputerStatus>(token, "computer/status", {}, signal);
      if (signal.aborted) return;
      setStatus(result);
      if (result.enabled && result.supported) {
        const list = await rpc<{ windows: NativeWindow[] }>(token, "computer/windows", {}, signal);
        if (!signal.aborted) setWindows(list.windows);
      }
    })}>重新连接电脑服务</button>}
    {active && <>
      <div className="cu-toolbar">
        <label className="sc-field">目标窗口<select className="sc-select" value={windowId} disabled={blocked} onChange={(event) => { setWindowId(event.target.value); setSnapshot(undefined); setPointer(undefined); setNodeId(""); setStale(true); setError(""); setNotice(""); }}><option value="">选择一个正在运行的窗口</option>{windows.map((item) => <option key={String(item.window_id)} value={String(item.window_id)}>{item.title || "未命名窗口"}{item.foreground ? " · 当前前台" : ""}</option>)}</select></label>
        <button className="sc-button" disabled={blocked} onClick={() => void run("刷新窗口列表", async (signal) => {
          setSnapshot(undefined); setPointer(undefined); setNodeId(""); setStale(true);
          const result = await rpc<{ windows: NativeWindow[] }>(token, "computer/windows", {}, signal);
          if (signal.aborted) return;
          setWindows(result.windows);
          if (!result.windows.some((item) => String(item.window_id) === windowId)) setWindowId("");
        })}><ReloadIcon />刷新窗口</button>
      </div>
      {!windows.length && !busy && <p className="sc-description">没有可读取的窗口。打开一个应用后刷新窗口列表。</p>}
      {selectedWindow && <>
        <p className="cu-window-note">{selectedWindow.title} · {selectedWindow.rect.width} × {selectedWindow.rect.height}</p>
        <button className="sc-button sc-primary" disabled={blocked} onClick={() => void run("读取窗口画面", async (signal) => { setStale(true); setNodeId(""); await readSnapshot(selectedWindow, signal); })}><DesktopIcon />{snapshot ? "刷新画面与控件" : "读取画面与控件"}</button>
        <figure className="cu-preview">
          {safeScreenshot ? <div className="cu-image-surface" style={{ maxWidth: snapshot && snapshot.rect.height > 0 ? `${340 * snapshot.rect.width / snapshot.rect.height}px` : undefined }}>
            <img src={safeScreenshot} alt={`${selectedWindow.title} 当前窗口画面`} className={current && !blocked ? "cu-interactive-image" : undefined} onClick={(event) => {
              if (!current || blocked || !snapshot) return;
              const bounds = event.currentTarget.getBoundingClientRect();
              if (bounds.width <= 0 || bounds.height <= 0) return;
              const x = Math.max(0, Math.min(snapshot.rect.width - 1, Math.floor((event.clientX - bounds.left) / bounds.width * snapshot.rect.width)));
              const y = Math.max(0, Math.min(snapshot.rect.height - 1, Math.floor((event.clientY - bounds.top) / bounds.height * snapshot.rect.height)));
              void act("click", { x, y, button: "left" });
            }} />
            {visiblePointer && <span className="cu-independent-pointer" role="img" aria-label={`DSH 独立指针：${Math.round(pointer!.x)}, ${Math.round(pointer!.y)}`} style={{ left: `${Math.max(0, Math.min(pointer!.x, snapshot!.rect.width - 1)) / snapshot!.rect.width * 100}%`, top: `${Math.max(0, Math.min(pointer!.y, snapshot!.rect.height - 1)) / snapshot!.rect.height * 100}%` }}>
              <svg viewBox="0 0 24 30" aria-hidden="true"><path d="M2 2 3 23 9 18 15 28 20 25 14 16 23 15Z" /></svg><b>DSH</b>
            </span>}
          </div> : <div className="cu-preview-empty">{snapshot ? snapshot.screenshot_error || "当前窗口未返回画面预览，仍可查看下方可读控件。" : "读取后显示窗口画面与可操作控件。"}</div>}
          {snapshot && <figcaption><strong>{snapshot.nodes.length} 个可读控件{snapshot.truncated ? " · 已截取部分内容" : ""}</strong><span className={stale ? "cu-stale" : ""}>{stale ? "请刷新画面后操作" : "画面已读取"}</span></figcaption>}
        </figure>
        {snapshot && <>
          {safeScreenshot && <p className="sc-description">点击画面可点击控件或选择后台输入位置。操作后独立指针同步显示在桌面与这里；画面过期后需先刷新。</p>}
          {snapshot.recognition && <details className="cu-reading cu-recognition" open><summary>本机识别文字</summary>
            {snapshot.recognition.status === "ok" ? <>
              <p className="sc-description">{snapshot.recognition.language || "系统识别语言"} · 模型可读取这些文字与位置{snapshot.recognition.truncated ? " · 已截取部分内容" : ""}</p>
              <pre>{snapshot.recognition.text || "当前画面未识别到文字。"}</pre>
              {!!snapshot.recognition.lines?.length && <details><summary>文字位置</summary><pre>{snapshot.recognition.lines.map((line) => `${line.text}\n  ${Math.round(line.bounds.x)}, ${Math.round(line.bounds.y)} · ${Math.round(line.bounds.width)} × ${Math.round(line.bounds.height)}`).join("\n")}</pre></details>}
            </> : <p className="sc-description">{snapshot.recognition.error || "本机文字识别暂不可用，仍可读取窗口控件。"}</p>}
          </details>}
          <details className="cu-reading"><summary>窗口文字与控件</summary><pre>{readable}</pre></details>
          <div className="cu-controls">
            <h3>后台操作所选控件</h3>
            <p className="sc-description">选择具体控件后操作，完成后自动刷新画面。可尝试向其他应用发送后台操作，应用是否接受需要核对；不会切换到你的鼠标或键盘。</p>
            <label className="sc-field">目标控件<select className="sc-select" value={nodeId} disabled={blocked || !current} onChange={(event) => { setNodeId(event.target.value); setText(""); }}><option value="">选择画面中的控件</option>{inputTarget && <option value="background">画面中已选位置 · {inputTarget.class}</option>}{snapshot.nodes.filter((item) => !item.offscreen).map((item) => <option key={String(item.node_id)} value={String(item.node_id)} disabled={!item.enabled}>{item.name || "未命名控件"} · {roleLabels[item.role] || item.role}</option>)}</select></label>
            {node && <div className="cu-control-meta"><span>{roleLabels[node.role] || node.role}</span>{node.password && <span>密码输入框</span>}{canInvoke && <span>可点击</span>}{canSetValue && <span>可填写</span>}{canKey && <span>后台按键</span>}{canScroll && <span>后台滚动</span>}</div>}
            {unverifiedTarget && <p className="sc-description">此位置使用应用的后台输入通道。发送后请查看实际变化；组合键仅适用于支持直接操作的文本框。</p>}
            <button className="sc-button" disabled={blocked || !canInvoke} onClick={() => void act("invoke", { node_id: node!.node_id })}><CursorArrowIcon />点击所选控件</button>
            <label className="sc-field">填写内容{node?.password ? <input className="sc-input" type="password" autoComplete="off" value="" disabled /> : <textarea className="sc-textarea" value={text} onChange={(event) => setText(event.target.value)} rows={3} maxLength={4000} disabled={blocked || !current} placeholder="输入要填写的文字" />}</label>
            {node?.password && <p className="sc-description">密码请直接在目标窗口中手动输入。</p>}
            <div className="sc-actions"><button className="sc-button" disabled={blocked || !canSetValue || !text} onClick={() => void act("set_value", { node_id: node!.node_id, text })}><CheckIcon />填写所选控件</button><button className="sc-button" disabled={blocked || !canTypeText || !text} onClick={() => void act("type_text", { node_id: node!.node_id, text })}>后台键入所选控件</button></div>
            <div className="cu-control-group"><label className="sc-field">按键<select className="sc-select" value={key} onChange={(event) => setKey(event.target.value)} disabled={blocked || !canKey}>{keys.map(([value, label]) => <option key={value} value={value} disabled={!pattern(node, "key") && value.includes("+")}>{label}</option>)}</select></label><button className="sc-button" disabled={blocked || !canKey || !keySupported} onClick={() => void act("key", { node_id: node!.node_id, key })}>发送按键</button></div>
            <div className="cu-commands"><button className="sc-button" disabled={blocked || !canScroll} onClick={() => void act("scroll", { node_id: node!.node_id, delta: 3 })}>向上滚动</button><button className="sc-button" disabled={blocked || !canScroll} onClick={() => void act("scroll", { node_id: node!.node_id, delta: -3 })}>向下滚动</button></div>
          </div>
        </>}
      </>}
    </>}
  </section>;
}
