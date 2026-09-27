// The only network code: same-origin calls to the QuantDesk API (INV-15).

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { Accept: "application/json" };
  if (method !== "GET") headers["X-Requested-With"] = "quantdesk";
  const init: RequestInit = { method, headers, credentials: "same-origin" };
  if (body !== undefined) {
    headers["Content-Type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  const response = await fetch(`/api${path}`, init);
  const text = await response.text();
  const json: unknown = text ? JSON.parse(text) : null;
  if (!response.ok) {
    const err = (json ?? {}) as { code?: string; error?: string };
    throw new ApiError(response.status, err.code ?? "error", err.error ?? response.statusText);
  }
  return json as T;
}

export type Role = "owner" | "viewer";

export interface Me {
  username: string;
  role: Role;
  stepped_up_until: string | null;
}

export interface HaltView {
  id: string;
  kind: string;
  scope: { scope: string; id?: string };
  reason: string;
  started_at: string;
  requires_manual_rearm: boolean;
  active: boolean;
}

export interface Status {
  now: string;
  environment: "development" | "production";
  account: { id: string; name: string; mode: "backtest" | "paper" | "live"; currency: string; live_armed: boolean } | null;
  live_trading_enabled: boolean;
  live_orders_compiled: boolean;
  halt_state_known: boolean;
  entries_halted: boolean;
  active_halts: HaltView[];
  alerts?: Array<{ severity: "critical" | "warning"; code: string; message: string }>;
  live_account?: { id: string; name: string; mode: "backtest" | "paper" | "live"; currency: string; live_armed: boolean } | null;
}

export interface DataIssue {
  issue: "missing_day" | "bar_on_holiday" | "price_jump" | "calendar_unknown";
  date: string;
  previous?: string;
  close?: string;
}

export interface KiteStatus {
  enabled: boolean;
  user_id: string;
  api_key_set: boolean;
  api_secret_set: boolean;
  access_token_set: boolean;
  last_login: { user_id: string; at: string } | null;
  live_configured: boolean;
  live_trading_enabled: boolean;
  live_orders_compiled: boolean;
  redirect_path: string;
}

export interface DecisionSummary {
  seq: number;
  id: string;
  at: string;
  as_of_date: string;
  instrument_id: string;
  symbol: string | null;
  strategy: string;
  strategy_version: number;
  regime: string;
  headline: string;
  reason: { code: string; detail: Record<string, unknown> | null } | null;
  setup_type: string | null;
  entry: string | null;
  stop: string | null;
  target: string | null;
  rr_net: string | null;
  ev_r: string | null;
  quantity: string | null;
  recorded_at: string;
}

export interface StrategyRow {
  version: {
    reference: { strategy_id: string; name: string; version_id: string; version_number: number; logic_version: string; git_sha: string };
    parameters: unknown;
    rr_floor: string;
  };
  stage: { stage: string; resume_to?: string };
}

export interface SettingsField {
  key: string;
  label: string;
  help: string | null;
  kind: "bool" | "integer" | "decimal" | "date" | "time" | "text";
  value: string | number | boolean | null;
}

export interface SettingsSection {
  name: string;
  title: string;
  help: string;
  source: { kind: "file" | "saved"; version?: number; updated_by?: string; updated_at?: string };
  fields: SettingsField[];
  problem: string | null;
}

export interface SettingsView {
  sections: SettingsSection[];
  server: { environment: string; live_trading_enabled: boolean; live_orders_compiled: boolean; account_id: string; note: string };
}

export interface SecretRow {
  name: string;
  provider: string;
  label: string;
  help: string;
  set: boolean;
  readable: boolean;
  updated_at: string | null;
  updated_by: string | null;
}

export const api = {
  me: () => request<Me>("GET", "/auth/me"),
  login: (username: string, password: string) => request<Me>("POST", "/auth/login", { username, password }),
  logout: () => request<unknown>("POST", "/auth/logout"),
  stepUp: (password: string) => request<{ stepped_up_until: string }>("POST", "/auth/step-up", { password }),
  status: () => request<Status>("GET", "/status"),
  decisions: (limit = 50) => request<DecisionSummary[]>("GET", `/decisions?limit=${limit}`),
  decision: (id: string) => request<{ summary: DecisionSummary; record: Record<string, unknown> }>("GET", `/decisions/${encodeURIComponent(id)}`),
  journal: (limit = 100) => request<Array<{ seq: number; kind: string; entry: unknown; recorded_at: string }>>("GET", `/journal?limit=${limit}`),
  halts: () => request<HaltView[]>("GET", "/halts"),
  createHalt: (reason: string) => request<HaltView>("POST", "/halts", { reason }),
  rearm: (id: string) => request<HaltView>("POST", `/halts/${encodeURIComponent(id)}/rearm`),
  strategies: () => request<StrategyRow[]>("GET", "/strategies"),
  strategyEvent: (id: string, event: Record<string, unknown>) => request<{ stage: unknown }>("POST", `/strategies/${encodeURIComponent(id)}/events`, event),
  instruments: () => request<Array<{ id: string; symbol: string; version: number; currency: string }>>("GET", "/instruments"),
  strategyCatalog: () => request<Array<{ logic_version: string; name: string; parameters: Record<string, unknown> }>>("GET", "/strategy-catalog"),
  backtest: (body: { instrument: string; from: string; to: string; equity: string; logic_version?: string }) => request<Record<string, unknown>>("POST", "/backtests", body),
  validate: (body: { version: string; instruments: string[]; from: string; to: string; equity: string }) => request<Record<string, unknown>>("POST", "/validations", body),
  evidence: (version?: string) => request<Array<Record<string, unknown>>>("GET", version ? `/evidence?version=${encodeURIComponent(version)}` : "/evidence"),
  aiRun: () => request<Record<string, unknown>>("POST", "/ai/run"),
  aiAdvice: (decision?: string) => request<Array<Record<string, unknown>>>("GET", decision ? `/ai/advice?decision=${encodeURIComponent(decision)}` : "/ai/advice"),
  aiScorecard: () => request<Array<Record<string, unknown>>>("GET", "/ai/scorecard"),
  review: () => request<Record<string, unknown>>("GET", "/review"),
  recordReview: (version: string) => request<Record<string, unknown>>("POST", `/review/${encodeURIComponent(version)}/record`),
  settings: () => request<SettingsView>("GET", "/settings"),
  updateSettings: (section: string, values: Record<string, string | boolean>) => request<SettingsSection>("PUT", `/settings/${encodeURIComponent(section)}`, { values }),
  resetSettings: (section: string) => request<SettingsSection>("POST", `/settings/${encodeURIComponent(section)}/reset`),
  secrets: () => request<{ available: boolean; secrets: SecretRow[] }>("GET", "/secrets"),
  setSecret: (name: string, value: string) => request<{ name: string; set: boolean }>("PUT", `/secrets/${encodeURIComponent(name)}`, { value }),
  clearSecret: (name: string) => request<{ name: string; set: boolean }>("DELETE", `/secrets/${encodeURIComponent(name)}`),
  paper: () => request<Record<string, unknown>>("GET", "/paper"),
  paperRun: (through: string) => request<Record<string, unknown>>("POST", "/paper/run", { through }),
  instrumentSpecs: () => request<Array<Record<string, unknown>>>("GET", "/instruments"),
  addInstrument: (toml: string) => request<Record<string, unknown>>("POST", "/instruments", { toml }),
  importBars: (id: string, csv: string, acceptJumps: boolean) => request<{ rows_written: number; issues: DataIssue[] }>("POST", `/instruments/${encodeURIComponent(id)}/bars`, { csv, accept_jumps: acceptJumps }),
  barQuality: (id: string) => request<{ symbol: string; bars: number; first: string | null; last: string | null; issues: DataIssue[] }>("GET", `/instruments/${encodeURIComponent(id)}/quality`),
  portfolio: (book: "paper" | "live") => request<Record<string, unknown>>("GET", `/portfolio?book=${book}`),
  bars: (id: string, from: string, to: string) => request<Array<{ date: string; open: string; high: string; low: string; close: string; volume: string }>>("GET", `/instruments/${encodeURIComponent(id)}/bars?from=${from}&to=${to}`),
  kite: () => request<KiteStatus>("GET", "/kite"),
  kiteLogin: () => request<{ url: string }>("POST", "/kite/login"),
  kiteSyncBars: () => request<Record<string, unknown>>("POST", "/kite/sync-bars"),
  kiteSyncFills: () => request<Record<string, unknown>>("POST", "/kite/sync-fills"),
  live: () => request<Record<string, unknown>>("GET", "/live"),
  liveRun: (through: string) => request<Record<string, unknown>>("POST", "/live/run", { through }),
  testNotification: () => request<{ sent: boolean }>("POST", "/notifications/test"),
  setLiveArmed: (armed: boolean) => request<{ armed: boolean }>("POST", "/account/live-armed", { armed }),
};
