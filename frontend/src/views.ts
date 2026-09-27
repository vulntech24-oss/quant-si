// Screens. Decision screens follow the spec §2 order: decision first, then
// why, trade details, risk, deep analysis and raw data. Headlines are always
// the post-risk verdict (INV-17).

import { ApiError, api, type DecisionSummary, type HaltView, type Me, type Status, type StrategyRow } from "./api";
import { clear, h } from "./dom";
import { formatIst, formatNumber, formatPercent } from "./format";
import { reasonText } from "./labels";

export interface Ctx {
  me: Me;
  status: Status | null;
  refreshStatus: () => Promise<void>;
}

type Json = Record<string, unknown>;

function get(obj: unknown, path: string): unknown {
  return path.split(".").reduce<unknown>((v, k) => (v && typeof v === "object" ? (v as Json)[k] : undefined), obj);
}

function str(v: unknown): string | null {
  return typeof v === "string" ? v : typeof v === "number" ? String(v) : null;
}

function errorText(e: unknown): string {
  return e instanceof ApiError ? `${e.message}` : e instanceof Error ? e.message : String(e);
}

function isOwner(ctx: Ctx): boolean {
  return ctx.me.role === "owner";
}

/** Runs an action; if the server asks for a step-up, asks for the password and retries once. */
export async function withStepUp<T>(action: () => Promise<T>): Promise<T> {
  try {
    return await action();
  } catch (e) {
    if (!(e instanceof ApiError && e.code === "step_up_required")) throw e;
    const password = await askPassword("This action needs your password again.");
    if (password === null) throw e;
    await api.stepUp(password);
    return action();
  }
}

function askPassword(message: string): Promise<string | null> {
  return new Promise((resolve) => {
    const input = h("input", { type: "password", autocomplete: "current-password", "aria-label": "Password" });
    const dialog = h(
      "dialog",
      { class: "modal" },
      h("form", { method: "dialog" }, h("p", {}, message), input, h("div", { class: "row" }, h("button", { value: "cancel", class: "ghost" }, "Cancel"), h("button", { value: "ok", class: "primary" }, "Confirm"))),
    );
    dialog.addEventListener("close", () => {
      resolve(dialog.returnValue === "ok" ? input.value : null);
      dialog.remove();
    });
    document.body.append(dialog);
    dialog.showModal();
  });
}

export function headlineClass(headline: string): string {
  if (headline === "NO TRADE") return "chip neutral";
  if (headline === "HOLD") return "chip neutral";
  return headline.startsWith("BUY") ? "chip long" : "chip short";
}

// ---------- decisions ----------

function decisionCard(d: DecisionSummary): HTMLElement {
  const reason = d.reason ? reasonText(d.reason.code, d.reason.detail) : null;
  return h(
    "a",
    { class: "card decision", href: `#/decisions/${d.id}` },
    h("div", { class: "row between" }, h("div", {}, h("div", { class: "title" }, d.symbol ?? d.instrument_id.slice(0, 8)), h("div", { class: "muted" }, `${d.strategy} v${d.strategy_version} · ${d.regime.replace(/_/g, " ")} · ${d.as_of_date}`)), h("span", { class: headlineClass(d.headline) }, d.headline)),
    reason ? h("p", { class: "reason" }, reason) : null,
    d.entry
      ? h("div", { class: "metrics" }, metric("Entry", formatNumber(d.entry)), metric("Stop", formatNumber(d.stop)), metric("Target", formatNumber(d.target)), metric("Net RR", formatNumber(d.rr_net)), metric("EV", d.ev_r ? `${formatNumber(d.ev_r)}R` : "—"), metric("Quantity", formatNumber(d.quantity, 0)))
      : null,
    h("div", { class: "muted small" }, formatIst(d.at)),
  );
}

function metric(label: string, value: string): HTMLElement {
  return h("div", { class: "metric" }, h("div", { class: "label" }, label), h("div", { class: "mono" }, value));
}

export async function decisionsView(): Promise<HTMLElement> {
  const list = await api.decisions(50);
  return h(
    "section",
    {},
    h("h1", {}, "Decisions"),
    h("p", { class: "muted" }, "Every decision after risk checks, NO TRADE included. Newest first."),
    list.length === 0 ? h("p", { class: "empty" }, "No decisions yet.") : h("div", { class: "stack" }, ...list.map(decisionCard)),
  );
}

