// Screens. Decision screens follow the spec §2 order: decision first, then
// why, trade details, risk, deep analysis and raw data. Headlines are always
// the post-risk verdict (INV-17).

import { ApiError, api, type DecisionSummary, type DataIssue, type HaltView, type KiteStatus, type Me, type SecretRow, type SettingsField, type SettingsSection, type Status, type StrategyRow } from "./api";
import { clear, h } from "./dom";
import { formatIst, formatNumber, formatPercent } from "./format";
import { actionLabel, reasonText } from "./labels";

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

const STANCE_CLASS: Record<string, string> = { agree: "chip long", caution: "chip neutral", disagree: "chip short", abstain: "chip neutral" };

function adviceCard(a: Json): HTMLElement {
  const flags = (a["flags"] as string[] | undefined) ?? [];
  return h("div", { class: "card stack" },
    h("div", { class: "row" }, h("span", { class: STANCE_CLASS[String(a["stance"])] ?? "chip neutral" }, String(a["stance"]).toUpperCase()), h("strong", {}, String(a["advisor"])), h("span", { class: "muted" }, `confidence ${formatPercent(str(a["confidence"]), 0)} · ${formatIst(String(a["at"]))}`)),
    h("p", {}, String(a["summary"])),
    flags.length ? h("ul", { class: "reasons" }, ...flags.map((f) => h("li", { class: "opposes" }, f))) : null);
}

/** Advisory AI on a decision. Missing or disabled AI shows nothing. */
async function adviceSection(decision: string): Promise<HTMLElement | null> {
  let advice: Json[];
  try { advice = await api.aiAdvice(decision); } catch { return null; }
  if (advice.length === 0) return null;
  return h("section", { class: "card" }, h("h2", {}, "AI review (advisory only)"), h("p", { class: "muted" }, "Commentary after the decision. It never changes the decision, the size, the limits or any order."), ...advice.map(adviceCard));
}

export async function decisionView(id: string): Promise<HTMLElement> {
  const [{ summary: d, record }, ai] = await Promise.all([api.decision(id), adviceSection(id)]);
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
    ai,
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

async function latestPassedEvidence(version: string, kind: string): Promise<string | undefined> {
  const records = await api.evidence(version);
  const hit = records.find((r) => r["kind"] === kind && r["passed"] === true);
  return typeof hit?.["id"] === "string" ? hit["id"] : undefined;
}

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
        const kind = o.event["to"] === "small_capital" || o.event["to"] === "full" ? "paper_review" : "validation";
        let evidence: string | undefined;
        try { evidence = await latestPassedEvidence(row.version.reference.version_id, kind); } catch (err) { message.textContent = errorText(err); return; }
        if (!evidence) { message.textContent = `No passed ${kind.replace("_", " ")} is recorded for this version. Record one first.`; return; }
        if (!window.confirm(`Cite ${kind.replace("_", " ")} ${evidence} for "${o.label}"?`)) return;
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
  const strategies = await api.strategyCatalog();
  const strategy = h("select", { "aria-label": "Strategy" }, ...strategies.map((s) => h("option", { value: s.logic_version }, `${s.name} (${s.logic_version})`)));
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
      const report = await api.backtest({ instrument: select.value, from: from.value, to: to.value, equity: equity.value, logic_version: strategy.value });
      clear(result);
      result.append(backtestReport(report));
    } catch (err) {
      clear(result);
      result.append(h("p", { class: "error" }, errorText(err)));
    }
  };
  return h("section", {}, h("h1", {}, "Research backtest"), h("p", { class: "muted" }, "Any strategy in this build, simulated at the Paper stage with a neutral evidence prior. Research only: results are not evidence until validated out of sample."), isOwner(ctx) ? h("form", { class: "card row wrap", onsubmit: run }, strategy, select, from, to, equity, h("button", { class: "primary" }, "Run")) : h("p", { class: "muted" }, "Only the owner can run backtests."), result);
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

// ---------- validation and evidence ----------

function checksTable(checks: Json[]): HTMLElement {
  return h("table", { class: "table" }, h("thead", {}, h("tr", {}, ...["Check", "Result", "Detail"].map((t) => h("th", {}, t)))), h("tbody", {}, ...checks.map((c) => h("tr", {}, h("td", {}, String(c["name"]).replace(/_/g, " ")), h("td", {}, c["passed"] === true ? h("span", { class: "chip long" }, "PASS") : h("span", { class: "chip short" }, "FAIL")), h("td", {}, str(c["detail"]) ?? "")))));
}

function evidenceTables(tables: Json[]): HTMLElement {
  if (tables.length === 0) return h("p", { class: "empty" }, "No evidence tables: no out-of-sample trades.");
  return h("table", { class: "table" }, h("thead", {}, h("tr", {}, ...["Setup", "Trades", "P(target)", "P(stop)", "P(other)", "Other-exit R"].map((t) => h("th", {}, t)))), h("tbody", {}, ...tables.map((t) => h("tr", {}, h("td", {}, str(t["setup_type"]) ?? ""), h("td", { class: "mono right" }, str(t["count"]) ?? "0"), h("td", { class: "mono right" }, formatPercent(str(t["p_target"]))), h("td", { class: "mono right" }, formatPercent(str(t["p_stop"]))), h("td", { class: "mono right" }, formatPercent(str(t["p_time"]))), h("td", { class: "mono right" }, formatNumber(str(t["time_exit_r"])))))));
}

