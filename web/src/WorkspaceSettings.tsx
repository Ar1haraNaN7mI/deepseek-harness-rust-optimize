import { useCallback, useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import { errorText, isAbort, rpc } from "./api";
import type { SessionResult } from "./types";
import "./workspace-settings.css";

type Section = "workspace" | "tasks" | "review";
type Props = { token: string; section: Section; onChanged?: () => void };
type Workspace = {
  workspace: string;
  outer_home: string;
  workspace_outer: string;
  git: { available: boolean; root?: string; branch?: string; status?: string; error?: string };
  permissions: string;
  sandbox: string;
  approval: string;
};
type Task = {
  id: string;
  session_id: string | null;
  goal: { outcome: string };
  state: string;
  updated_at: string;
};
type PreparedReview = { label: string; diff: string; status: string; prompt: string };
type Accepted = { accepted: boolean; session_id: string; task_id: string | null };
type Artifact = { id: string; source: string; patch: string; workspace: string; sha256: string };
type PatchPreview = { artifact_id: string; dry_run: boolean; files: string[]; hunks: number; operations: string[]; report?: string };

const states: Record<string, string> = {
  queued: "排队中", running: "执行中", waiting_approval: "等待审批", waiting_event: "等待事件",
  paused: "已暂停", completed: "已完成", failed: "失败", cancelled: "已取消",
};
const terminal = new Set(["completed", "cancelled"]);
const active = new Set(["queued", "running", "waiting_approval", "waiting_event"]);

function useOperation() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const alive = useRef(true);
  useEffect(() => { alive.current = true; return () => { alive.current = false; }; }, []);
  const run = async (work: () => Promise<void>) => {
    setBusy(true); setError(""); setNotice("");
    try { await work(); }
    catch (failure) { if (alive.current && !isAbort(failure)) setError(errorText(failure)); }
    finally { if (alive.current) setBusy(false); }
  };
  return { busy, error, notice, setError, setNotice, run };
}

function Feedback({ error, notice }: { error: string; notice: string }) {
  return <>{error && <p className="sc-info sc-danger" role="alert">{error}</p>}{notice && <p className="sc-info" role="status">{notice}</p>}</>;
}
function ValueRow({ title, value }: { title: string; value: string }) {
  return <div className="sc-row"><span className="sc-row-title">{title}</span><code className="sc-path">{value}</code></div>;
}

export function WorkspaceSettings({ token, section, onChanged }: Props) {
  if (section === "workspace") return <WorkspacePanel token={token} />;
  if (section === "review") return <ReviewPanel token={token} onChanged={onChanged} />;
  return <TasksPanel token={token} onChanged={onChanged} />;
}

