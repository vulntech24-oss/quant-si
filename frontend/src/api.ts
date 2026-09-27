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
  backtest: (body: { instrument: string; from: string; to: string; equity: string }) => request<Record<string, unknown>>("POST", "/backtests", body),
  validate: (body: { version: string; instruments: string[]; from: string; to: string; equity: string }) => request<Record<string, unknown>>("POST", "/validations", body),
  evidence: (version?: string) => request<Array<Record<string, unknown>>>("GET", version ? `/evidence?version=${encodeURIComponent(version)}` : "/evidence"),
  review: () => request<Record<string, unknown>>("GET", "/review"),
  recordReview: (version: string) => request<Record<string, unknown>>("POST", `/review/${encodeURIComponent(version)}/record`),
  paper: () => request<Record<string, unknown>>("GET", "/paper"),
  paperRun: (through: string) => request<Record<string, unknown>>("POST", "/paper/run", { through }),
  setLiveArmed: (armed: boolean) => request<{ armed: boolean }>("POST", "/account/live-armed", { armed }),
};