function evidenceCard(r: Json): HTMLElement {
  const mc = get(r, "monte_carlo") as Json | null;
  return h("div", { class: "card stack" },
    h("div", { class: "row" }, h("span", { class: r["passed"] === true ? "chip long" : "chip short" }, r["passed"] === true ? "PASSED" : "FAILED"), h("strong", {}, String(r["kind"]).replace(/_/g, " ")), h("span", { class: "muted mono" }, String(r["id"])), h("span", { class: "muted" }, formatIst(String(r["created_at"])))),
    h("div", { class: "metrics" }, metric("OOS trades", str(get(r, "oos.trades")) ?? "—"), metric("OOS expectancy", `${formatNumber(str(get(r, "oos.expectancy_r")))}R`), metric("Holdout trades", str(get(r, "holdout.trades")) ?? "—"), metric("Holdout expectancy", `${formatNumber(str(get(r, "holdout.expectancy_r")))}R`), mc ? metric("MC p95 drawdown", formatPercent(str(mc["max_drawdown_p95"]))) : null),
    checksTable((get(r, "checks") as Json[] | null) ?? []),
    evidenceTables((get(r, "evidence") as Json[] | null) ?? []));
}

export async function validationView(ctx: Ctx, rerender: () => void): Promise<HTMLElement> {
  const [rows, instruments, records] = await Promise.all([api.strategies(), api.instruments(), api.evidence()]);
  const message = h("p", { class: "error", role: "alert" });
  const result = h("div", { class: "stack" });
  const version = h("select", { "aria-label": "Strategy version" }, ...rows.map((r) => h("option", { value: r.version.reference.version_id }, `${r.version.reference.name} v${r.version.reference.version_number} (${r.stage.stage.replace(/_/g, " ")})`)));
  const instrument = h("select", { "aria-label": "Instrument" }, h("option", { value: "" }, "All instruments with bars"), ...instruments.map((i) => h("option", { value: i.id }, `${i.symbol} (v${i.version})`)));
  const from = h("input", { type: "date", "aria-label": "From" });
  const to = h("input", { type: "date", "aria-label": "To" });
  const equity = h("input", { value: "1000000", inputmode: "decimal", "aria-label": "Starting equity" });
  const run = async (e: Event) => {
    e.preventDefault();
    message.textContent = "";
    clear(result);
    result.append(h("p", { class: "muted" }, "Validating… (walk-forward windows, holdout, Monte Carlo)"));
    try {
      // The new record is recorded server-side; the list below shows it first.
      await api.validate({ version: version.value, instruments: instrument.value ? [instrument.value] : [], from: from.value, to: to.value, equity: equity.value });
      rerender();
    } catch (err) {
      clear(result);
      message.textContent = errorText(err);
    }
  };
  return h("section", {}, h("h1", {}, "Validation and evidence"),
    h("p", { class: "muted" }, "Walk-forward out-of-sample windows, then a holdout run once, then Monte Carlo on the out-of-sample trades. Every result, pass or fail, is recorded and cannot be edited. Only a passed validation can move a version to Paper; only a passed paper review can move it to a live stage."),
    isOwner(ctx) ? h("form", { class: "card row wrap", onsubmit: run }, version, instrument, from, to, equity, h("button", { class: "primary" }, "Validate")) : null,
    message, result,
    records.length === 0 ? h("p", { class: "empty" }, "No evidence recorded yet.") : h("div", { class: "stack" }, ...records.map(evidenceCard)));
}

// ---------- advisory AI ----------

export async function aiView(ctx: Ctx, rerender: () => void): Promise<HTMLElement> {
  const title = h("h1", {}, "AI review");
  const intro = h("p", { class: "muted" }, "Advisory only, in shadow mode: advisors comment on entry decisions after the Risk Gate decided. Their advice never changes orders, sizes, limits, parameters or halts, and is scored against outcomes here.");
  let scores: Json[];
  let advice: Json[];
  try { [scores, advice] = await Promise.all([api.aiScorecard(), api.aiAdvice()]); } catch (err) { return h("section", {}, title, intro, h("p", { class: "empty" }, errorText(err))); }
  const message = h("p", { class: "error", role: "alert" });
  const run = isOwner(ctx) ? h("button", { class: "primary", onclick: async () => { try { const r = await api.aiRun(); window.alert(`Advice written: ${str(r["advice_written"]) ?? "0"}; over budget: ${str(r["over_budget"]) ?? "0"}; failures: ${((r["failures"] as unknown[]) ?? []).length}`); rerender(); } catch (err) { message.textContent = errorText(err); } } }, "Advise on new entries") : null;
  const table = scores.length === 0 ? h("p", { class: "empty" }, "No advice scored yet.") : h("table", { class: "table" }, h("thead", {}, h("tr", {}, ...["Advisor", "Advised", "Scored", "Right", "Agree trades", "Mean R (agree)", "Doubt trades", "Mean R (doubt)"].map((t) => h("th", {}, t)))), h("tbody", {}, ...scores.map((s) => h("tr", {}, h("td", {}, String(s["advisor"])), h("td", { class: "mono right" }, str(s["advised"]) ?? "0"), h("td", { class: "mono right" }, str(s["scored"]) ?? "0"), h("td", { class: "mono right" }, str(s["right"]) ?? "0"), h("td", { class: "mono right" }, str(s["agree_trades"]) ?? "0"), h("td", { class: "mono right" }, `${formatNumber(str(s["agree_mean_r"]))}R`), h("td", { class: "mono right" }, str(s["doubt_trades"]) ?? "0"), h("td", { class: "mono right" }, `${formatNumber(str(s["doubt_mean_r"]))}R`)))));
  return h("section", {}, title, intro, run, message, h("h2", {}, "Scorecard"), table, h("h2", {}, "Recent advice"), advice.length === 0 ? h("p", { class: "empty" }, "No advice yet.") : h("div", { class: "stack" }, ...advice.slice(0, 30).map((a) => h("div", { class: "stack" }, h("a", { href: `#/decisions/${encodeURIComponent(String(a["decision"]))}` }, `Decision ${String(a["decision"])}`), adviceCard(a)))));
}