export async function decisionView(id: string): Promise<HTMLElement> {
  const { summary: d, record } = await api.decision(id);
  const p = get(record, "proposal");
  const a = get(record, "approval");
  const reasons = (get(p, "explanation.reasons") as Array<{ factor: string; value: unknown; direction: string }> | undefined) ?? [];
  const costs = (get(a, "costs.lines") as Array<{ name: string; amount: string }> | undefined) ?? [];
  const section = (title: string, ...children: Array<Node | null>) => h("section", { class: "card" }, h("h2", {}, title), ...children);
  return h(
    "div",
    { class: "stack" },
    h("a", { href: "#/decisions", class: "ghost" }, "← All decisions"),
    section("Decision", h("div", { class: "row between" }, h("div", { class: "title" }, d.symbol ?? d.instrument_id), h("span", { class: headlineClass(d.headline) }, d.headline)), d.reason ? h("p", { class: "reason" }, reasonText(d.reason.code, d.reason.detail)) : null, h("p", { class: "muted" }, `${d.strategy} v${d.strategy_version} · decided ${formatIst(d.at)} on bars to ${d.as_of_date}`)),
    p
      ? section("Why", h("ul", { class: "reasons" }, ...reasons.map((r) => h("li", { class: r.direction === "supports" ? "supports" : "opposes" }, `${r.factor.replace(/_/g, " ")}: ${str(r.value) ?? JSON.stringify(r.value)}`))), h("p", {}, h("strong", {}, "Strongest argument against: "), str(get(p, "explanation.strongest_argument_against")) ?? "—"))
      : null,
    p
      ? section("Trade plan", h("div", { class: "metrics" }, metric("Entry", `${formatNumber(str(get(p, "plan.entry.price")))} (${str(get(p, "plan.entry.order_type"))?.replace(/_/g, " ") ?? ""})`), metric("Stop", formatNumber(str(get(p, "plan.stop")))), metric("Target", formatNumber(str(get(p, "plan.target")))), metric("Time exit", `${str(get(p, "plan.max_holding_days")) ?? "—"} trading days`)))
      : null,
    a
      ? section("Risk", h("div", { class: "metrics" }, metric("Quantity", formatNumber(str(get(a, "quantity")), 0)), metric("Planned risk", formatNumber(str(get(a, "planned_risk.amount")))), metric("Risk budget", formatNumber(str(get(a, "risk_budget.amount")))), metric("Stage multiplier", formatNumber(str(get(a, "stage_multiplier")))), metric("Costs", formatNumber(str(get(a, "costs.total")))), metric("Costs verified", get(a, "costs_verified") ? "yes" : "NO (unverified rates)")), h("table", { class: "table" }, h("tbody", {}, ...costs.map((c) => h("tr", {}, h("td", {}, c.name.replace(/_/g, " ")), h("td", { class: "mono right" }, formatNumber(c.amount)))))))
      : null,
    p
      ? section("Deep analysis", h("div", { class: "metrics" }, metric("P(target first)", formatPercent(str(get(p, "probabilities.p_target")))), metric("P(stop first)", formatPercent(str(get(p, "probabilities.p_stop")))), metric("P(time exit)", formatPercent(str(get(p, "probabilities.p_time")))), metric("Evidence", `${str(get(p, "probabilities.evidence_count")) ?? "?"} setups (${str(get(p, "probabilities.source")) ?? "?"})`), metric("Risk net / unit", formatNumber(str(get(p, "economics.risk_net")), 4)), metric("Reward net / unit", formatNumber(str(get(p, "economics.reward_net")), 4)), metric("Net RR", formatNumber(str(get(p, "economics.rr_net")))), metric("EV", `${formatNumber(str(get(p, "expected_value.in_r")))}R`)))
      : null,
    h("details", { class: "card" }, h("summary", {}, "Raw record"), h("pre", {}, JSON.stringify(record, null, 2))),
  );
}

// ---------- halts ----------

