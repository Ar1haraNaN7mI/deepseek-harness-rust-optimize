import {
  useEffect,
  useRef,
  useState,
  type ComponentType,
  type ReactNode,
  type FormEvent,
} from "react";
import * as Dialog from "@radix-ui/react-dialog";
import {
  ArchiveIcon,
  BarChartIcon,
  BellIcon,
  BoxIcon,
  CheckIcon,
  ChevronRightIcon,
  CodeIcon,
  Cross1Icon,
  DesktopIcon,
  DownloadIcon,
  FaceIcon,
  GearIcon,
  GlobeIcon,
  KeyboardIcon,
  LockClosedIcon,
  MagicWandIcon,
  MagnifyingGlassIcon,
  PersonIcon,
  PlayIcon,
  ReloadIcon,
  RocketIcon,
  SpeakerLoudIcon,
  StackIcon,
  SunIcon,
  TrashIcon,
} from "@radix-ui/react-icons";
import { Emblem } from "./Emblem";
import { Companion } from "./Companion";
import { bootstrap, errorText, isAbort, post, rpc } from "./api";
import { ModelServiceSettings } from "./ModelServiceSettings";
import { ExtensionSettings } from "./ExtensionSettings";
import { WorkspaceSettings } from "./WorkspaceSettings";
import {
  readPreferences,
  savePreferences,
  type WebPreferences,
} from "./preferences";
import {
  resetUiPreferences,
  useUiPreferences,
  type UiPreferences,
} from "./uiPreferences";
import type { useHarness } from "./useHarness";
import type { Profile, SessionSummary } from "./types";
import "./settings-center.css";