// ---------- review and calibration ----------

export async function reviewView(ctx: Ctx, rerender: () => void): Promise<HTMLElement> {
  const data = await api.review();
  const rows = await api.strategies();
  const name = (id: unknown) => { const r = rows.find((x) => x.version.reference.version_id === id); return r ? `${r.version.reference.name} v${r.version.reference.version_number}` : String(id); };
  const report = (get(data, "report") ?? {}) as Json;
  const versions = (report["versions"] as Json[] | undefined) ?? [];
  const checks = (get(data, "checks") as Json[] | undefined) ?? [];
  const message = h("p", { class: "error", role: "alert" });
  const shares = (v: unknown) => (Array.isArray(v) ? v.map((x) => formatPercent(str(x))).join(" / ") : "—");
  const card = (v: Json) => {
    const vc = (checks.find((c) => c["version"] === v["version"])?.["checks"] as Json[] | undefined) ?? [];
    const bins = ((v["bins"] as Json[] | undefined) ?? []).filter((b) => Number(b["count"]) > 0);
    return h("div", { class: "card stack" },
      h("h2", {}, name(v["version"])),
      h("div", { class: "metrics" }, metric("Entries", str(v["entries"]) ?? "0"), metric("Closed trades", str(v["trades"]) ?? "0"), metric("Predicted EV", `${formatNumber(str(v["predicted_ev_r"]))}R`), metric("Realized", `${formatNumber(str(v["realized_r"]))}R`), metric("Brier (target)", formatNumber(str(v["brier"]), 4))),
      h("p", { class: "muted" }, `Target / stop / other — predicted ${shares(v["predicted"])}, realized ${shares(v["realized"])}`),
      bins.length ? h("table", { class: "table" }, h("thead", {}, h("tr", {}, ...["P(target) bin", "Trades", "Predicted", "Realized"].map((t) => h("th", {}, t)))), h("tbody", {}, ...bins.map((b) => h("tr", {}, h("td", {}, `${formatPercent(str(b["from"]), 0)}–${formatPercent(str(b["to"]), 0)}`), h("td", { class: "mono right" }, str(b["count"]) ?? ""), h("td", { class: "mono right" }, formatPercent(str(b["predicted"]))), h("td", { class: "mono right" }, formatPercent(str(b["realized"]))))))) : null,
      checksTable(vc),
      isOwner(ctx) ? h("button", { class: "ghost", onclick: async () => { try { const r = await api.recordReview(String(v["version"])); window.alert(r["passed"] === true ? "Paper review recorded: PASSED." : "Paper review recorded: FAILED. It cannot back a promotion."); rerender(); } catch (err) { message.textContent = errorText(err); } } }, "Record paper review") : null);
  };
  return h("section", {}, h("h1", {}, "Review and calibration"),
    h("p", { class: "muted" }, "Paper decisions against their outcomes. A recorded paper review that passes is the only evidence that can back a promotion to a live stage; live trading also needs every live-trading condition."),
    h("div", { class: "card metrics" }, metric("Paper days", str(report["days"]) ?? "0"), metric("Max drawdown", formatPercent(str(report["max_drawdown"]))), metric("Operational incidents", str(report["operational_incidents"]) ?? "0")),
    message,
    versions.length === 0 ? h("p", { class: "empty" }, "No paper entry decisions yet.") : h("div", { class: "stack" }, ...versions.map(card)));
}

// ---------- settings ----------

/** The text a field shows; decimals stay strings (never JS numbers). */
export function fieldText(value: SettingsField["value"]): string {
  if (value === null || value === undefined) return "";
  return typeof value === "string" ? value : String(value);
}