export async function haltsView(ctx: Ctx, rerender: () => void): Promise<HTMLElement> {
  const halts = await api.halts();
  const message = h("p", { class: "error", role: "alert" });
  const reason = h("input", { placeholder: "Reason", "aria-label": "Halt reason", maxlength: "200" });
  const create = isOwner(ctx)
    ? h("form", { class: "card row", onsubmit: async (e: Event) => { e.preventDefault(); try { await api.createHalt(reason.value); await ctx.refreshStatus(); rerender(); } catch (err) { message.textContent = errorText(err); } } }, reason, h("button", { class: "danger" }, "Halt new entries"))
    : null;
  const row = (halt: HaltView) =>
    h("tr", {}, h("td", {}, halt.kind.replace(/_/g, " ")), h("td", {}, halt.scope.scope), h("td", {}, halt.reason), h("td", {}, formatIst(halt.started_at)), h("td", {}, halt.active ? h("span", { class: "chip short" }, "ACTIVE") : h("span", { class: "chip neutral" }, "cleared")), h("td", {}, halt.active && isOwner(ctx) && halt.kind !== "startup" ? h("button", { class: "ghost", onclick: async () => { try { await withStepUp(() => api.rearm(halt.id)); await ctx.refreshStatus(); rerender(); } catch (err) { message.textContent = errorText(err); } } }, "Re-arm") : null));
  return h("section", {}, h("h1", {}, "Kill switch"), h("p", { class: "muted" }, "Halts block new entries only. Exits and protective orders always go through. Re-arming needs your password."), create, message, h("table", { class: "table" }, h("thead", {}, h("tr", {}, ...["Kind", "Scope", "Reason", "Started", "State", ""].map((t) => h("th", {}, t)))), h("tbody", {}, ...halts.map(row))));
}

// ---------- strategies ----------

const NEXT_EVENTS: Record<string, Array<{ label: string; event: Record<string, unknown>; needsEvidence?: boolean }>> = {
  draft: [{ label: "Start research", event: { event: "start_research" } }],
  research: [{ label: "Pass research", event: { event: "pass_research" }, needsEvidence: true }, { label: "Reject", event: { event: "reject_research", reason: "rejected by owner" } }],
  research_passed: [{ label: "Promote to Paper", event: { event: "promote", to: "paper" }, needsEvidence: true }],
  paper: [{ label: "Promote to Small capital", event: { event: "promote", to: "small_capital" }, needsEvidence: true }, { label: "Suspend", event: { event: "suspend", reason: "suspended by owner" } }],
  small_capital: [{ label: "Promote to Full", event: { event: "promote", to: "full" }, needsEvidence: true }, { label: "Suspend", event: { event: "suspend", reason: "suspended by owner" } }],
  full: [{ label: "Suspend", event: { event: "suspend", reason: "suspended by owner" } }],
  suspended: [{ label: "Resume", event: { event: "resume" } }],
};

export async function strategiesView(ctx: Ctx, rerender: () => void): Promise<HTMLElement> {
  const rows = await api.strategies();
  const message = h("p", { class: "error", role: "alert" });
  const actions = (row: StrategyRow) => {
    if (!isOwner(ctx)) return null;
    const options = NEXT_EVENTS[row.stage.stage] ?? [];
    return h("div", { class: "row" }, ...options.map((o) => h("button", { class: "ghost", onclick: async () => {
      let event = o.event;
      if (o.needsEvidence) {
        const evidence = window.prompt("Evidence id (UUID of the recorded validation evidence):");
        if (!evidence) return;
        event = { ...event, evidence };
      }
      try { await withStepUp(() => api.strategyEvent(row.version.reference.version_id, event)); rerender(); } catch (err) { message.textContent = errorText(err); }
    } }, o.label)), row.stage.stage !== "retired" ? h("button", { class: "ghost danger-text", onclick: async () => { try { await api.strategyEvent(row.version.reference.version_id, { event: "retire", reason: "retired by owner" }); rerender(); } catch (err) { message.textContent = errorText(err); } } }, "Retire") : null);
  };
  return h("section", {}, h("h1", {}, "Strategy registry"), h("p", { class: "muted" }, "Versions are immutable. Promotions need recorded evidence and your password; demotion on a breach is automatic."), message, rows.length === 0 ? h("p", { class: "empty" }, "No strategy versions registered. Use `qd strategy register-trend-pullback`.") : h("table", { class: "table" }, h("thead", {}, h("tr", {}, ...["Strategy", "Version", "Logic", "RR floor", "Stage", ""].map((t) => h("th", {}, t)))), h("tbody", {}, ...rows.map((r) => h("tr", {}, h("td", {}, r.version.reference.name), h("td", { class: "mono" }, String(r.version.reference.version_number)), h("td", { class: "mono" }, r.version.reference.logic_version), h("td", { class: "mono" }, formatNumber(r.version.rr_floor)), h("td", {}, h("span", { class: "chip neutral" }, r.stage.stage.replace(/_/g, " "))), h("td", {}, actions(r)))))));
}

// ---------- backtests ----------

