export type Profile = { username: string; badge_id: string };
export type Skill = {
  name: string;
  source: string;
  description: string;
  status: string;
};
export type Plugin = {
  id: string;
  name: string;
  tool_count: number;
  status: string;
};
export type SessionSummary = {
  id: string;
  name: string | null;
  archived: boolean;
  updated_at: string | null;
  event_count: number;
  task_id: string | null;
  state: string | null;
};
export type SessionEvent = {
  id: string;
  type: string;
  text?: string;
  at: string;
  name?: string;
  call_id?: string;
  arguments?: unknown;
  ok?: boolean;
  content?: string;
  reasoning?: string | null;
};
export type Session = {
  id: string;
  name: string | null;
  events: SessionEvent[];
  cwd?: string;
};
export type SessionResult = {
  session: Session;
  task_id: string | null;
  state: string | null;
};
export type Bootstrap = {
  token: string;
  workspace: string;
  profile: Profile;
  model: { name: string; backend: string; ready: boolean; local: boolean };
  status: "ready" | "missing_credentials";
  startup: {
    enabled: boolean;
    override_enabled?: boolean | null;
    next_enabled: boolean | null;
    sound: boolean;
    interactive: boolean;
    reduced_motion: boolean;
  };
  inventory_mode: "mounted";
  skills: Skill[];
  plugins: Plugin[];
  sessions: SessionSummary[];
  latest_sequence: number;
};
export type Envelope = {
  sequence: number;
  event_type: string;
  task_id?: string | null;
  turn_id?: string | null;
  occurred_at: string;
  payload: Record<string, unknown>;
};
export type EventBatch = {
  events: Envelope[];
  latest_sequence: number;
  timed_out?: boolean;
};
export type Approval = {
  request_id: string;
  call_id: string;
  name: string;
  summary: string;
  task_id: string | null;
  run_id: string | null;
};
export type Activity = {
  id: string;
  kind: "text" | "tool" | "reasoning";
  text: string;
  name?: string;
  ok?: boolean;
  finished?: boolean;
};
export type LiveTurn = {
  running: boolean;
  taskId: string | null;
  turnId: string | null;
  lastSequence: number;
  entries: Activity[];
  error?: string;
  prompt?: string;
  status?: string;
  requestId?: number;
};