function fieldInput(f: SettingsField, disabled: boolean): HTMLInputElement {
  const common = { "aria-label": f.label, name: f.key, ...(disabled ? { disabled: "" } : {}) };
  switch (f.kind) {
    case "bool": {
      const box = h("input", { type: "checkbox", ...common });
      box.checked = f.value === true;
      return box;
    }
    case "integer":
      return h("input", { type: "text", inputmode: "numeric", value: fieldText(f.value), ...common });
    case "decimal":
      return h("input", { type: "text", inputmode: "decimal", value: fieldText(f.value), ...common });
    case "date":
      return h("input", { type: "date", value: fieldText(f.value), ...common });
    case "time":
      return h("input", { type: "time", step: "1", value: fieldText(f.value), ...common });
    default:
      return h("input", { type: "text", value: fieldText(f.value), ...common });
  }
}

function sectionCard(ctx: Ctx, sec: SettingsSection, rerender: () => void): HTMLElement {
  const owner = isOwner(ctx);
  const message = h("p", { class: "error", role: "alert" });
  const inputs = sec.fields.map((f) => ({ f, input: fieldInput(f, !owner) }));
  const save = async (e: Event) => {
    e.preventDefault();
    message.textContent = "";
    const values: Record<string, string | boolean> = {};
    for (const { f, input } of inputs) {
      const now = f.kind === "bool" ? input.checked : input.value.trim();
      const before = f.kind === "bool" ? f.value === true : fieldText(f.value);
      if (now !== before) values[f.key] = now;
    }
    if (Object.keys(values).length === 0) { message.textContent = "Nothing changed."; return; }
    try { await withStepUp(() => api.updateSettings(sec.name, values)); await ctx.refreshStatus(); rerender(); } catch (err) { message.textContent = errorText(err); }
  };
  const reset = async () => {
    if (!window.confirm(`Return "${sec.title}" to the configuration-file defaults?`)) return;
    try { await withStepUp(() => api.resetSettings(sec.name)); rerender(); } catch (err) { message.textContent = errorText(err); }
  };
  const source = sec.source.kind === "saved"
    ? h("span", { class: "chip long" }, `Saved in UI · v${sec.source.version ?? "?"} · ${sec.source.updated_by ?? ""} · ${sec.source.updated_at ? formatIst(sec.source.updated_at) : ""}`)
    : h("span", { class: "chip neutral" }, "File default");
  return h("form", { class: "card stack", onsubmit: save },
    h("div", { class: "row between" }, h("h2", {}, sec.title), source),
    h("p", { class: "muted" }, sec.help),
    sec.problem ? h("p", { class: "error" }, `Saved value no longer valid: ${sec.problem}`) : null,
    h("div", { class: "settings-grid" }, ...inputs.map(({ f, input }) => h("label", { class: "setting" }, h("span", { class: "setting-label" }, f.label), input, f.help ? h("span", { class: "muted small" }, f.help) : null))),
    message,
    owner ? h("div", { class: "row" }, h("button", { class: "primary" }, "Save (asks for your password)"), sec.source.kind === "saved" ? h("button", { type: "button", class: "ghost", onclick: reset }, "Reset to file default") : null) : h("p", { class: "muted" }, "Only the owner can change settings."));
}

function secretsCard(ctx: Ctx, data: { available: boolean; secrets: SecretRow[] }, rerender: () => void): HTMLElement {
  const owner = isOwner(ctx);
  const message = h("p", { class: "error", role: "alert" });
  const providers = [...new Set(data.secrets.map((s) => s.provider))];
  const row = (s: SecretRow) => {
    const input = h("input", { type: "password", autocomplete: "new-password", spellcheck: "false", placeholder: s.set ? "Enter a new value to replace" : "Paste the value", "aria-label": `${s.provider} ${s.label}`, ...(owner && data.available ? {} : { disabled: "" }) });
    const status = s.set
      ? h("span", { class: s.readable ? "chip long" : "chip short" }, s.readable ? `Set · ${s.updated_at ? formatIst(s.updated_at) : ""}` : "Set, but cannot be decrypted (master key changed?)")
      : h("span", { class: "chip neutral" }, "Not set");
    const saveIt = async (e: Event) => {
      e.preventDefault();
      message.textContent = "";
      const value = input.value;
      if (!value.trim()) { message.textContent = "Enter a value first."; return; }
      try { await withStepUp(() => api.setSecret(s.name, value)); input.value = ""; rerender(); } catch (err) { message.textContent = errorText(err); }
    };
    const clearIt = async () => {
      if (!window.confirm(`Delete the stored ${s.provider} ${s.label}?`)) return;
      try { await withStepUp(() => api.clearSecret(s.name)); rerender(); } catch (err) { message.textContent = errorText(err); }
    };
    return h("form", { class: "secret-row", onsubmit: saveIt },
      h("div", { class: "stack" }, h("strong", {}, s.label), h("span", { class: "muted small" }, s.help)),
      status,
      owner && data.available ? h("div", { class: "row" }, input, h("button", { class: "primary" }, "Save"), s.set ? h("button", { type: "button", class: "ghost danger-text", onclick: clearIt }, "Delete") : null) : null);
  };
  return h("section", { class: "card stack" },
    h("h2", {}, "API keys and credentials"),
    h("p", { class: "muted" }, "Stored encrypted on the server and never shown again, not even to you: this page only knows whether a key is set. Saving or deleting asks for your password. Keys are used by server-side adapters only; the Kite, crypto and AI provider adapters are not built yet."),
    data.available ? null : h("p", { class: "error" }, "The server has no master key, so keys cannot be stored. In Docker this is automatic; otherwise set data_dir in the server configuration or QD_MASTER_KEY in the server environment."),
    message,
    ...providers.map((p) => h("div", { class: "stack" }, h("h3", {}, p), ...data.secrets.filter((s) => s.provider === p).map(row))));
}