type Characteristic = "default" | "more" | "less";
type EditableSettings = {
  model?: string | null;
  thinking?: boolean | null;
  personality?: string | null;
  custom_instructions?: string;
  memory_inject: boolean;
  memory_generate: boolean;
  approval: string;
  sandbox: string;
  security_research_mode: boolean;
  characteristics?: {
    warmth: Characteristic;
    enthusiasm: Characteristic;
    headers_lists: Characteristic;
    emoji: Characteristic;
  };
};
type SettingsSnapshot = {
  settings: EditableSettings;
  effective: {
    model: string;
    thinking: boolean;
    backend: string;
    permissions: string;
    sandbox: string;
    approval: string;
    memory_available: boolean;
    memory_inject: boolean;
    memory_generate: boolean;
  };
  storage: {
    session_count: number;
    archived_count: number;
    session_bytes: number;
    memory_bytes: number;
    event_bytes: number;
    total_bytes: number;
  };
  usage: {
    session_count: number;
    message_count: number;
    user_message_count: number;
    assistant_message_count: number;
    tool_call_count: number;
    event_count: number;
    token_usage_available: false;
  };
};
type Memory = {
  id: string;
  at: string;
  query?: string;
  goal?: string;
  tool?: string;
  note: string;
  ok: boolean;
  skill?: string | null;
  plugin?: string | null;
};
type Memories = {
  memories: Memory[];
  feedback: Memory[];
  total: number;
  feedback_total: number;
};
type Category = {
  id: string;
  label: string;
  english: string;
  icon: ComponentType<{ className?: string }>;
  keywords: string;
  group?: string;
};
const categories: Category[] = [
  {
    id: "general",
    label: "通用",
    english: "General",
    icon: GearIcon,
    keywords: "模型 默认 思考 语言",
  },
  {
    id: "appearance",
    label: "外观",
    english: "Appearance",
    icon: SunIcon,
    keywords: "主题 深色 浅色 颜色 字号 密度 动画",
  },
  {
    id: "notifications",
    label: "通知",
    english: "Notifications",
    icon: BellIcon,
    keywords: "任务 完成 审批 提醒 声音 桌面",
  },
  {
    id: "profile",
    label: "个人资料",
    english: "Profile",
    icon: PersonIcon,
    keywords: "用户名 名称 操作员 编号 身份",
  },
  {
    id: "security",
    label: "执行权限",
    english: "Execution permissions",
    icon: LockClosedIcon,
    keywords: "批准 沙箱 权限 工具 安全 研究",
  },
  {
    id: "model-service",
    label: "模型服务",
    english: "Model service",
    icon: GlobeIcon,
    keywords: "后端 API URL 地址 模型 密钥 凭据 连接 测试",
  },
  {
    id: "archived",
    label: "已归档聊天",
    english: "Archived chats",
    icon: ArchiveIcon,
    keywords: "会话 恢复 删除 档案",
  },
  {
    id: "voice",
    label: "语音",
    english: "Voice",
    icon: SpeakerLoudIcon,
    keywords: "启动 英文 配音 旁白 试听",
  },
  {
    id: "storage",
    label: "存储",
    english: "Storage",
    icon: StackIcon,
    keywords: "空间 文件 记忆 字节 容量",
  },
  {
    id: "personalization",
    label: "个性化",
    english: "Personalization",
    icon: MagicWandIcon,
    keywords:
      "记忆 自定义 指令 风格 暖 热情 标题 列表 表情 warmth enthusiasm emoji",
  },
  {
    id: "pets",
    label: "宠物",
    english: "Pets",
    icon: FaceIcon,
    keywords: "DeepSeek 大肥鱼 桌宠 伙伴 动画",
  },
  {
    id: "keyboard",
    label: "键盘",
    english: "Keyboard",
    icon: KeyboardIcon,
    keywords: "发送 快捷键 Enter",
  },
  {
    id: "usage",
    label: "用量",
    english: "Usage",
    icon: BarChartIcon,
    keywords: "统计 token 会话 工具",
  },
  {
    id: "data",
    label: "数据控制",
    english: "Data controls",
    icon: BoxIcon,
    keywords: "导出 删除 归档 会话 重命名 重置",
  },
  {
    id: "plugins",
    label: "插件与 Skills",
    english: "Plugins and skills",
    icon: GlobeIcon,
    keywords: "skills 技能 扩展 安装 启用 停用 卸载",
    group: "集成",
  },
  {
    id: "workspace",
    label: "本机工作区",
    english: "Local workspace",
    icon: DesktopIcon,
    keywords: "目录 Git 分支 状态 修改 补丁 应用",
    group: "开发工具",
  },
  {
    id: "tasks",
    label: "DSH 任务",
    english: "DSH tasks",
    icon: CodeIcon,
    keywords: "执行 暂停 继续 取消 结果 本机",
  },
  {
    id: "code-review",
    label: "代码审查",
    english: "Code Review",
    icon: CheckIcon,
    keywords: "Git 分支 修改 差异 diff 模型 检查",
  },
  {
    id: "startup",
    label: "启动动画",
    english: "DSH startup",
    icon: RocketIcon,
    keywords: "开场 下次 一次性 CLI 终端 播放",
    group: "DSH",
  },
];
const defaultCharacteristics = {
  warmth: "default",
  enthusiasm: "default",
  headers_lists: "default",
  emoji: "default",
} as const;
function Row({
  title,
  description,
  children,
}: {
  title: string;
  description?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div className="sc-row">
      <div className="sc-row-copy">
        <span className="sc-row-title">{title}</span>
        {description && <p>{description}</p>}
      </div>
      {children && <div className="sc-row-control">{children}</div>}
    </div>
  );
}
function Toggle({
  label,
  checked,
  onChange,
  disabled = false,
}: {
  label: string;
  checked: boolean;
  onChange: (value: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-label={label}
      aria-checked={checked}
      disabled={disabled}
      className="sc-switch"
      onClick={() => onChange(!checked)}
    >
      <span />
    </button>
  );
}
function Select({
  label,
  value,
  onChange,
  options,
  disabled = false,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  options: [string, string][];
  disabled?: boolean;
}) {
  return (
    <select
      className="sc-select"
      aria-label={label}
      value={value}
      onChange={(event) => onChange(event.target.value)}
      disabled={disabled}
    >
      {options.map(([key, title]) => (
        <option key={key} value={key}>
          {title}
        </option>
      ))}
    </select>
  );
}
function Section({
  title,
  description,
  children,
}: {
  title?: string;
  description?: string;
  children: ReactNode;
}) {
  return (
    <section className="sc-section">
      {title && <h3>{title}</h3>}
      {description && <p className="sc-description">{description}</p>}
      {children}
    </section>
  );
}
const bytes = (value: number | undefined) =>
  value == null
    ? "—"
    : value < 1024
      ? `${value} B`
      : value < 1024 * 1024
        ? `${(value / 1024).toFixed(1)} KB`
        : `${(value / 1024 / 1024).toFixed(1)} MB`;
const date = (value: string | null) =>
  value
    ? new Date(value).toLocaleString("zh-CN", {
        year: "numeric",
        month: "2-digit",
        day: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
      })
    : "暂无消息";
const sessionName = (value: SessionSummary) =>
  value.name || `会话 ${value.id.slice(0, 8)}`;

export function SettingsCenter({
  harness,
  onReplay,
  open,
  onOpenChange,
}: {
  harness: ReturnType<typeof useHarness>;
  onReplay: () => void;
  open: boolean;
  onOpenChange: (value: boolean) => void;
}) {
  const { data, setData } = harness;
  const [ui, patchUi] = useUiPreferences();
  const [selected, setSelected] = useState("general");
  const [search, setSearch] = useState("");
  const [remote, setRemote] = useState<SettingsSnapshot>();
  const [draft, setDraft] = useState<EditableSettings>();
  const [allSessions, setAllSessions] = useState<SessionSummary[]>([]);
  const [memories, setMemories] = useState<Memories>();
  const [manageMemory, setManageMemory] = useState(false);
  const [manageSessions, setManageSessions] = useState(false);
  const [sessionSearch, setSessionSearch] = useState("");
  const [editingSession, setEditingSession] = useState<{
    id: string;
    name: string;
  }>();
  const [memorySearch, setMemorySearch] = useState("");
  const [username, setUsername] = useState("");
  const [badge, setBadge] = useState("");
  const [webPreference, setWebPreference] =
    useState<WebPreferences>(readPreferences);
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [permission, setPermission] = useState<
    NotificationPermission | "unsupported"
  >(() =>
    typeof Notification === "undefined"
      ? "unsupported"
      : Notification.permission,
  );
  const [confirmation, setConfirmation] = useState<{
    title: string;
    text: string;
    action: () => Promise<void>;
  }>();
  const generation = useRef(0);
  const operation = useRef(false);
  const nativeRefresh = useRef(0);
  const voice = useRef<HTMLAudioElement>(null);
  const [voicePhase, setVoicePhase] = useState("phase-0");
  const matches = categories.filter((category) =>
    `${category.label} ${category.english} ${category.keywords}`
      .toLowerCase()
      .includes(search.toLowerCase().trim()),
  );
  const current =
    matches.find((category) => category.id === selected) || matches[0];
  const blocked = !!busy || loading || !data;
  const archived = allSessions.filter((session) => session.archived);

  useEffect(() => {
    const serial = ++generation.current;
    if (!open) {
      setConfirmation(undefined);
      setEditingSession(undefined);
      return;
    }
    if (!data) return;
    const abort = new AbortController();
    setLoading(true);
    setError("");
    setNotice("");
    setRemote(undefined);
    setDraft(undefined);
    setMemories(undefined);
    setAllSessions([]);
    setConfirmation(undefined);
    setEditingSession(undefined);
    setUsername(data.profile.username);
    setBadge(data.profile.badge_id);
    setWebPreference(readPreferences());
    const read = async () => {
      const results = await Promise.allSettled([
        rpc<SettingsSnapshot>(data.token, "settings/get", {}, abort.signal),
        rpc<{ sessions: SessionSummary[] }>(
          data.token,
          "sessions/list",
          { all: true },
          abort.signal,
        ),
        rpc<Memories>(data.token, "memory/list", {}, abort.signal),
      ]);
      if (abort.signal.aborted || serial !== generation.current) return;
      const [settings, sessions, memory] = results;
      if (settings.status === "fulfilled") {
        setRemote(settings.value);
        setDraft({
          ...settings.value.settings,
          model: settings.value.effective.model,
          thinking: settings.value.effective.thinking,
        });
      }
      if (sessions.status === "fulfilled")
        setAllSessions(sessions.value.sessions);
      if (memory.status === "fulfilled") setMemories(memory.value);
      const failures = results.filter((result) => result.status === "rejected");
      if (failures.length)
        setError(
          failures
            .map((result) =>
              errorText((result as PromiseRejectedResult).reason),
            )
            .join("；"),
        );
      setLoading(false);
    };
    void read();
    return () => {
      ++generation.current;
      abort.abort();
    };
  }, [open, data?.token]);

  useEffect(() => {
    const audio = voice.current;
    return () => {
      audio?.pause();
    };
  }, [current?.id, open]);

  function local(patch: Partial<UiPreferences>) {
    try {
      patchUi(patch);
      setNotice("已保存到当前浏览器。");
      setError("");
    } catch (failure) {
      setError(errorText(failure));
    }
  }
  async function perform(
    label: string,
    action: (isCurrent: () => boolean) => Promise<void>,
  ) {
    if (operation.current) return false;
    const serial = generation.current;
    const isCurrent = () => generation.current === serial;
    operation.current = true;
    setBusy(label);
    setNotice("");
    setError("");
    try {
      await action(isCurrent);
      if (isCurrent()) setNotice("已保存。");
      return isCurrent();
    } catch (failure) {
      if (generation.current === serial && !isAbort(failure))
        setError(errorText(failure));
      return false;
    } finally {
      operation.current = false;
      setBusy("");
    }
  }
  async function saveSettings(patch: Partial<EditableSettings>) {
    if (!data) return;
    await perform("保存设置", async (isCurrent) => {
      const result = await rpc<SettingsSnapshot>(
        data.token,
        "settings/update",
        { patch },
      );
      if (!isCurrent()) return;
      setRemote(result);
      setData(
        (previous) =>
          previous && {
            ...previous,
            model: { ...previous.model, name: result.effective.model },
          },
      );
    });
  }
  async function reloadLists(isCurrent: () => boolean) {
    if (!data) return;
    const [sessions, memory, settings] = await Promise.all([
      rpc<{ sessions: SessionSummary[] }>(data.token, "sessions/list", {
        all: true,
      }),
      rpc<Memories>(data.token, "memory/list"),
      rpc<SettingsSnapshot>(data.token, "settings/get"),
    ]);
    if (!isCurrent()) return;
    setAllSessions(sessions.sessions);
    setMemories(memory);
    setRemote(settings);
    await harness.refreshSessions();
  }
  async function refreshNativeSettings() {
    if (!data) return;
    const serial = generation.current;
    const request = ++nativeRefresh.current;
    try {
      const [settings, state] = await Promise.all([
        rpc<SettingsSnapshot>(data.token, "settings/get"),
        bootstrap(),
      ]);
      if (serial !== generation.current || request !== nativeRefresh.current)
        return;
      setRemote(settings);
      setDraft((previous) => previous && {
        ...previous,
        model: settings.effective.model,
        thinking: settings.effective.thinking,
      });
      setData(state);
      await harness.refreshSessions();
    } catch (failure) {
      if (serial === generation.current && request === nativeRefresh.current)
        setError(`刷新工作台失败：${errorText(failure)}`);
    }
  }
  function confirmAction(
    title: string,
    text: string,
    method: string,
    params: Record<string, unknown> = {},
  ) {
    if (!data) return;
    setConfirmation({
      title,
      text,
      action: async () => {
        const saved = await perform(title, async (isCurrent) => {
          await rpc(data.token, method, params);
          if (isCurrent()) await reloadLists(isCurrent);
        });
        if (saved) setConfirmation(undefined);
      },
    });
  }
  async function restoreSession(id: string) {
    if (!data) return;
    await perform("恢复会话", async (isCurrent) => {
      await rpc(data.token, "sessions/unarchive", { id });
      if (isCurrent()) await reloadLists(isCurrent);
    });
  }
  async function exportSessions(format: "json" | "markdown", id?: string) {
    if (!data) return;
    await perform("导出会话", async (isCurrent) => {
      const result = await rpc<{
        filename: string;
        content: string;
        mime: string;
      }>(data.token, "sessions/export", { format, ...(id ? { id } : {}) });
      if (!isCurrent()) return;
      const url = URL.createObjectURL(
        new Blob([result.content], {
          type:
            result.mime ||
            (format === "json" ? "application/json" : "text/markdown"),
        }),
      );
      const link = document.createElement("a");
      link.href = url;
      link.download = result.filename;
      document.body.append(link);
      link.click();
      link.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    });
  }
  function updateWeb(next: WebPreferences) {
    try {
      savePreferences(next);
      setWebPreference(next);
      setNotice("已保存到当前浏览器。");
      setError("");
    } catch (failure) {
      setError(errorText(failure));
    }
  }
  async function nextCli(enabled: boolean) {
    if (!data) return;
    await perform("保存终端开场", async (isCurrent) => {
      await post("/api/startup-next", data.token, { enabled });
      if (!isCurrent()) return;
      setData(
        (previous) =>
          previous && {
            ...previous,
            startup: { ...previous.startup, next_enabled: enabled },
          },
      );
    });
  }
  async function enableDesktopNotifications() {
    if (typeof Notification === "undefined") return;
    try {
      const result = await Notification.requestPermission();
      setPermission(result);
      if (result === "granted") local({ desktopNotifications: true });
    } catch (failure) {
      setError(errorText(failure));
    }
  }
  const saveButton = (patch: Partial<EditableSettings>) => (
    <div className="sc-form-actions">
      <button
        className="sc-button sc-primary"
        disabled={blocked || !draft}
        onClick={() => void saveSettings(patch)}
      >
        {busy === "保存设置" ? "正在保存…" : "保存更改"}
      </button>
      <span>保存到本机 DSH 设置</span>
    </div>
  );

  function sessionManager() {
    const visible = allSessions.filter((session) =>
      sessionName(session)
        .toLocaleLowerCase()
        .includes(sessionSearch.toLocaleLowerCase()),
    );
    return (
      <div className="sc-session-manager">
        <input
          className="sc-input"
          aria-label="搜索聊天"
          placeholder="搜索聊天名称"
          value={sessionSearch}
          onChange={(event) => setSessionSearch(event.target.value)}
        />
        {visible.map((session) => (
          <div className="sc-managed-session" key={session.id}>
            {editingSession?.id === session.id ? (
              <form
                className="sc-rename-form"
                onSubmit={async (event) => {
                  event.preventDefault();
                  if (!data || !editingSession) return;
                  const saved = await perform(
                    "重命名聊天",
                    async (isCurrent) => {
                      await rpc(data.token, "sessions/rename", {
                        id: session.id,
                        name: editingSession.name.trim(),
                      });
                      if (isCurrent()) await reloadLists(isCurrent);
                    },
                  );
                  if (saved) setEditingSession(undefined);
                }}
              >
                <input
                  className="sc-input"
                  aria-label="聊天名称"
                  value={editingSession.name}
                  maxLength={100}
                  required
                  disabled={blocked}
                  onChange={(event) =>
                    setEditingSession({
                      id: session.id,
                      name: event.target.value,
                    })
                  }
                />
                <button
                  className="sc-button"
                  disabled={blocked || !editingSession.name.trim()}
                >
                  保存名称
                </button>
                <button
                  type="button"
                  className="sc-button"
                  disabled={blocked}
                  onClick={() => setEditingSession(undefined)}
                >
                  取消
                </button>
              </form>
            ) : (
              <div className="sc-session-heading">
                <strong>{sessionName(session)}</strong>
                <small>
                  {date(session.updated_at)} ·{" "}
                  {session.archived ? "已归档" : "未归档"}
                </small>
              </div>
            )}
            <div className="sc-actions">
              <button
                className="sc-button"
                disabled={blocked}
                aria-label={`重命名 ${sessionName(session)}`}
                onClick={() =>
                  setEditingSession({
                    id: session.id,
                    name: session.name || "",
                  })
                }
              >
                重命名
              </button>
              <button
                className="sc-button"
                disabled={blocked}
                aria-label={`${session.archived ? "恢复" : "归档"} ${sessionName(session)}`}
                onClick={() => {
                  if (session.archived) void restoreSession(session.id);
                  else
                    confirmAction(
                      "归档这段会话？",
                      "从会话侧栏隐藏，记录仍会保留，可以恢复。",
                      "sessions/archive",
                      { id: session.id },
                    );
                }}
              >
                {session.archived ? "恢复" : "归档"}
              </button>
              <button
                className="sc-button"
                disabled={blocked}
                aria-label={`导出 ${sessionName(session)}`}
                onClick={() => void exportSessions("markdown", session.id)}
              >
                导出
              </button>
              <button
                className="sc-button sc-danger"
                disabled={blocked}
                aria-label={`删除 ${sessionName(session)}`}
                onClick={() =>
                  confirmAction(
                    "删除这段会话？",
                    "这段会话的聊天快照将被永久删除，无法恢复。追加式审计事件和工作区文件仍会保留。",
                    "sessions/delete",
                    { id: session.id },
                  )
                }
              >
                删除
              </button>
            </div>
          </div>
        ))}
        {!visible.length && (
          <p className="sc-description">
            {allSessions.length ? "没有匹配的聊天。" : "当前没有聊天记录。"}
          </p>
        )}
      </div>
    );
  }

  function panel() {
    if (!current)
      return (
        <div className="sc-empty">
          <MagnifyingGlassIcon />
          <h3>没有找到相关设置</h3>
          <p>试试“主题”“记忆”或“启动动画”。</p>
        </div>
      );
    switch (current.id) {
      case "general":
        return (
          <>
            <Section title="模型与响应">
              <Row
                title="默认模型"
                description="使用当前模型服务支持的模型名称。"
              >
                <input
                  className="sc-input sc-model-input"
                  aria-label="默认模型"
                  value={draft?.model || ""}
                  onChange={(event) =>
                    setDraft(
                      (previous) =>
                        previous && { ...previous, model: event.target.value },
                    )
                  }
                  disabled={blocked || !draft}
                />
              </Row>
              <Row
                title="思考模式"
                description="由当前模型及后端决定是否支持。"
              >
                <Toggle
                  label="思考模式"
                  checked={draft?.thinking === true}
                  onChange={(thinking) =>
                    setDraft(
                      (previous) => previous && { ...previous, thinking },
                    )
                  }
                  disabled={blocked || !draft}
                />
              </Row>
              {saveButton({
                model: draft?.model?.trim(),
                thinking: draft?.thinking,
              })}
            </Section>
            <Section>
              <Row title="界面语言" description="当前 DSH 界面提供简体中文。">
                <span className="sc-value">简体中文</span>
              </Row>
              <Row title="当前模型服务">
                <span className="sc-value">
                  {remote?.effective.backend || data?.model.backend || "—"}
                </span>
              </Row>
              <Row title="当前工作区">
                <span className="sc-path" title={data?.workspace}>
                  {data?.workspace || "—"}
                </span>
              </Row>
            </Section>
          </>
        );
      case "appearance":
        return (
          <>
            <Section>
              <Row title="主题">
                <Select
                  label="主题"
                  value={ui.theme}
                  onChange={(theme) =>
                    local({ theme: theme as UiPreferences["theme"] })
                  }
                  options={[
                    ["system", "跟随系统"],
                    ["light", "浅色"],
                    ["dark", "深色"],
                  ]}
                />
              </Row>
              <Row title="强调色">
                <div className="sc-colors" role="group" aria-label="强调色">
                  {[
                    ["#3679af", "冷蓝"],
                    ["#0f8b8d", "青绿"],
                    ["#7560c4", "紫色"],
                    ["#c2743a", "琥珀"],
                    ["#b85677", "玫瑰"],
                    ["#61656a", "石墨"],
                  ].map(([color, label]) => (
                    <button
                      key={color}
                      className="sc-color"
                      style={{ background: color }}
                      aria-label={label}
                      aria-pressed={ui.accent === color}
                      onClick={() => local({ accent: color })}
                    >
                      {ui.accent === color && <CheckIcon />}
                    </button>
                  ))}
                </div>
              </Row>
              <Row title="界面密度">
                <Select
                  label="界面密度"
                  value={ui.density}
                  onChange={(density) =>
                    local({ density: density as UiPreferences["density"] })
                  }
                  options={[
                    ["comfortable", "舒适"],
                    ["compact", "紧凑"],
                  ]}
                />
              </Row>
              <Row title="文字大小">
                <Select
                  label="文字大小"
                  value={ui.fontSize}
                  onChange={(fontSize) =>
                    local({ fontSize: fontSize as UiPreferences["fontSize"] })
                  }
                  options={[
                    ["small", "较小"],
                    ["medium", "标准"],
                    ["large", "较大"],
                  ]}
                />
              </Row>
              <Row title="减少动画">
                <Select
                  label="减少动画"
                  value={ui.reducedMotion}
                  onChange={(reducedMotion) =>
                    local({
                      reducedMotion:
                        reducedMotion as UiPreferences["reducedMotion"],
                    })
                  }
                  options={[
                    ["system", "跟随系统"],
                    ["reduce", "开启"],
                  ]}
                />
              </Row>
              <Row
                title="显示模型思考"
                description="仅展示模型返回的思考内容；没有返回时不生成额外内容。"
              >
                <Toggle
                  label="显示模型思考"
                  checked={ui.showThinking}
                  onChange={(showThinking) => local({ showThinking })}
                />
              </Row>
              <Row title="默认展开工具详情">
                <Toggle
                  label="默认展开工具详情"
                  checked={ui.showToolDetails}
                  onChange={(showToolDetails) => local({ showToolDetails })}
                />
              </Row>
            </Section>
            <div className="sc-appearance-preview">
              <span className="sc-preview-avatar">
                <Emblem size={24} />
              </span>
              <div>
                <strong>DSH Harness</strong>
                <p>清晰、直接，专注于你的任务。</p>
              </div>
              <span className="sc-preview-chip">界面预览</span>
            </div>
          </>
        );
      case "notifications":
        return (
          <Section>
            <Row title="任务完成提醒" description="任务结束时显示通知。">
              <Toggle
                label="任务完成提醒"
                checked={ui.notifyOnCompletion}
                onChange={(notifyOnCompletion) => local({ notifyOnCompletion })}
              />
            </Row>
            <Row title="工具审批提醒" description="有工具等待授权时提醒你。">
              <Toggle
                label="工具审批提醒"
                checked={ui.notifyOnApproval}
                onChange={(notifyOnApproval) => local({ notifyOnApproval })}
              />
            </Row>
            <Row title="通知提示音">
              <Toggle
                label="通知提示音"
                checked={ui.notificationSound}
                onChange={(notificationSound) => local({ notificationSound })}
              />
            </Row>
            <Row
              title="系统桌面通知"
              description={
                permission === "denied"
                  ? "浏览器已阻止通知，可在浏览器网站权限中修改。"
                  : permission === "unsupported"
                    ? "当前浏览器不支持系统通知。"
                    : "页面在后台且浏览器仍打开时提醒；关闭页面后不会推送。"
              }
            >
              {permission === "granted" ? (
                <Toggle
                  label="系统桌面通知"
                  checked={ui.desktopNotifications}
                  onChange={(desktopNotifications) =>
                    local({ desktopNotifications })
                  }
                />
              ) : permission === "default" ? (
                <button
                  className="sc-button"
                  onClick={() => void enableDesktopNotifications()}
                >
                  允许通知
                </button>
              ) : (
                <span className="sc-value">
                  {permission === "denied" ? "已阻止" : "不支持"}
                </span>
              )}
            </Row>
          </Section>
        );
      case "profile":
        return (
          <form
            onSubmit={async (event: FormEvent) => {
              event.preventDefault();
              if (!data) return;
              await perform("保存个人资料", async (isCurrent) => {
                const profile = await post<Profile>(
                  "/api/profile",
                  data.token,
                  { username, badge_id: badge },
                );
                if (!isCurrent()) return;
                setData((previous) => previous && { ...previous, profile });
                setUsername(profile.username);
                setBadge(profile.badge_id);
              });
            }}
          >
            <div className="sc-profile-intro">
              <span className="sc-profile-avatar">
                {username.slice(0, 1).toUpperCase() || "D"}
              </span>
              <div>
                <h3>{data?.profile.username || "操作员"}</h3>
                <p>本机操作员资料</p>
              </div>
            </div>
            <Section>
              <label className="sc-field">
                显示名称
                <input
                  className="sc-input"
                  required
                  maxLength={32}
                  value={username}
                  onChange={(event) => setUsername(event.target.value)}
                  autoComplete="nickname"
                  disabled={blocked}
                />
              </label>
              <label className="sc-field">
                档案编号
                <input
                  className="sc-input"
                  required
                  maxLength={32}
                  value={badge}
                  onChange={(event) => setBadge(event.target.value)}
                  disabled={blocked}
                />
              </label>
              <p className="sc-description">名称会出现在工作台和启动动画中。</p>
              <div className="sc-form-actions">
                <button
                  type="submit"
                  className="sc-button sc-primary"
                  disabled={blocked}
                >
                  {busy ? "正在保存…" : "保存资料"}
                </button>
              </div>
            </Section>
          </form>
        );
      case "security":
        return (
          <>
            <Section title="本机执行权限">
              <Row title="工具审批">
                <Select
                  label="工具审批"
                  value={draft?.approval || "on-request"}
                  onChange={(approval) =>
                    setDraft(
                      (previous) => previous && { ...previous, approval },
                    )
                  }
                  disabled={blocked || !draft}
                  options={[
                    ["on-request", "需要时询问"],
                    ["untrusted", "非读取操作均询问"],
                    ["never", "不询问"],
                  ]}
                />
              </Row>
              <Row title="沙箱范围">
                <Select
                  label="沙箱范围"
                  value={draft?.sandbox || "workspace-write"}
                  onChange={(sandbox) =>
                    setDraft((previous) => previous && { ...previous, sandbox })
                  }
                  disabled={blocked || !draft}
                  options={[
                    ["read-only", "只读"],
                    ["workspace-write", "工作区读写"],
                    ["danger-full-access", "完全访问"],
                  ]}
                />
              </Row>
              <Row
                title="安全研究模式"
                description="调整任务提示方式，执行权限仍由上述设置决定。"
              >
                <Toggle
                  label="安全研究模式"
                  checked={draft?.security_research_mode === true}
                  onChange={(security_research_mode) =>
                    setDraft(
                      (previous) =>
                        previous && { ...previous, security_research_mode },
                    )
                  }
                  disabled={blocked || !draft}
                />
              </Row>
              {saveButton({
                approval: draft?.approval,
                sandbox: draft?.sandbox,
                security_research_mode: draft?.security_research_mode,
              })}
            </Section>
          </>
        );
      case "model-service":
        return data ? <ModelServiceSettings token={data.token} onChanged={refreshNativeSettings} /> : null;
      case "archived":
        return (
          <Section description="归档会话保留记录，可随时恢复到侧栏。">
            {archived.length ? (
              <div className="sc-session-list">
                {archived.map((session) => (
                  <div className="sc-session" key={session.id}>
                    <div>
                      <strong>{sessionName(session)}</strong>
                      <small>
                        {date(session.updated_at)} · {session.event_count}{" "}
                        条记录
                      </small>
                    </div>
                    <button
                      className="sc-button"
                      disabled={blocked}
                      onClick={() => void restoreSession(session.id)}
                    >
                      恢复
                    </button>
                    <button
                      className="sc-icon-button sc-danger"
                      aria-label={`删除${sessionName(session)}`}
                      disabled={blocked}
                      onClick={() =>
                        confirmAction(
                          "删除这段会话？",
                          "这段会话的聊天快照将被永久删除，无法恢复。追加式审计事件和工作区文件仍会保留。",
                          "sessions/delete",
                          { id: session.id },
                        )
                      }
                    >
                      <TrashIcon />
                    </button>
                  </div>
                ))}
              </div>
            ) : (
              <div className="sc-empty">
                <ArchiveIcon />
                <h3>暂无已归档聊天</h3>
                <p>归档后的会话会显示在这里。</p>
              </div>
            )}
          </Section>
        );
      case "voice":
        return (
          <>
            <Section>
              <Row title="启动旁白">
                <span className="sc-value">DSH 固定英文启动旁白</span>
              </Row>
              <Row title="旁白语言">
                <span className="sc-value">English</span>
              </Row>
              <Row title="试听步骤">
                <Select
                  label="试听步骤"
                  value={voicePhase}
                  onChange={(phase) => {
                    voice.current?.pause();
                    setVoicePhase(phase);
                  }}
                  options={[
                    ["phase-0", "01 · 启动序列"],
                    ["phase-1", "02 · 本机工作区"],
                    ["phase-2", "03 · 操作员身份"],
                    ["phase-3-mounted", "04 · 已挂载能力"],
                    ["phase-4", "05 · 加载完成"],
                    ["phase-5", "06 · 欢迎操作员"],
                    ["load-warning", "加载结果 · 部分异常"],
                    ["load-unavailable", "加载结果 · 无法读取"],
                  ]}
                />
              </Row>
              <div className="sc-audio">
                <span>试听所选旁白</span>
                <audio
                  key={voicePhase}
                  ref={voice}
                  controls
                  preload="none"
                  src={`/assets/voice/${voicePhase}.wav`}
                  aria-label="试听 DSH 英文开场旁白"
                  onError={() =>
                    setError("旁白音频暂时无法读取，请确认本机资源已安装。")
                  }
                />
              </div>
              <p className="sc-description">
                播放固定英文录音，不使用系统语音合成。
              </p>
            </Section>
          </>
        );
      case "storage":
        return (
          <>
            <div className="sc-storage-total">
              <strong>{bytes(remote?.storage.total_bytes)}</strong>
              <span>本机 DSH 数据</span>
            </div>
            <Section>
              <Row
                title="会话记录"
                description={
                  remote
                    ? `${remote.storage.session_count} 个会话，其中 ${remote.storage.archived_count} 个已归档`
                    : "正在读取"
                }
              >
                <span className="sc-value">
                  {bytes(remote?.storage.session_bytes)}
                </span>
              </Row>
              <Row title="学习记忆">
                <span className="sc-value">
                  {bytes(remote?.storage.memory_bytes)}
                </span>
              </Row>
              <Row title="运行事件日志">
                <span className="sc-value">
                  {bytes(remote?.storage.event_bytes)}
                </span>
              </Row>
            </Section>
            <div className="sc-actions">
              <button
                className="sc-button"
                onClick={() => {
                  setSelected("data");
                  setSearch("");
                }}
              >
                管理会话数据 <ChevronRightIcon />
              </button>
              <button
                className="sc-button"
                onClick={() => {
                  setSelected("personalization");
                  setSearch("");
                  setManageMemory(true);
                }}
              >
                管理记忆
              </button>
            </div>
          </>
        );
      case "personalization":
        return (
          <>
            <Section title="基础风格">
              <Row title="回应风格">
                <Select
                  label="回应风格"
                  value={draft?.personality || "default"}
                  onChange={(personality) =>
                    setDraft(
                      (previous) => previous && { ...previous, personality },
                    )
                  }
                  disabled={blocked || !draft}
                  options={[
                    ["default", "默认"],
                    ["concise", "简洁"],
                    ["explanatory", "详细解释"],
                    ["collaborative", "协作"],
                    ["friendly", "友好"],
                  ]}
                />
              </Row>
              {(
                [
                  ["warmth", "温暖程度"],
                  ["enthusiasm", "热情程度"],
                  ["headers_lists", "标题与列表"],
                  ["emoji", "表情符号"],
                ] as const
              ).map(([key, label]) => (
                <Row title={label} key={key}>
                  <Select
                    label={label}
                    value={draft?.characteristics?.[key] || "default"}
                    onChange={(value) =>
                      setDraft(
                        (previous) =>
                          previous && {
                            ...previous,
                            characteristics: {
                              ...defaultCharacteristics,
                              ...previous.characteristics,
                              [key]: value as Characteristic,
                            },
                          },
                      )
                    }
                    disabled={blocked || !draft}
                    options={[
                      ["less", "更少"],
                      ["default", "默认"],
                      ["more", "更多"],
                    ]}
                  />
                </Row>
              ))}
            </Section>
            <Section
              title="自定义指令"
              description="告诉 Harness 你希望它了解的背景、回应方式和工作偏好。"
            >
              <textarea
                className="sc-textarea"
                aria-label="自定义指令"
                value={draft?.custom_instructions || ""}
                onChange={(event) =>
                  setDraft(
                    (previous) =>
                      previous && {
                        ...previous,
                        custom_instructions: event.target.value,
                      },
                  )
                }
                maxLength={8000}
                rows={5}
                disabled={blocked || !draft}
                placeholder="例如：用中文回复，先给结论，再解释关键依据。"
              />
              <div className="sc-counter">
                {Array.from(draft?.custom_instructions || "").length} / 8000
              </div>
            </Section>
            <Section title="记忆">
              {remote?.effective.memory_available === false && (
                <p className="sc-info">
                  本机全局配置已关闭学习记忆。以下偏好仍可保存，但当前运行时不会读取或生成记忆。
                </p>
              )}
              {remote?.effective.memory_available && (
                <p className="sc-description">
                  当前运行时：使用已保存记忆
                  {remote.effective.memory_inject ? "已开启" : "已关闭"}
                  ，保存新经验
                  {remote.effective.memory_generate ? "已开启" : "已关闭"}。
                </p>
              )}
              <Row title="在任务中使用已保存记忆">
                <Toggle
                  label="在任务中使用已保存记忆"
                  checked={draft?.memory_inject === true}
                  onChange={(memory_inject) =>
                    setDraft(
                      (previous) => previous && { ...previous, memory_inject },
                    )
                  }
                  disabled={blocked || !draft}
                />
              </Row>
              <Row title="保存新的任务经验">
                <Toggle
                  label="保存新的任务经验"
                  checked={draft?.memory_generate === true}
                  onChange={(memory_generate) =>
                    setDraft(
                      (previous) =>
                        previous && { ...previous, memory_generate },
                    )
                  }
                  disabled={blocked || !draft}
                />
              </Row>
              <Row
                title="管理记忆"
                description={
                  memories
                    ? `${memories.total} 条工具经验 · ${memories.feedback_total} 条任务反馈`
                    : "读取本机学习记录"
                }
              >
                <button
                  className="sc-button"
                  aria-expanded={manageMemory}
                  onClick={() => setManageMemory((value) => !value)}
                >
                  {manageMemory ? "收起" : "管理"}
                </button>
              </Row>
              {manageMemory && (
                <div className="sc-memory-manager">
                  <input
                    className="sc-input"
                    aria-label="搜索记忆"
                    placeholder="搜索任务或内容"
                    value={memorySearch}
                    onChange={(event) => setMemorySearch(event.target.value)}
                  />
                  {memories &&
                    [
                      ...memories.memories.map((item) => ({
                        ...item,
                        kind: "episode",
                      })),
                      ...memories.feedback.map((item) => ({
                        ...item,
                        kind: "feedback",
                      })),
                    ]
                      .filter((item) =>
                        `${item.query || item.goal} ${item.note} ${item.tool}`
                          .toLowerCase()
                          .includes(memorySearch.toLowerCase()),
                      )
                      .map((item) => (
                        <details
                          className="sc-memory"
                          key={`${item.kind}-${item.id}`}
                        >
                          <summary>
                            <span>
                              {item.query ||
                                item.goal ||
                                item.tool ||
                                "任务经验"}
                              <small>
                                {date(item.at)} ·{" "}
                                {item.kind === "episode"
                                  ? "工具经验"
                                  : "任务反馈"}
                              </small>
                            </span>
                            <ChevronRightIcon />
                          </summary>
                          <p>{item.note || "没有附加说明。"}</p>
                          <button
                            className="sc-button sc-danger"
                            disabled={blocked}
                            onClick={() =>
                              confirmAction(
                                "删除这条记忆？",
                                "该记录将从本机学习数据中移除。",
                                "memory/delete",
                                { id: item.id, kind: item.kind },
                              )
                            }
                          >
                            删除这条记忆
                          </button>
                        </details>
                      ))}
                  {memories &&
                    memories.total + memories.feedback_total === 0 && (
                      <p className="sc-description">目前没有已保存的记忆。</p>
                    )}
                  <button
                    className="sc-button sc-danger"
                    disabled={
                      blocked ||
                      !memories ||
                      memories.total + memories.feedback_total === 0
                    }
                    onClick={() =>
                      confirmAction(
                        "清除所有记忆？",
                        "工具经验、任务反馈，以及学习到的路由权重和关联记录都会清除。此操作无法撤销。",
                        "memory/clear",
                      )
                    }
                  >
                    清除全部记忆
                  </button>
                </div>
              )}
              {saveButton({
                personality: draft?.personality,
                characteristics:
                  draft?.characteristics || defaultCharacteristics,
                custom_instructions: draft?.custom_instructions || "",
                memory_inject: draft?.memory_inject,
                memory_generate: draft?.memory_generate,
              })}
            </Section>
          </>
        );
      case "pets":
        return (
          <>
            <Section>
              <Row
                title="工作台宠物"
                description="待机、工作、等待确认和完成时显示对应动作，也可以点击打招呼。"
              >
                <Select
                  label="工作台宠物"
                  value={ui.pet}
                  onChange={(pet) =>
                    local({ pet: pet as UiPreferences["pet"] })
                  }
                  options={[
                    ["none", "关闭"],
                    ["fat-fish", "DeepSeek 大肥鱼"],
                  ]}
                />
              </Row>
              <div className="sc-pet-preview"><Companion working={false} preview /><p className="sc-description">使用原作者 gmskywalker 提供的蓝色大肥鱼动画图集。启用“减少动态效果”时，保留静态姿态与文字回应。</p><a href="https://github.com/gmskywalker/deepseek-fat-fish-codex-pet" target="_blank" rel="noreferrer noopener">查看原作与同人作品声明 ↗</a></div>
            </Section>
          </>
        );
      case "keyboard":
        return (
          <>
            <Section>
              <Row title="发送消息">
                <Select
                  label="发送消息快捷键"
                  value={ui.sendKey}
                  onChange={(sendKey) =>
                    local({ sendKey: sendKey as UiPreferences["sendKey"] })
                  }
                  options={[
                    ["enter", "Enter"],
                    ["mod-enter", "Ctrl / ⌘ + Enter"],
                  ]}
                />
              </Row>
              <Row title="消息换行">
                <kbd>Shift + Enter</kbd>
              </Row>
            </Section>
            <Section title="工作台快捷键">
              <Row title="打开设置">
                <kbd>Ctrl / ⌘ + ,</kbd>
              </Row>
              <Row title="新建会话">
                <kbd>Ctrl / ⌘ + Shift + O</kbd>
              </Row>
              <Row title="聚焦输入框">
                <kbd>Ctrl / ⌘ + Shift + L</kbd>
              </Row>
            </Section>
          </>
        );
      case "usage":
        return (
          <>
            <div className="sc-stats">
              <div>
                <strong>{remote?.usage.session_count ?? "—"}</strong>
                <span>本机会话</span>
              </div>
              <div>
                <strong>{remote?.usage.message_count ?? "—"}</strong>
                <span>聊天消息</span>
              </div>
              <div>
                <strong>{remote?.usage.tool_call_count ?? "—"}</strong>
                <span>工具调用</span>
              </div>
            </div>
            <Section>
              <Row title="用户消息">
                <span className="sc-value">
                  {remote?.usage.user_message_count ?? "—"}
                </span>
              </Row>
              <Row title="助手消息">
                <span className="sc-value">
                  {remote?.usage.assistant_message_count ?? "—"}
                </span>
              </Row>
              <Row title="审计事件">
                <span className="sc-value">
                  {remote?.usage.event_count ?? "—"}
                </span>
              </Row>
              <Row title="当前模型">
                <span className="sc-value">
                  {remote?.effective.model || data?.model.name || "—"}
                </span>
              </Row>
              <Row title="已挂载 Skills">
                <span className="sc-value">{data?.skills.length ?? "—"}</span>
              </Row>
              <Row title="数据占用">
                <span className="sc-value">
                  {bytes(remote?.storage.total_bytes)}
                </span>
              </Row>
            </Section>
            <p className="sc-info">
              以上统计来自本机记录。模型
              Token、额度和费用需从实际模型服务获取；当前接口未提供统一账单数据。
            </p>
          </>
        );
      case "data":
        return (
          <>
            <Section title="本机会话">
              <Row
                title="管理聊天"
                description="重命名、归档、导出或删除单个会话。"
              >
                <button
                  className="sc-button"
                  aria-expanded={manageSessions}
                  onClick={() => setManageSessions((value) => !value)}
                >
                  {manageSessions ? "收起聊天列表" : "管理聊天"}
                </button>
              </Row>
              {manageSessions && sessionManager()}
              <Row
                title="已归档聊天"
                description={`${archived.length} 个已归档会话`}
              >
                <button
                  className="sc-button"
                  onClick={() => {
                    setSelected("archived");
                    setSearch("");
                  }}
                >
                  管理
                </button>
              </Row>
              <Row
                title="归档所有聊天"
                description="从会话侧栏隐藏，记录仍保留。"
              >
                <button
                  className="sc-button"
                  disabled={blocked}
                  onClick={() =>
                    confirmAction(
                      "归档所有会话？",
                      "会话记录仍会保留，可在已归档聊天中恢复。",
                      "sessions/archive_all",
                    )
                  }
                >
                  全部归档
                </button>
              </Row>
              <Row
                title="删除所有聊天"
                description="删除会话快照，保留追加式审计事件和工作区文件。"
              >
                <button
                  className="sc-button sc-danger"
                  disabled={blocked}
                  onClick={() =>
                    confirmAction(
                      "删除所有会话？",
                      "所有聊天快照将被永久删除，无法恢复。追加式审计事件和工作区文件仍会保留。",
                      "sessions/delete_all",
                    )
                  }
                >
                  全部删除
                </button>
              </Row>
              <Row
                title="导出数据"
                description="下载会话快照，不包含配置或凭据。"
              >
                <div className="sc-actions">
                  <button
                    className="sc-button"
                    disabled={blocked}
                    onClick={() => void exportSessions("json")}
                  >
                    <DownloadIcon />
                    JSON
                  </button>
                  <button
                    className="sc-button"
                    disabled={blocked}
                    onClick={() => void exportSessions("markdown")}
                  >
                    Markdown
                  </button>
                </div>
              </Row>
            </Section>
            <Section title="浏览器偏好">
              <Row
                title="重置界面设置"
                description="恢复主题、通知和显示偏好，不删除会话、个人资料或启动开关。"
              >
                <button
                  className="sc-button"
                  onClick={() =>
                    setConfirmation({
                      title: "重置界面设置？",
                      text: "仅恢复当前浏览器中的界面偏好，会话和本机设置不会删除。",
                      action: async () => {
                        try {
                          resetUiPreferences();
                          setNotice("已恢复默认界面设置。");
                        } catch (failure) {
                          setError(errorText(failure));
                        }
                        setConfirmation(undefined);
                      },
                    })
                  }
                >
                  恢复默认
                </button>
              </Row>
            </Section>
          </>
        );
      case "plugins":
        return data ? <ExtensionSettings token={data.token} onChanged={refreshNativeSettings} /> : null;
      case "workspace":
      case "tasks":
      case "code-review":
        return data ? <WorkspaceSettings token={data.token} section={current.id === "code-review" ? "review" : current.id} onChanged={refreshNativeSettings} /> : null;
      case "startup":
        return (
          <>
            <Section title="网页启动动画">
              <Row
                title="每次打开网页时播放"
                description="动画需要点击画面或按 Enter 开始。"
              >
                <Toggle
                  label="每次打开网页时播放"
                  checked={
                    webPreference.enabled ?? data?.startup.enabled ?? false
                  }
                  onChange={(enabled) =>
                    updateWeb({ ...webPreference, enabled })
                  }
                />
              </Row>
              <Row
                title="仅下一次打开网页时播放"
                description="只使用一次，之后自动清除。"
              >
                <Toggle
                  label="仅下一次打开网页时播放"
                  checked={webPreference.next}
                  onChange={(next) => updateWeb({ ...webPreference, next })}
                />
              </Row>
              <Row
                title="立即体验"
                description="保留当前会话与草稿，播放结束后返回。"
              >
                <button
                  className="sc-button"
                  onClick={() => {
                    voice.current?.pause();
                    onOpenChange(false);
                    onReplay();
                  }}
                >
                  <PlayIcon />
                  播放开场
                </button>
              </Row>
            </Section>
            <Section title="下一次终端启动">
              <Row
                title="CLI 一次性设置"
                description="写入本机，网页不会消耗此设置。"
              >
                <span className="sc-value">
                  {data?.startup.next_enabled == null
                    ? `跟随配置（${data?.startup.enabled ? "开启" : "关闭"}）`
                    : data.startup.next_enabled
                      ? "下一次开启"
                      : "下一次关闭"}
                </span>
              </Row>
              <div className="sc-actions">
                <button
                  className="sc-button"
                  disabled={blocked}
                  onClick={() => void nextCli(true)}
                >
                  下一次开启
                </button>
                <button
                  className="sc-button"
                  disabled={blocked}
                  onClick={() => void nextCli(false)}
                >
                  下一次关闭
                </button>
              </div>
            </Section>
          </>
        );
      default:
        return null;
    }
  }

  return (
    <Dialog.Root open={open} onOpenChange={onOpenChange}>
      <Dialog.Trigger asChild>
        <button className="settings-trigger">
          <GearIcon />
          <span>设置</span>
        </button>
      </Dialog.Trigger>
      <Dialog.Portal>
        <Dialog.Overlay className="sc-overlay" />
        <Dialog.Content
          className="sc-dialog"
          aria-describedby="sc-dialog-description"
        >
          <div className="sc-sidebar">
            <div className="sc-brand">
              <Emblem size={28} />
              <Dialog.Title>设置</Dialog.Title>
            </div>
            <label className="sc-search">
              <MagnifyingGlassIcon />
              <input
                aria-label="搜索设置"
                placeholder="搜索设置"
                value={search}
                onChange={(event) => setSearch(event.target.value)}
              />
              {search && (
                <button
                  type="button"
                  aria-label="清除搜索"
                  onClick={() => setSearch("")}
                >
                  <Cross1Icon />
                </button>
              )}
            </label>
            <nav className="sc-nav" aria-label="设置分类">
              {matches.map((category) => {
                const Icon = category.icon;
                return (
                  <div key={category.id}>
                    {category.group && !search && (
                      <p className="sc-nav-group">{category.group}</p>
                    )}
                    <button
                      className={current?.id === category.id ? "active" : ""}
                      aria-current={
                        current?.id === category.id ? "page" : undefined
                      }
                      onClick={() => {
                        setSelected(category.id);
                        setNotice("");
                      }}
                    >
                      <Icon />
                      <span>{category.label}</span>
                    </button>
                  </div>
                );
              })}
            </nav>
            <select
              className="sc-mobile-nav"
              aria-label="设置分类"
              value={current?.id || ""}
              onChange={(event) => setSelected(event.target.value)}
            >
              {matches.map((category) => (
                <option key={category.id} value={category.id}>
                  {category.label}
                </option>
              ))}
            </select>
            <span className="sc-local-mark">DSH · 本机工作区</span>
          </div>
          <div className="sc-main">
            <header className="sc-heading">
              <div>
                <h2>{current?.label || "搜索设置"}</h2>
                <Dialog.Description id="sc-dialog-description">
                  {current?.english || "Settings search"}
                </Dialog.Description>
              </div>
              <Dialog.Close className="sc-icon-button" aria-label="关闭设置">
                <Cross1Icon />
              </Dialog.Close>
            </header>
            <div className="sc-content" key={current?.id}>
              {loading && (
                <p className="sc-loading" role="status">
                  <ReloadIcon />
                  正在读取本机设置…
                </p>
              )}
              {panel()}
            </div>
            {(error || notice || busy) && (
              <div
                className={`sc-status ${error ? "is-error" : ""}`}
                role={error ? "alert" : "status"}
              >
                {error || (busy ? `${busy}…` : notice)}
              </div>
            )}
          </div>
        </Dialog.Content>
      </Dialog.Portal>
      <Dialog.Root
        open={open && !!confirmation}
        onOpenChange={(value) => {
          if (!value && !busy) setConfirmation(undefined);
        }}
      >
        <Dialog.Portal>
          <Dialog.Overlay className="sc-confirm-overlay" />
          <Dialog.Content className="sc-confirm">
            <Dialog.Title>{confirmation?.title}</Dialog.Title>
            <Dialog.Description>{confirmation?.text}</Dialog.Description>
            {error && (
              <p className="sc-confirm-error" role="alert">
                {error}
              </p>
            )}
            <div className="sc-actions">
              <button
                className="sc-button"
                disabled={!!busy}
                onClick={() => setConfirmation(undefined)}
              >
                取消
              </button>
              <button
                className="sc-button sc-primary"
                disabled={!!busy}
                onClick={() => void confirmation?.action()}
              >
                {busy ? "正在处理…" : "确认"}
              </button>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </Dialog.Root>
  );
}
