import { useEffect, useRef, useState, type FormEvent } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import {
  ArrowUpIcon,
  CheckIcon,
  ChevronRightIcon,
  Cross1Icon,
  GearIcon,
  HamburgerMenuIcon,
  MixerHorizontalIcon,
  PlusIcon,
  ReloadIcon,
  StopIcon,
} from "@radix-ui/react-icons";
import Markdown from "react-markdown";
import { Emblem } from "./Emblem";
import { errorText, post } from "./api";
import {
  readPreferences,
  savePreferences,
  type WebPreferences,
} from "./preferences";
import { useHarness } from "./useHarness";
import type { Activity, Bootstrap, Profile, SessionEvent } from "./types";

type Harness = ReturnType<typeof useHarness>;
const labelSession = (name: string | null, id: string) =>
  name || `会话 ${id.slice(0, 8)}`;
function ToolActivity({ item }: { item: Activity }) {
  return (
    <details className="tool-activity">
      <summary>
        <span
          className={`activity-dot ${item.finished ? (item.ok ? "ok" : "failed") : "working"}`}
        />
        <span>{item.name}</span>
        <small>
          {item.finished ? (item.ok ? "完成" : "未成功") : "运行中"}
        </small>
        <ChevronRightIcon />
      </summary>
      {item.text && <pre>{item.text}</pre>}
    </details>
  );
}
function Message({ role, text }: { role: "user" | "assistant"; text: string }) {
  return (
    <article className={`message ${role}`}>
      <div className="message-label">
        {role === "user" ? "YOU" : "DSH"}
        <span>{role === "user" ? "任务输入" : "回复"}</span>
      </div>
      <div className="message-body">
        {role === "assistant" ? (
          <Markdown
            components={{
              a: (props) => (
                <a {...props} target="_blank" rel="noreferrer noopener" />
              ),
            }}
          >
            {text}
          </Markdown>
        ) : (
          <p>{text}</p>
        )}
      </div>
    </article>
  );
}
function Transcript({ events }: { events: SessionEvent[] }) {
  const results = new Map(
    events
      .filter((event) => event.type === "tool_result")
      .map((event) => [event.call_id, event]),
  );
  return (
    <>
      {events.map((event) => {
        if (event.type === "user_message" || event.type === "assistant_message")
          return (
            <Message
              key={event.id}
              role={event.type === "user_message" ? "user" : "assistant"}
              text={event.text || ""}
            />
          );
        if (event.type === "tool_call") {
          const result = results.get(event.call_id);
          return (
            <ToolActivity
              key={event.id}
              item={{
                id: event.id,
                kind: "tool",
                name: event.name,
                text:
                  result?.content || JSON.stringify(event.arguments, null, 2),
                ok: result?.ok,
                finished: !!result,
              }}
            />
          );
        }
        if (event.type === "system_note")
          return (
            <details className="system-note" key={event.id}>
              <summary>运行时记录</summary>
              <pre>{event.text}</pre>
            </details>
          );
        return null;
      })}
    </>
  );
}
function Catalog({ data, onClose }: { data: Bootstrap; onClose: () => void }) {
  const [tab, setTab] = useState<"skills" | "plugins">("skills");
  const [search, setSearch] = useState("");
  const skills = data.skills.filter((item) =>
    `${item.name} ${item.description}`
      .toLowerCase()
      .includes(search.toLowerCase()),
  );
  const plugins = data.plugins.filter((item) =>
    item.name.toLowerCase().includes(search.toLowerCase()),
  );
  return (
    <aside className="catalog" aria-label="本机能力目录">
      <div className="catalog-heading">
        <div>
          <span className="eyebrow">LOCAL REGISTRY</span>
          <h2>已挂载能力</h2>
        </div>
        <button
          className="icon-button"
          onClick={onClose}
          aria-label="关闭能力目录"
        >
          <Cross1Icon />
        </button>
      </div>
      <div className="catalog-tabs" role="tablist" aria-label="能力类型">
        <button
          role="tab"
          aria-selected={tab === "skills"}
          onClick={() => setTab("skills")}
        >
          Skills <b>{data.skills.length}</b>
        </button>
        <button
          role="tab"
          aria-selected={tab === "plugins"}
          onClick={() => setTab("plugins")}
        >
          Plugins <b>{data.plugins.length}</b>
        </button>
      </div>
      <input
        className="catalog-search"
        aria-label="搜索已挂载能力"
        placeholder="搜索名称或说明…"
        value={search}
        onChange={(event) => setSearch(event.target.value)}
      />
      <div
        className="catalog-list"
        role="tabpanel"
        aria-label={tab === "skills" ? "Skills" : "Plugins"}
      >
        {tab === "skills"
          ? skills.map((item) => (
              <details className="registry-item" key={item.source + item.name}>
                <summary>
                  <span className="registry-symbol">S</span>
                  <span className="registry-name" title={item.name}>
                    {item.name}
                    <small>SKILL / LOADED</small>
                  </span>
                  <PlusIcon />
                </summary>
                <div className="registry-detail">
                  <p>{item.description || "此能力未提供描述。"}</p>
                  <code>{item.source}</code>
                </div>
              </details>
            ))
          : plugins.map((item) => (
              <details className="registry-item" key={item.id}>
                <summary>
                  <span className="registry-symbol">P</span>
                  <span className="registry-name">
                    {item.name}
                    <small>{item.tool_count} TOOLS / LOADED</small>
                  </span>
                  <PlusIcon />
                </summary>
                <div className="registry-detail">
                  <code>{item.id}</code>
                  <p>{item.tool_count} 个工具已挂载到当前运行时。</p>
                </div>
              </details>
            ))}
        {(tab === "skills" ? skills : plugins).length === 0 && (
          <p className="subtle empty-list">
            {search ? "没有匹配的能力。" : "当前没有已挂载项目。"}
          </p>
        )}
      </div>
      <div className="catalog-note">
        <span className="status-dot" />
        来自当前 Rust 运行时
      </div>
    </aside>
  );
}
function Settings({
  harness,
  onReplay,
}: {
  harness: Harness;
  onReplay: () => void;
}) {
  const { data, setData } = harness;
  const [open, setOpen] = useState(false);
  const [username, setUsername] = useState("");
  const [badge, setBadge] = useState("");
  const [profileStatus, setProfileStatus] = useState("");
  const [saving, setSaving] = useState(false);
  const [preference, setPreference] = useState<WebPreferences>(readPreferences);
  const [preferenceStatus, setPreferenceStatus] = useState("");
  const [cliStatus, setCliStatus] = useState("");
  const [cliSaving, setCliSaving] = useState(false);
  useEffect(() => {
    if (data) {
      setUsername(data.profile.username);
      setBadge(data.profile.badge_id);
    }
  }, [data?.profile.username, data?.profile.badge_id]);
  async function saveIdentity(event: FormEvent) {
    event.preventDefault();
    if (!data) return;
    setSaving(true);
    setProfileStatus("");
    try {
      const profile = await post<Profile>("/api/profile", data.token, {
        username,
        badge_id: badge,
      });
      setData((previous) => previous && { ...previous, profile });
      setProfileStatus("已保存到本机资料。");
    } catch (failure) {
      setProfileStatus(`保存失败：${errorText(failure)}`);
    } finally {
      setSaving(false);
    }
  }
  function updatePreference(next: WebPreferences) {
    try {
      savePreferences(next);
      setPreference(next);
      setPreferenceStatus("已保存到当前浏览器。");
    } catch {
      setPreferenceStatus("浏览器未允许保存设置，请检查存储权限。");
    }
  }
  async function setCli(enabled: boolean) {
    if (!data) return;
    setCliSaving(true);
    setCliStatus("");
    try {
      await post("/api/startup-next", data.token, { enabled });
      setData(
        (previous) =>
          previous && {
            ...previous,
            startup: { ...previous.startup, next_enabled: enabled },
          },
      );
      setCliStatus(`下一次 CLI 启动将${enabled ? "播放" : "跳过"}动画。`);
    } catch (failure) {
      setCliStatus(errorText(failure));
    } finally {
      setCliSaving(false);
    }
  }
  return (
    <Dialog.Root open={open} onOpenChange={setOpen}>
      <Dialog.Trigger asChild>
        <button className="settings-trigger">
          <GearIcon />
          <span>偏好设置</span>
          <span className="keycap"></span>
        </button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="dialog-overlay" />
        <Dialog.Content className="settings-dialog">
          <div className="dialog-top">
            <span className="eyebrow">OPERATOR CONFIGURATION</span>
            <Dialog.Close className="icon-button" aria-label="关闭设置">
              <Cross1Icon />
            </Dialog.Close>
          </div>
          <Dialog.Title>个人与启动设置</Dialog.Title>
          <Dialog.Description>
            本机身份资料与两个独立的启动入口。
          </Dialog.Description>
          <form onSubmit={saveIdentity} className="settings-section">
            <h3>操作员身份</h3>
            <div className="identity-inputs">
              <label>
                显示名称
                <input
                  required
                  maxLength={32}
                  value={username}
                  onChange={(event) => setUsername(event.target.value)}
                  autoComplete="nickname"
                />
              </label>
              <label>
                档案编号
                <input
                  required
                  maxLength={32}
                  value={badge}
                  onChange={(event) => setBadge(event.target.value)}
                />
              </label>
            </div>
            <div className="inline-actions">
              <button
                className="solid-button"
                disabled={saving || !data}
                type="submit"
              >
                {saving ? "保存中…" : "保存资料"}
              </button>
              <span className="form-status" role="status">
                {profileStatus}
              </span>
            </div>
          </form>
          <section className="settings-section">
            <h3>
              网页开场<span>WEB</span>
            </h3>
            <label className="setting-row">
              <span>
                每次打开网页时播放<small>默认关闭；动画需点击画面开始。</small>
              </span>
              <input
                type="checkbox"
                checked={preference.enabled ?? data?.startup.enabled ?? false}
                onChange={(event) =>
                  updatePreference({
                    ...preference,
                    enabled: event.target.checked,
                  })
                }
              />
            </label>
            <label className="setting-row">
              <span>
                仅下一次打开网页时播放<small>使用一次后自动清除。</small>
              </span>
              <input
                type="checkbox"
                checked={preference.next}
                onChange={(event) =>
                  updatePreference({
                    ...preference,
                    next: event.target.checked,
                  })
                }
              />
            </label>
            <div className="inline-actions">
              <button
                className="outline-button"
                onClick={() => {
                  setOpen(false);
                  onReplay();
                }}
              >
                现在体验开场 <ChevronRightIcon />
              </button>
              <span className="form-status" role="status">
                {preferenceStatus}
              </span>
            </div>
          </section>
          <section className="settings-section">
            <h3>
              下一次终端开场<span>CLI</span>
            </h3>
            <p className="subtle">
              当前：
              {data?.startup.next_enabled == null
                ? `跟随配置（${data?.startup.enabled ? "开启" : "关闭"}）`
                : data.startup.next_enabled
                  ? "下一次开启"
                  : "下一次关闭"}
              。此开关写入本机 CLI 一次性设置。
            </p>
            <div className="inline-actions">
              <button
                className="outline-button"
                disabled={!data || cliSaving}
                onClick={() => void setCli(true)}
              >
                下一次开启
              </button>
              <button
                className="outline-button"
                disabled={!data || cliSaving}
                onClick={() => void setCli(false)}
              >
                下一次关闭
              </button>
            </div>
            <p className="form-status" role="status">
              {cliStatus}
            </p>
          </section>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

export function App({
  onReplay,
  initialData,
}: {
  onReplay: () => void;
  initialData?: Bootstrap;
}) {
  const harness = useHarness(initialData);
  const { data, sessions, currentId, snapshot, live } = harness;
  const [draft, setDraft] = useState("");
  const [sidebar, setSidebar] = useState(false);
  const [catalog, setCatalog] = useState(false);
  const scroll = useRef<HTMLDivElement>(null);
  const follow = useRef(true);
  const composer = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    if (follow.current && scroll.current)
      scroll.current.scrollTop = scroll.current.scrollHeight;
  }, [snapshot, live]);
  useEffect(() => {
    follow.current = true;
  }, [currentId]);
  const events = snapshot?.session.events || [];
  const visibleEvents = events.filter((event) =>
    ["user_message", "assistant_message", "tool_call", "system_note"].includes(
      event.type,
    ),
  );
  const empty =
    visibleEvents.length === 0 && !live?.prompt && !live?.entries.length;
  async function submit(event: FormEvent) {
    event.preventDefault();
    const sent = draft;
    if (await harness.send(sent))
      setDraft((previous) => (previous === sent ? "" : previous));
  }
  const currentApprovals = harness.approvals.filter(
    (item) => item.task_id === (live?.taskId || snapshot?.task_id),
  );
  return (
    <div
      className={`harness ${sidebar ? "sidebar-open" : ""} ${catalog ? "catalog-open" : ""}`}
    >
      {sidebar && (
        <button
          className="mobile-scrim"
          aria-label="关闭会话侧栏"
          onClick={() => setSidebar(false)}
        />
      )}
      <aside className="sidebar">
        <a className="brand" href="/" aria-label="DSH Harness 首页">
          <Emblem size={44} />
          <span>
            DSH<span>DEEPSEEK HARNESS</span>
          </span>
        </a>
        <div className="sidebar-section-label">WORKSPACE / 01</div>
        <button
          className="new-session"
          disabled={!data || harness.submitting}
          onClick={() => {
            void harness.createSession();
            setSidebar(false);
          }}
        >
          <PlusIcon />
          新建会话
        </button>
        <div className="session-list-heading">
          <span>会话档案</span>
          <span>{sessions.length.toString().padStart(2, "0")}</span>
        </div>
        <nav className="session-list" aria-label="会话列表">
          {sessions.map((item) => (
            <button
              key={item.id}
              className={`session-link ${item.id === currentId ? "active" : ""}`}
              onClick={() => {
                harness.selectSession(item.id);
                setSidebar(false);
              }}
            >
              <span className="session-marker" />
              <span className="session-info">
                <b>{labelSession(item.name, item.id)}</b>
                <small>
                  {item.updated_at
                    ? new Date(item.updated_at).toLocaleDateString("zh-CN", {
                        month: "2-digit",
                        day: "2-digit",
                      })
                    : "尚无消息"}
                  <span>{item.event_count} 条记录</span>
                </small>
              </span>
            </button>
          ))}
          {data && !sessions.length && (
            <p className="subtle empty-list">
              还没有会话。
              <br />
              写下第一个任务即可开始。
            </p>
          )}
        </nav>
        <div className="sidebar-bottom">
          <div className="operator">
            <span className="operator-avatar">
              {data?.profile.username.slice(0, 1).toUpperCase() || "—"}
            </span>
            <span>
              <b>{data?.profile.username || "连接本机…"}</b>
              <small>{data?.profile.badge_id || "OPERATOR"}</small>
            </span>
            <span className={`status-dot ${!data ? "offline" : ""}`} />
          </div>
          <Settings harness={harness} onReplay={onReplay} />
        </div>
      </aside>
      <div className="workspace">
        <header className="workspace-header">
          <button
            className="icon-button mobile-menu"
            aria-label="打开会话侧栏"
            onClick={() => setSidebar(true)}
          >
            <HamburgerMenuIcon />
          </button>
          <div className="workspace-location">
            <span className="eyebrow">LOCAL WORKSPACE</span>
            <b title={data?.workspace}>
              {data?.workspace.split(/[\\/]/).filter(Boolean).at(-1) ||
                "等待连接"}
            </b>
          </div>
          <div className="header-right">
            <span
              className={`connection-label ${data?.model.ready ? "" : "muted"}`}
            >
              <span
                className={`status-dot ${data?.model.ready ? "" : "offline"}`}
              />
              {data?.model.name || "LOCAL RUNTIME"}
            </span>
            <button
              className={`outline-button catalog-toggle ${catalog ? "selected" : ""}`}
              aria-expanded={catalog}
              onClick={() => setCatalog((value) => !value)}
            >
              <MixerHorizontalIcon />
              <span>本机能力</span>
              {data && <b>{data.skills.length + data.plugins.length}</b>}
            </button>
          </div>
        </header>
        <main className="conversation">
          <div className="conversation-top">
            <span className="eyebrow">
              {currentId
                ? `SESSION / ${currentId.slice(0, 8).toUpperCase()}`
                : "NEW SESSION"}
            </span>
            <span className="subtle">
              {live?.running ? live.status : "DEEP DIVE / 工作台"}
            </span>
          </div>
          {harness.connectionError && (
            <div className="notice error" role="alert">
              <strong>无法连接本机 Harness</strong>
              <p>{harness.connectionError}</p>
              <code>dsh web</code>
              <button className="outline-button" onClick={harness.reconnect}>
                <ReloadIcon />
                重新连接
              </button>
            </div>
          )}
          {harness.eventError && (
            <div className="notice error" role="status">
              事件连接暂时中断，正在重连：{harness.eventError}
            </div>
          )}
          <div
            className="transcript"
            ref={scroll}
            onScroll={() => {
              const element = scroll.current;
              if (element)
                follow.current =
                  element.scrollHeight -
                    element.scrollTop -
                    element.clientHeight <
                  96;
            }}
          >
            {empty && (
              <div className="welcome">
                <div className="welcome-mark">
                  <Emblem size={144} />
                  <span>
                    ENGINEERING
                    <br />
                    DIVISION / DSH
                  </span>
                </div>
                <span className="eyebrow">YOUR LOCAL AGENT WORKSPACE</span>
                <h1>
                  让想法，
                  <br />
                  开始运行。
                </h1>
                <p>
                  描述目标，交给 Harness。
                  <br />
                  会话、工具与工作区在本机协同。
                </p>
                <div className="welcome-registry">
                  <span>
                    <b>
                      {data
                        ? data.skills.length.toString().padStart(2, "0")
                        : "—"}
                    </b>{" "}
                    SKILLS
                  </span>
                  <span>
                    <b>
                      {data
                        ? data.plugins.length.toString().padStart(2, "0")
                        : "—"}
                    </b>{" "}
                    PLUGINS
                  </span>
                  <button onClick={() => setCatalog(true)}>
                    查看已挂载能力 <ChevronRightIcon />
                  </button>
                </div>
              </div>
            )}
            {harness.loadingSession && !snapshot && (
              <p className="loading-message" role="status">
                正在读取会话档案…
              </p>
            )}
            <Transcript events={events} />
            {live?.prompt &&
              !events.some(
                (event) =>
                  event.type === "user_message" &&
                  event.text === live.prompt &&
                  events.indexOf(event) > events.length - 8,
              ) && <Message role="user" text={live.prompt} />}
            {live?.entries.map((item) =>
              item.kind === "text" ? (
                <Message key={item.id} role="assistant" text={item.text} />
              ) : (
                <ToolActivity key={item.id} item={item} />
              ),
            )}
            {live?.running && (
              <div className="live-status" role="status">
                <span className="status-dot pulse" />
                {live.status || "正在处理"}
                <span className="scan-line" />
              </div>
            )}
            {currentApprovals.map((item) => (
              <div className="approval" key={item.request_id}>
                <span className="eyebrow">TOOL AUTHORIZATION</span>
                <h3>{item.name}</h3>
                <pre>{item.summary}</pre>
                <div className="inline-actions">
                  <button
                    className="solid-button"
                    onClick={() =>
                      void harness.resolveApproval(item.request_id, true)
                    }
                  >
                    <CheckIcon />
                    允许执行
                  </button>
                  <button
                    className="outline-button"
                    onClick={() =>
                      void harness.resolveApproval(item.request_id, false)
                    }
                  >
                    拒绝
                  </button>
                </div>
              </div>
            ))}
            {(harness.error || live?.error) && (
              <div className="notice error" role="alert">
                {harness.error || live?.error}
              </div>
            )}
          </div>
          <div className="composer-area">
            {data && !data.model.ready && (
              <div className="model-warning">
                模型尚未配置凭据。请配置本机 LLM 后重启 <code>dsh web</code>
                ；已有会话和能力仍可查看。
              </div>
            )}
            <form className="composer" onSubmit={(event) => void submit(event)}>
              <textarea
                ref={composer}
                aria-label="任务内容"
                placeholder="描述你的任务…"
                rows={3}
                value={draft}
                onChange={(event) => setDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (
                    event.key === "Enter" &&
                    !event.shiftKey &&
                    !event.nativeEvent.isComposing
                  ) {
                    event.preventDefault();
                    void submit(event);
                  }
                }}
                disabled={!data}
              />
              <div className="composer-bottom">
                <span className="composer-context">
                  <span className="status-dot" />
                  {data?.model.local ? "本机模型" : "本机工作区"}
                  <span className="composer-divider" />
                  {data?.model.name || "等待连接"}
                </span>
                {live?.running ? (
                  <button
                    className="send-button stop-button"
                    type="button"
                    onClick={() => void harness.stop()}
                    disabled={!live.taskId && !snapshot?.task_id}
                    aria-label="暂停当前任务"
                  >
                    <StopIcon />
                    暂停
                  </button>
                ) : (
                  <button
                    className="send-button"
                    type="submit"
                    disabled={!data || !draft.trim() || harness.submitting}
                    aria-label="发送任务"
                  >
                    <ArrowUpIcon />
                  </button>
                )}
              </div>
            </form>
            <div className="composer-foot">
              <span>Enter 发送 / Shift + Enter 换行</span>
              <span>DSH · RUST RUNTIME</span>
            </div>
          </div>
        </main>
      </div>
      {catalog && data && (
        <Catalog data={data} onClose={() => setCatalog(false)} />
      )}
    </div>
  );
}