export async function settingsView(ctx: Ctx, rerender: () => void): Promise<HTMLElement> {
  const [view, secrets] = await Promise.all([api.settings(), api.secrets()]);
  const server = view.server;
  return h("section", {}, h("h1", {}, "Settings"),
    h("p", { class: "muted" }, "Changes apply from the next run, without a restart. Every change is validated, needs your password, and is kept in an append-only history with who changed what."),
    secretsCard(ctx, secrets, rerender),
    ...view.sections.map((sec) => sectionCard(ctx, sec, rerender)),
    h("section", { class: "card stack" }, h("h2", {}, "Server (read-only)"),
      h("div", { class: "metrics" }, metric("Environment", server.environment), metric("Live trading", server.live_trading_enabled ? "ENABLED" : "off"), metric("Live orders compiled", server.live_orders_compiled ? "yes" : "no"), metric("Account", server.account_id)),
      h("p", { class: "muted" }, server.note)));
}

// ---------- paper trading ----------

const OPEN_STATES = new Set(["opening", "open", "protected", "unprotected", "exiting"]);

export async function paperView(ctx: Ctx, rerender: () => void): Promise<HTMLElement> {
  const title = h("h1", {}, "Paper trading");
  const intro = h("p", { class: "muted" }, "Simulated fills on completed daily bars with the same rules as backtests. No order ever reaches a broker. The book is rebuilt from the journal on every run.");
  let state: Json;
  try {
    state = await api.paper();
  } catch (err) {
    return h("section", {}, title, intro, h("p", { class: "empty" }, errorText(err)));
  }
  const instruments = await api.instruments();
  const symbol = (id: unknown) => instruments.find((i) => i.id === id)?.symbol ?? String(id);
  const lastDay = get(state, "last_day") as Json | null;
  const positions = ((get(state, "positions") as Json[] | undefined) ?? []).filter((p) => OPEN_STATES.has(String(p["state"])));
  const orders = (get(state, "working_orders") as Json[] | undefined) ?? [];
  const inconsistency = str(get(state, "inconsistency"));
  const message = h("p", { class: "error", role: "alert" });
  const result = h("div", { class: "stack" });
  const through = h("input", { type: "date", "aria-label": "Process through", value: new Date().toISOString().slice(0, 10) });
  const run = async (e: Event) => {
    e.preventDefault();
    clear(result);
    result.append(h("p", { class: "muted" }, "Running…"));
    try {
      const report = await api.paperRun(through.value);
      clear(result);
      const days = (get(report, "days") as Json[] | undefined) ?? [];
      const skipped = (get(report, "skipped_versions") as string[] | undefined) ?? [];
      result.append(h("p", {}, days.length === 0 ? "Nothing new to process." : `Processed ${days.length} trading day(s).`), ...skipped.map((s) => h("p", { class: "muted" }, `Not run: ${s}`)));
      rerender();
    } catch (err) {
      clear(result);
      message.textContent = errorText(err);
    }
  };
  return h("section", {}, title, intro, inconsistency ? h("p", { class: "error", role: "alert" }, `The book cannot be trusted: ${inconsistency}. Entries are halted until you investigate.`) : null, isOwner(ctx) ? h("form", { class: "card row wrap", onsubmit: run }, h("label", {}, "Process through ", through), h("button", { class: "primary" }, "Run paper day(s)")) : null, message, result, ...bookSections(lastDay, positions, orders, symbol, "No paper day processed yet."));
}