export async function backtestView(ctx: Ctx): Promise<HTMLElement> {
  const instruments = await api.instruments();
  const result = h("div", { class: "stack" });
  const select = h("select", { "aria-label": "Instrument" }, ...instruments.map((i) => h("option", { value: i.id }, `${i.symbol} (v${i.version})`)));
  const from = h("input", { type: "date", "aria-label": "From" });
  const to = h("input", { type: "date", "aria-label": "To" });
  const equity = h("input", { value: "1000000", inputmode: "decimal", "aria-label": "Starting equity" });
  const run = async (e: Event) => {
    e.preventDefault();
    clear(result);
    result.append(h("p", { class: "muted" }, "Running…"));
    try {
      const report = await api.backtest({ instrument: select.value, from: from.value, to: to.value, equity: equity.value });
      clear(result);
      result.append(backtestReport(report));
    } catch (err) {
      clear(result);
      result.append(h("p", { class: "error" }, errorText(err)));
    }
  };
  return h("section", {}, h("h1", {}, "Research backtest"), h("p", { class: "muted" }, "Trend pullback, simulated at the Paper stage with a neutral evidence prior. Research only: results are not evidence until validated out of sample."), isOwner(ctx) ? h("form", { class: "card row wrap", onsubmit: run }, select, from, to, equity, h("button", { class: "primary" }, "Run")) : h("p", { class: "muted" }, "Only the owner can run backtests."), result);
}

function backtestReport(report: Json): HTMLElement {
  const m = (get(report, "metrics") ?? {}) as Json;
  const trades = (get(report, "trades") as Json[] | undefined) ?? [];
  const curve = (get(report, "equity_curve") as Array<{ equity: string }> | undefined) ?? [];
  return h("div", { class: "stack" }, h("div", { class: "card metrics" }, metric("Trades", str(m["trades"]) ?? "0"), metric("Win rate", formatPercent(str(m["win_rate"]))), metric("Expectancy", `${formatNumber(str(m["expectancy_r"]))}R`), metric("Net P&L", formatNumber(str(m["net_pnl"]))), metric("Total return", formatPercent(str(m["total_return"]))), metric("Max drawdown", formatPercent(str(m["max_drawdown"])))), curve.length > 1 ? h("div", { class: "card" }, sparkline(curve.map((p) => p.equity))) : null, h("table", { class: "table" }, h("thead", {}, h("tr", {}, ...["Opened", "Closed", "Qty", "Entry", "Exit", "Net", "R", "Exit reason"].map((t) => h("th", {}, t)))), h("tbody", {}, ...trades.map((t) => h("tr", {}, h("td", {}, str(t["opened_on"]) ?? ""), h("td", {}, str(t["closed_on"]) ?? ""), h("td", { class: "mono right" }, formatNumber(str(t["quantity"]), 0)), h("td", { class: "mono right" }, formatNumber(str(t["entry_price"]))), h("td", { class: "mono right" }, formatNumber(str(t["exit_price"]))), h("td", { class: "mono right" }, formatNumber(str(t["net_pnl"]))), h("td", { class: "mono right" }, formatNumber(str(t["r_multiple"]))), h("td", {}, (str(t["exit_reason"]) ?? "").replace(/_/g, " ")))))));
}

/** An equity sparkline. Coordinates only, never money: floats are fine here. */
function sparkline(values: string[]): SVGSVGElement {
  const nums = values.map(Number);
  const min = Math.min(...nums);
  const max = Math.max(...nums);
  const span = max - min || 1;
  const points = nums.map((v, i) => `${(i / (nums.length - 1)) * 600},${100 - ((v - min) / span) * 100}`).join(" ");
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 600 100");
  svg.setAttribute("class", "spark");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", "Equity curve");
  const line = document.createElementNS("http://www.w3.org/2000/svg", "polyline");
  line.setAttribute("points", points);
  svg.append(line);
  return svg;
}

// ---------- journal ----------

export async function journalView(): Promise<HTMLElement> {
  const entries = await api.journal(100);
  return h("section", {}, h("h1", {}, "Decision journal"), h("p", { class: "muted" }, "Append-only record of decisions, orders, fills, positions and halts."), h("table", { class: "table" }, h("thead", {}, h("tr", {}, ...["#", "Kind", "Recorded", ""].map((t) => h("th", {}, t)))), h("tbody", {}, ...entries.map((e) => h("tr", {}, h("td", { class: "mono" }, String(e.seq)), h("td", {}, e.kind.replace(/_/g, " ")), h("td", {}, formatIst(e.recorded_at)), h("td", {}, h("details", {}, h("summary", {}, "entry"), h("pre", {}, JSON.stringify(e.entry, null, 2)))))))));
}