function WorkspacePanel({ token }: { token: string }) {
  const op = useOperation();
  const [workspace, setWorkspace] = useState<Workspace | null>(null);
  const [artifacts, setArtifacts] = useState<Artifact[]>([]);
  const [patch, setPatch] = useState("");
  const [source, setSource] = useState("browser.patch");
  const [preview, setPreview] = useState<PatchPreview | null>(null);
  const [confirmApply, setConfirmApply] = useState(false);
  const load = useCallback(async (signal?: AbortSignal) => {
    const [state, result] = await Promise.all([
      rpc<Workspace>(token, "workspace/get", {}, signal),
      rpc<{ artifacts: Artifact[] }>(token, "cloud/list", {}, signal),
    ]);
    if (!signal?.aborted) { setWorkspace(state); setArtifacts(result.artifacts); }
  }, [token]);
  useEffect(() => {
    const controller = new AbortController();
    void load(controller.signal).catch(error => { if (!isAbort(error)) op.setError(errorText(error)); });
    return () => controller.abort();
  }, [load]);
  return <>
    <section className="sc-section">
      <h3>当前 DSH 工作区</h3>
      <p className="sc-description">网页与 CLI 使用同一个本机运行时。这里显示实际目录、Git 状态和当前执行策略。</p>
      {workspace ? <>
        <ValueRow title="项目目录" value={workspace.workspace} />
        <ValueRow title="个人数据目录" value={workspace.outer_home} />
        <ValueRow title="项目状态目录" value={workspace.workspace_outer} />
        <ValueRow title="执行权限" value={`${workspace.permissions} · ${workspace.sandbox} · ${workspace.approval}`} />
        {workspace.git.available ? <>
          <ValueRow title="Git 分支" value={workspace.git.branch || "分离的 HEAD"} />
          <pre className="ws-code" aria-label="Git 工作区状态">{workspace.git.status || "工作区干净"}</pre>
        </> : <p className="sc-description">{workspace.git.error || "当前目录不是 Git 仓库"}</p>}
      </> : <p className="sc-description">正在读取本机状态…</p>}
      <button className="sc-button" disabled={op.busy} onClick={() => void op.run(() => load())}>刷新工作区</button>
    </section>
    <section className="sc-section">
      <h3>补丁工件</h3>
      <p className="sc-description">导入 DSH SEARCH / REPLACE 补丁，仅保存在本机。预览会验证摘要、工作区绑定和路径权限；确认应用后才修改项目文件。</p>
      <label className="sc-field">来源名称<input className="sc-input" value={source} onChange={event => setSource(event.target.value)} maxLength={200} /></label>
      <label className="sc-field">补丁内容<textarea className="sc-textarea ws-mono" value={patch} onChange={event => setPatch(event.target.value)} rows={5} placeholder="*** Update File: example.txt" /></label>
      <button className="sc-button" disabled={op.busy || !workspace || !patch.trim() || !source.trim()} onClick={() => void op.run(async () => {
        await rpc(token, "cloud/import", { workspace: workspace!.workspace, source: source.trim(), text: patch });
        setPatch(""); await load(); op.setNotice("补丁已保存，尚未修改工作区。");
      })}>导入补丁</button>
      <div className="ws-list">
        {artifacts.length === 0 && <p className="sc-description">尚无本机补丁工件。</p>}
        {artifacts.map(item => <div className="ws-item" key={item.id}>
          <div className="ws-item-heading"><strong>{item.source}</strong><button className="sc-button" disabled={op.busy} onClick={() => void op.run(async () => {
            setConfirmApply(false); setPreview(null);
            const result = await rpc<PatchPreview>(token, "workspace/artifact_apply", { id: item.id, dry_run: true });
            setPreview(result);
          })}>预览 {item.source}</button></div>
          <code className="ws-subtle">{item.id}</code>
          <details><summary>查看补丁内容</summary><pre className="ws-code">{item.patch}</pre></details>
        </div>)}
      </div>
      {preview && <div className="ws-result" aria-label="补丁预览">
        <p>{preview.hunks} 个修改块 · {preview.files.length} 个文件</p>
        <pre className="ws-code">{preview.files.join("\n")}</pre>
        {preview.report ? <p role="status">{preview.report}</p> : confirmApply ? <>
          <p className="sc-description">确认将这些修改写入当前工作区？</p>
          <div className="sc-actions"><button className="sc-button sc-primary" disabled={op.busy} onClick={() => void op.run(async () => {
            const result = await rpc<PatchPreview>(token, "workspace/artifact_apply", { id: preview.artifact_id, dry_run: false });
            setPreview(result); setConfirmApply(false); await load(); op.setNotice("补丁已应用到工作区。");
          })}>确认写入文件</button><button className="sc-button" disabled={op.busy} onClick={() => setConfirmApply(false)}>取消</button></div>
        </> : <button className="sc-button" disabled={op.busy} onClick={() => setConfirmApply(true)}>应用此补丁</button>}
      </div>}
    </section>
    <Feedback {...op} />
  </>;
}

function SessionOutput({ result }: { result: SessionResult | null }) {
  if (!result) return null;
  const answers = result.session.events.filter(event => event.type === "assistant_message" && event.text);
  return <div className="ws-result" aria-label="任务输出">
    <div className="ws-item-heading"><strong>任务输出</strong><span className="ws-subtle">{result.state ? states[result.state] || result.state : "正在创建任务"}</span></div>
    {answers.length ? answers.map(event => <div className="ws-markdown" key={event.id}><ReactMarkdown>{event.text!}</ReactMarkdown></div>) : <p className="sc-description">尚无模型回答。执行中的任务会持续刷新；需要审批时请在主对话中处理。</p>}
    <details><summary>执行记录（{result.session.events.length}）</summary>
      {result.session.events.filter(event => event.type !== "assistant_message").map(event => <div className="ws-event" key={event.id}><b>{event.name || event.type}</b><pre>{event.content || event.text || (event.arguments ? JSON.stringify(event.arguments, null, 2) : "")}</pre></div>)}
    </details>
  </div>;
}