/** Book metrics, open positions and working orders (paper and live). */
function bookSections(lastDay: Json | null, positions: Json[], orders: Json[], symbol: (id: unknown) => string, emptyText: string): HTMLElement[] {
  const book = lastDay
    ? h("div", { class: "card metrics" }, metric("Last day", str(lastDay["date"]) ?? "—"), metric("Equity", formatNumber(str(lastDay["equity"]))), metric("Realized net", formatNumber(str(get(lastDay, "book.realized_net")))), metric("Peak equity", formatNumber(str(get(lastDay, "book.high_water_mark")))), metric("Losses in a row", str(get(lastDay, "book.consecutive_losses")) ?? "0"))
    : h("p", { class: "empty" }, emptyText);
  const positionsTable = positions.length === 0
    ? h("p", { class: "empty" }, "No open positions.")
    : h("table", { class: "table" }, h("thead", {}, h("tr", {}, ...["Instrument", "Side", "State", "Qty", "Entry", "Stop", "Target", "Bars held"].map((t) => h("th", {}, t)))), h("tbody", {}, ...positions.map((p) => h("tr", {}, h("td", {}, symbol(p["instrument"])), h("td", {}, str(p["side"]) ?? ""), h("td", {}, h("span", { class: p["state"] === "unprotected" ? "chip short" : "chip neutral" }, String(p["state"]))), h("td", { class: "mono right" }, formatNumber(str(p["quantity"]), 0)), h("td", { class: "mono right" }, formatNumber(str(p["entry_price"]))), h("td", { class: "mono right" }, formatNumber(str(p["stop"]))), h("td", { class: "mono right" }, formatNumber(str(p["target"]))), h("td", { class: "mono right" }, str(p["bars_held"]) ?? "0")))));
  const ordersTable = orders.length === 0
    ? h("p", { class: "empty" }, "No working orders.")
    : h("table", { class: "table" }, h("thead", {}, h("tr", {}, ...["Instrument", "Action", "Purpose", "Type", "Qty", "Limit", "Trigger", "State"].map((t) => h("th", {}, t)))), h("tbody", {}, ...orders.map((o) => h("tr", {}, h("td", {}, symbol(get(o, "intent.instrument"))), h("td", {}, actionLabel(String(get(o, "intent.action")))), h("td", {}, String(get(o, "intent.purpose.purpose") ?? "").replace(/_/g, " ")), h("td", {}, String(get(o, "intent.terms.order_type") ?? "").replace(/_/g, " ")), h("td", { class: "mono right" }, formatNumber(str(get(o, "intent.quantity")), 0)), h("td", { class: "mono right" }, formatNumber(str(get(o, "intent.terms.limit")))), h("td", { class: "mono right" }, formatNumber(str(get(o, "intent.terms.trigger")))), h("td", {}, String(o["state"]).replace(/_/g, " "))))));
  return [book, h("h2", {}, "Open positions"), positionsTable, h("h2", {}, "Working orders"), ordersTable];
}

// ---------- Zerodha and live trading ----------

/** The outcome of a "Login with Zerodha" redirect, from the location hash. */
export function loginOutcome(hash: string): "ok" | "failed" | null {
  const query = hash.split("?")[1] ?? "";
  const value = new URLSearchParams(query).get("login");
  return value === "ok" || value === "failed" ? value : null;
}

/** What is still missing before Zerodha can be used, in order. */
export function kiteMissing(k: KiteStatus): string[] {
  const missing: string[] = [];
  if (!k.api_key_set) missing.push("Enter the Kite API key (Settings → API keys).");
  if (!k.api_secret_set) missing.push("Enter the Kite API secret (Settings → API keys).");
  if (!k.user_id) missing.push("Enter your Zerodha client id (Settings → Zerodha Kite).");
  if (!k.enabled) missing.push("Switch the connection on (Settings → Zerodha Kite).");
  if (!k.access_token_set) missing.push("Log in with Zerodha (today's session).");
  return missing;
}

/** Every INV-14 condition for real orders, and whether it holds. */
export function liveConditions(k: KiteStatus, status: Status | null): Array<[string, boolean]> {
  return [
    ["Server build includes live orders", k.live_orders_compiled],
    ["Live book configured on the server", k.live_configured],
    ["Live trading enabled on the server (production only)", k.live_trading_enabled],
    ["Live account armed with your password", status?.live_account?.live_armed === true],
    ["Logged in with Zerodha today", k.access_token_set],
  ];
}

