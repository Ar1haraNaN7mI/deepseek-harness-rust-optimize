import { useEffect, useState } from "react";
import { errorText, isAbort, rpc } from "./api";
import "./extension-settings.css";

interface PluginEntry {
  id: string | null;
  name: string;
  version?: string;
  description?: string;
  root: string;
  enabled: boolean;
  mounted: boolean;
  managed: boolean;
  tools: string[];
  skills: string[];
  error?: string | null;
}
interface SkillEntry {
  name: string;
  description: string;
  path: string;
  source: string;
  enabled: boolean;
}
interface ExtensionSnapshot {
  plugins: PluginEntry[];
  skills: SkillEntry[];
  install_root: string;
  warnings?: string[];
}
const sourceLabels: Record<string, string> = {
  plugin: "插件附带",
  "outer-home": "DSH 用户",
  "outer-workspace": "DSH 工作区",
  "project-dsh": "项目 DSH",
  "project-agents": "项目共享",
  "openai-user": "用户共享",
  bundled: "DSH 内置",
  runtime: "运行时",
};

export function ExtensionSettings({ token, onChanged, initialTab = "plugins" }: {
  token: string;
  onChanged?: () => void;
  initialTab?: "plugins" | "skills";
}) {
  const [data, setData] = useState<ExtensionSnapshot | null>(null);
  const [tab, setTab] = useState(initialTab);
  const [query, setQuery] = useState("");
  const [path, setPath] = useState("");
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [uninstall, setUninstall] = useState<PluginEntry | null>(null);
  const [document, setDocument] = useState<{ name: string; content: string } | null>(null);

  useEffect(() => setTab(initialTab), [initialTab]);
  useEffect(() => {
    const controller = new AbortController();
    rpc<ExtensionSnapshot>(token, "extensions/list", {}, controller.signal)
      .then(setData)
      .catch((failure) => { if (!isAbort(failure)) setError(errorText(failure)); });
    return () => controller.abort();
  }, [token]);

  async function mutate(method: string, params: Record<string, unknown>, message: string) {
    if (busy) return;
    setBusy(method);
    setError("");
    setNotice("");
    try {
      const next = await rpc<ExtensionSnapshot>(token, method, params);
      setData(next);
      setUninstall(null);
      setNotice(message);
      if (method === "plugins/install") setPath("");
      onChanged?.();
    } catch (failure) {
      setError(errorText(failure));
      // Partial filesystem failures can leave a package disabled. Reconcile
      // actual registry state instead of displaying the old successful state.
      try { setData(await rpc<ExtensionSnapshot>(token, "extensions/list")); }
      catch { /* Keep the actionable mutation error visible. */ }
    } finally { setBusy(""); }
  }

  async function readSkill(name: string) {
    setBusy(`read:${name}`);
    setError("");
    setDocument(null);
    try { setDocument(await rpc(token, "skills/read", { name })); }
    catch (failure) { setError(errorText(failure)); }
    finally { setBusy(""); }
  }

  const match = (name: string, description = "") => `${name} ${description}`.toLocaleLowerCase().includes(query.toLocaleLowerCase());
  const plugins = data?.plugins.filter((plugin) => match(plugin.name, plugin.description)) ?? [];
  const skills = data?.skills.filter((skill) => match(skill.name, skill.description)) ?? [];

  return <section className="sc-section ext-settings" aria-label="DSH 插件与 Skills">
    <p className="sc-description">管理这台电脑实际加载的扩展。更改会保存到 DSH，并立即影响新工具调用和技能路由；已开始执行的工具会完成当前调用。</p>
    <div className="ext-toolbar">
      <div className="ext-tabs" role="group" aria-label="扩展类型">
        <button className="sc-button" aria-pressed={tab === "plugins"} onClick={() => { setTab("plugins"); setDocument(null); }}>插件 {data ? `(${data.plugins.length})` : ""}</button>
        <button className="sc-button" aria-pressed={tab === "skills"} onClick={() => setTab("skills")}>Skills {data ? `(${data.skills.length})` : ""}</button>
      </div>
      <button className="sc-button" disabled={!!busy} onClick={() => void mutate("extensions/reload", {}, "已重新扫描本机扩展。")}>重新扫描</button>
    </div>
    {error && <p className="ext-error" role="alert">{error}</p>}
    {notice && <p className="ext-notice" role="status">{notice}</p>}
    {!!data?.warnings?.length && <div className="ext-error" role="alert">扫描发现问题：{data.warnings.map((warning) => <p key={warning}>{warning}</p>)}</div>}
    {!data && !error && <p className="sc-description" role="status">正在读取本机扩展…</p>}
    {data && <>
      {tab === "plugins" && <form className="ext-install" onSubmit={(event) => { event.preventDefault(); void mutate("plugins/install", { path: path.trim() }, "插件已安装并加载。相同 ID 的安装会更新原插件。"); }}>
        <label className="sc-field"><span>从本机目录安装或更新</span>
          <input className="sc-input" value={path} onChange={(event) => setPath(event.target.value)} placeholder="包含 plugin.json 或 plugin.yml 的完整目录路径" required autoComplete="off" />
        </label>
        <button className="sc-button sc-primary" type="submit" disabled={!!busy || !path.trim()}>{busy === "plugins/install" ? "正在安装…" : "安装并加载"}</button>
        <p className="sc-description">安装到 <code>{data.install_root}</code>。Rhai 脚本会先通过编译校验；同 ID 的包会被更新。</p>
      </form>}
      <input className="sc-input" aria-label="搜索本机扩展" placeholder="搜索名称或说明" value={query} onChange={(event) => setQuery(event.target.value)} />
      {tab === "plugins" ? <div className="ext-list">
        {plugins.length === 0 && <p className="sc-empty">没有匹配的本机插件。</p>}
        {plugins.map((plugin) => <article className="ext-entry" key={plugin.id ?? plugin.root}>
          <div className="ext-heading"><strong>{plugin.name}</strong><span>{plugin.version}</span><span className={`ext-state ${plugin.mounted ? "is-live" : ""}`}>{plugin.mounted ? "已加载" : plugin.enabled ? "加载失败" : "已停用"}</span></div>
          <p>{plugin.description}</p>
          <p className="ext-meta">{plugin.tools.length} 个工具 · {plugin.skills.length} 个技能 · {plugin.managed ? "DSH 安装" : "外部来源"}</p>
          <code className="ext-path">{plugin.root}</code>
          {plugin.tools.length > 0 && <details><summary>工具名称</summary><p className="ext-tool-names">{plugin.tools.map((name) => <code key={name}>plugin.{plugin.id}.{name}</code>)}</p></details>}
          {plugin.error && <p className="ext-error">{plugin.error}</p>}
          <div className="ext-actions">
            <button className="sc-button" disabled={!!busy || !plugin.id} onClick={() => void mutate(plugin.enabled ? "plugins/disable" : "plugins/enable", { id: plugin.id }, plugin.enabled ? `${plugin.name} 已停用，其工具和附带技能已移出运行时。` : `${plugin.name} 已启用。`)}>{plugin.enabled ? "停用" : "启用"}</button>
            {plugin.managed && <button className="sc-button sc-danger" disabled={!!busy || !plugin.id} onClick={() => setUninstall(plugin)}>卸载</button>}
          </div>
          {uninstall?.id === plugin.id && plugin.id && <div className="ext-confirm" role="group" aria-label={`确认卸载 ${plugin.name}`}>
            <p>从 DSH 安装目录删除 {plugin.name} 的文件，并停用它的工具与技能？原始安装来源保留。</p>
            <button className="sc-button sc-danger" disabled={!!busy} onClick={() => void mutate("plugins/uninstall", { id: plugin.id }, `${plugin.name} 已从 DSH 安装目录卸载。`)}>确认卸载</button>
            <button className="sc-button" disabled={!!busy} onClick={() => setUninstall(null)}>取消</button>
          </div>}
        </article>)}
      </div> : <div className="ext-list">
        {skills.length === 0 && <p className="sc-empty">没有匹配的本机技能。停用插件的附带技能不会加载。</p>}
        {skills.map((skill) => <article className="ext-entry" key={skill.name}>
          <div className="ext-heading"><strong>{skill.name}</strong><span className={`ext-state ${skill.enabled ? "is-live" : ""}`}>{skill.enabled ? "可用" : "已停用"}</span></div>
          <p>{skill.description}</p><p className="ext-meta">{sourceLabels[skill.source] ?? skill.source}</p><code className="ext-path">{skill.path}</code>
          <div className="ext-actions">
            <button className="sc-button" disabled={!!busy} onClick={() => void readSkill(skill.name)}>查看指令</button>
            <button className="sc-button" disabled={!!busy} onClick={() => void mutate(skill.enabled ? "skills/disable" : "skills/enable", { name: skill.name }, skill.enabled ? `${skill.name} 已停用，不再参与技能路由和加载。` : `${skill.name} 已启用。`)}>{skill.enabled ? "停用" : "启用"}</button>
          </div>
        </article>)}
      </div>}
      {document && <section className="ext-document" aria-label={`${document.name} 技能指令`}><div className="ext-heading"><strong>{document.name}</strong><button className="sc-button" onClick={() => setDocument(null)}>关闭指令</button></div><pre>{document.content}</pre></section>}
    </>}
  </section>;
}