function TasksPanel({ token, onChanged }: Omit<Props, "section">) {
  const op = useOperation();
  const [tasks, setTasks] = useState<Task[]>([]);
  const [prompt, setPrompt] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const [result, setResult] = useState<SessionResult | null>(null);
  const [cancelId, setCancelId] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  useEffect(() => {
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    const refresh = async () => {
      try {
        const value = await rpc<{ tasks: Task[] }>(token, "tasks/list", { all: true }, controller.signal);
        const output = selected ? await rpc<SessionResult>(token, "sessions/get", { id: selected }, controller.signal) : null;
        if (controller.signal.aborted) return;
        setTasks(value.tasks.sort((a, b) => b.updated_at.localeCompare(a.updated_at))); setResult(output); setLoaded(true);
      } catch (failure) { if (!controller.signal.aborted) op.setError(errorText(failure)); }
      if (!controller.signal.aborted) timer = setTimeout(refresh, 2500);
    };
    void refresh();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [token, selected]);
  const control = (task: Task, action: "pause" | "resume" | "cancel") => void op.run(async () => {
    const result = await rpc<{ task?: Task }>(token, `tasks/${action}`, { id: task.id, ...(action === "resume" ? { wait: false } : {}) });
    if (result.task) setTasks(previous => previous.map(item => item.id === task.id ? result.task! : item));
    setCancelId(null); onChanged?.(); op.setNotice(action === "pause" ? "暂停请求已提交。" : action === "cancel" ? "任务已取消。" : "任务已继续执行。");
  });
  return <>
    <section className="sc-section"><h3>本机任务</h3>
      <p className="sc-description">任务由当前 DSH 模型与工具实际执行，保留会话、状态与输出。执行权限沿用安全设置。</p>
      <label className="sc-field">任务说明<textarea className="sc-textarea" value={prompt} onChange={event => setPrompt(event.target.value)} maxLength={16000} rows={4} placeholder="描述希望 DSH 完成的工作…" /></label>
      <button className="sc-button sc-primary" disabled={op.busy || !prompt.trim()} onClick={() => void op.run(async () => {
        const accepted = await rpc<Accepted>(token, "agent/turn", { prompt: prompt.trim(), wait: false });
        if (!accepted.accepted) throw new Error("服务未接受任务。");
        setSelected(accepted.session_id); setResult(null); setPrompt(""); onChanged?.(); op.setNotice("任务已提交，状态与输出将自动刷新。");
      })}>开始任务</button>
    </section>
    <section className="sc-section"><h3>任务记录</h3>
      {!loaded && <p className="sc-description">正在读取任务…</p>}
      {loaded && tasks.length === 0 && <p className="sc-description">尚无任务记录。</p>}
      <div className="ws-list">{tasks.map(task => <div className="ws-item" key={task.id}>
        <div className="ws-item-heading"><strong>{task.goal.outcome}</strong><span className="ws-subtle">{states[task.state] || task.state}</span></div>
        <code className="ws-subtle">{task.id}</code>
        <div className="sc-actions">
          {task.session_id && <button className="sc-button" disabled={op.busy} onClick={() => { setResult(null); setSelected(task.session_id); }}>查看输出</button>}
          {active.has(task.state) && <button className="sc-button" disabled={op.busy} onClick={() => control(task, "pause")}>暂停任务</button>}
          {["paused", "failed"].includes(task.state) && <button className="sc-button" disabled={op.busy} onClick={() => control(task, "resume")}>继续任务</button>}
          {!terminal.has(task.state) && (cancelId === task.id ? <><button className="sc-button sc-danger" disabled={op.busy} onClick={() => control(task, "cancel")}>确认取消任务</button><button className="sc-button" disabled={op.busy} onClick={() => setCancelId(null)}>保留任务</button></> : <button className="sc-button" disabled={op.busy} onClick={() => setCancelId(task.id)}>取消任务</button>)}
        </div>
      </div>)}</div>
      <SessionOutput result={result} />
    </section>
    <Feedback {...op} />
  </>;
}

function ReviewPanel({ token, onChanged }: Omit<Props, "section">) {
  const op = useOperation();
  const [scope, setScope] = useState("uncommitted");
  const [reference, setReference] = useState("");
  const [instructions, setInstructions] = useState("");
  const [prepared, setPrepared] = useState<PreparedReview | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [result, setResult] = useState<SessionResult | null>(null);
  useEffect(() => {
    if (!sessionId) return;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    let terminalSeen = false;
    const refresh = async () => {
      try {
        const value = await rpc<SessionResult>(token, "sessions/get", { id: sessionId }, controller.signal);
        if (controller.signal.aborted) return;
        setResult(value);
        // The server captures the session before reading task state. A turn
        // can finish between those reads, so reconcile once more after the
        // first terminal snapshot to include its final persisted answer.
        if (value.state && ["completed", "cancelled", "failed"].includes(value.state)) {
          if (terminalSeen) { onChanged?.(); return; }
          terminalSeen = true;
        } else terminalSeen = false;
      } catch (failure) { if (!controller.signal.aborted) op.setError(errorText(failure)); }
      if (!controller.signal.aborted) timer = setTimeout(refresh, 2000);
    };
    void refresh();
    return () => { controller.abort(); clearTimeout(timer); };
  }, [token, sessionId]);
  const invalidate = () => { setPrepared(null); op.setNotice(""); };
  return <>
    <section className="sc-section"><h3>DSH 代码审查</h3>
      <p className="sc-description">先读取当前仓库的真实 Git 差异，再交给已配置的 DSH 模型审查。结果保存在正常会话中，不自动发布到 GitHub。</p>
      <label className="sc-field">审查范围<select className="sc-select" value={scope} disabled={op.busy} onChange={event => { invalidate(); setScope(event.target.value); }}>
        <option value="uncommitted">未提交的已跟踪改动</option><option value="base">与指定分支或版本比较</option><option value="commit">单个提交</option>
      </select></label>
      {scope !== "uncommitted" && <label className="sc-field">Git 引用<input className="sc-input" value={reference} disabled={op.busy} onChange={event => { invalidate(); setReference(event.target.value); }} maxLength={200} placeholder={scope === "commit" ? "提交 SHA 或 HEAD" : "main 或 origin/main"} /></label>}
      <label className="sc-field">额外审查要求<textarea className="sc-textarea" value={instructions} disabled={op.busy} onChange={event => { invalidate(); setInstructions(event.target.value); }} maxLength={8000} rows={3} placeholder="例如：重点检查状态竞争、错误处理与回归。" /></label>
      <button className="sc-button" disabled={op.busy || (scope !== "uncommitted" && !reference.trim())} onClick={() => void op.run(async () => {
        setPrepared(null);
        setPrepared(await rpc<PreparedReview>(token, "review/prepare", { scope, instructions: instructions.trim(), ...(scope !== "uncommitted" ? { reference: reference.trim() } : {}) }));
      })}>读取审查差异</button>
      {prepared && <div className="ws-result"><strong>{prepared.label}</strong><pre className="ws-code" aria-label="审查差异">{prepared.diff}</pre>
        <p className="sc-description">将审查上述差异快照。DSH 会收到仅分析、不修改文件的指令，工具执行仍受当前权限设置约束。</p>
        <button className="sc-button sc-primary" disabled={op.busy} onClick={() => void op.run(async () => {
          const accepted = await rpc<Accepted>(token, "agent/turn", { prompt: prepared.prompt, wait: false });
          if (!accepted.accepted) throw new Error("服务未接受审查任务。");
          setSessionId(accepted.session_id); setResult(null); setPrepared(null); onChanged?.(); op.setNotice("代码审查已开始。结果保存在会话中，也可在 DSH 任务页管理。");
        })}>开始代码审查</button>
      </div>}
    </section>
    <Feedback {...op} />
    <SessionOutput result={result} />
  </>;
}