export async function brokerView(ctx: Ctx, rerender: () => void): Promise<HTMLElement> {
  const title = h("h1", {}, "Broker · Zerodha");
  let kite: KiteStatus;
  try {
    kite = await api.kite();
  } catch (err) {
    return h("section", {}, title, h("p", { class: "empty" }, errorText(err)));
  }
  const outcome = loginOutcome(location.hash);
  const notice = outcome === "ok"
    ? h("p", { class: "chip long" }, "Logged in with Zerodha. Today's session is stored on the server.")
    : outcome === "failed"
      ? h("p", { class: "error", role: "alert" }, "The Zerodha login did not complete. Check the client id in Settings and try again; the server log has the reason.")
      : null;
  const message = h("p", { class: "error", role: "alert" });
  const result = h("div", { class: "stack" });
  const act = (label: string, action: () => Promise<unknown>, done: (r: unknown) => string, className = "ghost") =>
    h("button", { class: className, onclick: async () => {
      message.textContent = "";
      clear(result);
      result.append(h("p", { class: "muted" }, "Working…"));
      try {
        const r = await action();
        clear(result);
        result.append(h("p", {}, done(r)));
      } catch (err) {
        clear(result);
        message.textContent = errorText(err);
      }
    } }, label);
  const missing = kiteMissing(kite);
  const owner = isOwner(ctx);
  const setup = h("div", { class: "card stack" },
    h("h2", {}, "Connection"),
    h("p", { class: "muted" }, `Register the redirect URL ${location.origin}${kite.redirect_path} in the Kite developer console. Sessions expire every day at 06:00 IST.`),
    missing.length === 0 ? h("p", { class: "chip long" }, "Ready: keys set, connection on, session stored.") : h("ul", {}, ...missing.map((m) => h("li", {}, m))),
    kite.last_login ? h("p", { class: "muted" }, `Last login: ${kite.last_login.user_id} at ${formatIst(kite.last_login.at)}`) : null,
    owner ? h("div", { class: "row wrap" },
      h("button", { class: "primary", onclick: async () => {
        message.textContent = "";
        try { location.assign((await api.kiteLogin()).url); } catch (err) { message.textContent = errorText(err); }
      } }, "Login with Zerodha"),
      act("Import daily bars now", () => api.kiteSyncBars(), (r) => {
        const rows = (get(r, "instruments") as Json[] | undefined) ?? [];
        const inserted = rows.reduce((n, row) => n + Number(row["inserted"] ?? 0), 0);
        const errors = rows.filter((row) => row["error"]).length;
        return `Imported ${inserted} bar(s) for ${rows.length} instrument(s)${errors ? `; ${errors} failed (see the server log)` : ""}.`;
      }),
      act("Send a test Telegram message", () => api.testNotification(), () => "Sent. Check Telegram."),
    ) : null,
  );
  const conditions = liveConditions(kite, ctx.status);
  const live = h("div", { class: "card stack" },
    h("h2", {}, "Live trading"),
    h("p", { class: "muted" }, "Real orders go out only when every condition holds (INV-14). Exits and protective orders are never blocked by a halt. Stop and target are one GTT at Zerodha."),
    h("ul", { class: "checklist" }, ...conditions.map(([label, ok]) => h("li", {}, h("span", { class: ok ? "chip long" : "chip neutral" }, ok ? "yes" : "no"), " ", label))),
  );
  const sections: Array<HTMLElement | null> = [title, notice, setup, live, message, result];
  if (kite.live_configured) {
    const account = ctx.status?.live_account;
    if (owner && account) {
      live.append(h("div", { class: "row wrap" },
        h("button", { class: account.live_armed ? "danger" : "ghost", onclick: async () => {
          const arming = !account.live_armed;
          if (arming && !window.confirm("Arm LIVE trading with real money on the live account?")) return;
          try { await withStepUp(() => api.setLiveArmed(arming)); await ctx.refreshStatus(); rerender(); } catch (err) { message.textContent = errorText(err); }
        } }, account.live_armed ? "Live armed · disarm" : "Arm live account"),
        act("Check fills now", () => api.kiteSyncFills(), (r) => `Fills applied: ${String(get(r, "refresh.fills") ?? 0)}; unprotected positions: ${String(get(r, "unprotected") ?? 0)}.`),
        act("Run today's live cycle", () => withStepUp(() => api.liveRun(new Date().toISOString().slice(0, 10))), (r) => {
          const processed = str(get(r, "processed"));
          const demoted = (get(r, "demoted") as string[] | undefined) ?? [];
          return `${processed ? `Processed ${processed}.` : "Nothing new to process."}${demoted.length ? ` Demoted to Paper after a breach: ${demoted.join(", ")}.` : ""}`;
        }, "danger"),
      ));
    }
    try {
      const state = await api.live();
      const instruments = await api.instruments();
      const symbol = (id: unknown) => instruments.find((i) => i.id === id)?.symbol ?? String(id);
      const positions = ((get(state, "positions") as Json[] | undefined) ?? []).filter((p) => OPEN_STATES.has(String(p["state"])));
      const orders = (get(state, "working_orders") as Json[] | undefined) ?? [];
      const inconsistency = str(get(state, "inconsistency"));
      sections.push(h("h2", {}, "Live book"), inconsistency ? h("p", { class: "error", role: "alert" }, `The live book cannot be trusted: ${inconsistency}.`) : null, ...bookSections(get(state, "last_day") as Json | null, positions, orders, symbol, "No live day processed yet."));
    } catch (err) {
      sections.push(h("p", { class: "error" }, errorText(err)));
    }
  } else {
    live.append(h("p", { class: "muted" }, "No live book is configured on the server ([live] in the server file; see the operator guide)."));
  }
  return h("section", {}, ...sections);
}

// ---------- journal ----------

export async function journalView(): Promise<HTMLElement> {
  const entries = await api.journal(100);
  return h("section", {}, h("h1", {}, "Decision journal"), h("p", { class: "muted" }, "Append-only record of decisions, orders, fills, positions and halts."), h("table", { class: "table" }, h("thead", {}, h("tr", {}, ...["#", "Kind", "Recorded", ""].map((t) => h("th", {}, t)))), h("tbody", {}, ...entries.map((e) => h("tr", {}, h("td", { class: "mono" }, String(e.seq)), h("td", {}, e.kind.replace(/_/g, " ")), h("td", {}, formatIst(e.recorded_at)), h("td", {}, h("details", {}, h("summary", {}, "entry"), h("pre", {}, JSON.stringify(e.entry, null, 2)))))))));
}

// ---------- data: instruments and bars ----------

/** One line of plain text per data issue. */
export function issueText(i: DataIssue): string {
  switch (i.issue) {
    case "missing_day": return `${i.date}: no bar on a trading day`;
    case "bar_on_holiday": return `${i.date}: bar on an exchange holiday`;
    case "price_jump": return `${i.date}: close ${i.close ?? "?"} after ${i.previous ?? "?"}; check for a split, bonus or bad print`;
    case "calendar_unknown": return `${i.date}: the holiday calendar does not cover this date, so gaps were not checked`;
  }
}

const SPEC_TEMPLATE = `# A new instrument (every field is validated). Use a new id (UUID) or a
# higher version of an existing one; stored versions never change.
id = "0199a000-0000-7000-8000-000000000102"
version = 1
effective_from = "2026-01-01"
symbol = "INFY-EQ"
venue = "nse"
asset_class = "equity"
kind = "cash_equity"
currency = "INR"
tick_size = "0.05"
lot_size = "1"
multiplier = "1"
quantity_step = "1"
min_quantity = "1"
calendar_id = "nse"
correlation_bucket = "india_equity"
broker_refs = [{ broker = "kite", symbol = "INFY" }]

[capabilities]
can_short_overnight = false
supports_market_orders = true
requires_market_protection = true
protection_modes = ["broker_oco"]
products = ["delivery"]
order_types = ["limit", "stop_limit", "stop_market", "market"]
validities = ["day", "good_till_cancelled"]
`;

export async function dataView(ctx: Ctx, rerender: () => void): Promise<HTMLElement> {
  const specs = await api.instrumentSpecs();
  const owner = isOwner(ctx);
  const message = h("p", { class: "error", role: "alert" });
  const result = h("div", { class: "stack" });
  const show = (lines: string[]) => { clear(result); result.append(...lines.map((l) => h("p", {}, l))); };
  const kiteSymbol = (s: Json) => ((s["broker_refs"] as Json[] | undefined) ?? []).find((r) => r["broker"] === "kite")?.["symbol"];
  const rows = specs.map((s) => {
    const id = String(s["id"]);
    const file = h("input", { type: "file", accept: ".csv,text/csv", "aria-label": `Bars CSV for ${String(s["symbol"])}` }) as HTMLInputElement;
    const accept = h("input", { type: "checkbox", "aria-label": "Accept price jumps" }) as HTMLInputElement;
    const upload = async () => {
      message.textContent = "";
      const chosen = file.files?.[0];
      if (!chosen) { message.textContent = "Choose a CSV file first."; return; }
      try {
        const out = await api.importBars(id, await chosen.text(), accept.checked);
        show([`Imported ${out.rows_written} bar(s) for ${String(s["symbol"])}.`, ...out.issues.map(issueText)]);
      } catch (err) { message.textContent = errorText(err); }
    };
    const check = async () => {
      message.textContent = "";
      try {
        const q = await api.barQuality(id);
        show([`${q.symbol}: ${q.bars} bar(s) in the last 400 days${q.first ? `, ${q.first} to ${q.last ?? ""}` : ""}.`, ...(q.issues.length ? q.issues.map(issueText) : ["No issues found."])]);
      } catch (err) { message.textContent = errorText(err); }
    };
    return h("tr", {},
      h("td", {}, String(s["symbol"])),
      h("td", { class: "mono" }, String(s["version"])),
      h("td", {}, String(s["venue"] && typeof s["venue"] === "object" ? JSON.stringify(s["venue"]) : s["venue"] ?? "")),
      h("td", {}, String(s["calendar_id"] ?? "")),
      h("td", {}, kiteSymbol(s) ? String(kiteSymbol(s)) : h("span", { class: "muted" }, "none")),
      h("td", {}, h("button", { class: "ghost", onclick: check }, "Check data")),
      h("td", {}, owner ? h("div", { class: "row wrap" }, file, h("label", { class: "small" }, accept, " accept jumps"), h("button", { class: "ghost", onclick: upload }, "Upload bars")) : null),
    );
  });
  const table = specs.length === 0
    ? h("p", { class: "empty" }, "No instruments yet. Add one below.")
    : h("table", { class: "table" }, h("thead", {}, h("tr", {}, ...["Symbol", "Version", "Venue", "Calendar", "Kite symbol", "", "Bars CSV (date,open,high,low,close,volume)"].map((t) => h("th", {}, t)))), h("tbody", {}, ...rows));
  const spec = h("textarea", { rows: "18", class: "mono", "aria-label": "Instrument spec (TOML)" }) as HTMLTextAreaElement;
  spec.value = SPEC_TEMPLATE;
  const add = h("form", { class: "card stack", onsubmit: async (e: Event) => {
    e.preventDefault();
    message.textContent = "";
    try { const out = await api.addInstrument(spec.value); show([`Added ${String(out["symbol"])} version ${String(out["version"])}.`]); rerender(); } catch (err) { message.textContent = errorText(err); }
  } }, h("h2", {}, "Add an instrument"), h("p", { class: "muted" }, "Paste or edit the spec. A \"kite\" broker reference lets the daily import fetch its bars from Zerodha."), spec, h("button", { class: "primary" }, "Add instrument"));
  return h("section", {},
    h("h1", {}, "Data"),
    h("p", { class: "muted" }, "Instruments and their daily bars. Every import is checked against the exchange holiday calendar for gaps and for suspect price jumps; a corrected bar is stored as a new version (INV-09)."),
    message, result, table, owner ? add : null);
}
