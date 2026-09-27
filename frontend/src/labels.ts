import { formatPercent } from "./format";

// The exact labels of spec §6.3 and human text for NO TRADE reasons. No bare
// BUY or SELL anywhere (INV-12).

export const ACTION_LABELS: Record<string, string> = {
  open_long: "BUY (open long)",
  close_long: "SELL (close long)",
  open_short: "SELL SHORT (open short)",
  close_short: "BUY TO COVER (close short)",
};

export function actionLabel(action: string): string {
  return ACTION_LABELS[action] ?? "UNKNOWN ACTION";
}

type Detail = Record<string, unknown> | null | undefined;

function field(detail: Detail, key: string): string {
  const value = detail?.[key];
  return typeof value === "string" || typeof value === "number" ? String(value) : "?";
}

function pct(detail: Detail, key: string): string {
  const value = detail?.[key];
  return typeof value === "string" ? formatPercent(value, 2) : "?";
}

const BREACHES: Record<string, (d: Detail) => string> = {
  total_open_risk: (d) => `Total open risk would reach ${pct(d, "would_be")} (limit ${pct(d, "limit")}).`,
  correlated_bucket: (d) => `Open risk in bucket ${field(d, "bucket")} would reach ${pct(d, "would_be")} (limit ${pct(d, "limit")}).`,
  strategy_version: (d) => `Open risk for this strategy version would reach ${pct(d, "would_be")} (limit ${pct(d, "limit")}).`,
  daily_loss: (d) => `Daily loss ${pct(d, "current")} reached the ${pct(d, "limit")} limit.`,
  weekly_loss: (d) => `Weekly loss ${pct(d, "current")} reached the ${pct(d, "limit")} limit.`,
  drawdown: (d) => `Drawdown ${pct(d, "current")} reached the ${pct(d, "limit")} hard-halt limit.`,
  consecutive_losses: (d) => `Cool-off after ${field(d, "losses")} losing trades in a row.`,
  stage_not_eligible: () => "The strategy version is not at a stage that may trade on this account.",
};

/** One sentence explaining a NO TRADE reason. */
export function reasonText(code: string, detail: Detail): string {
  switch (code) {
    case "strategy_inactive_in_regime":
      return `The strategy does not trade in the ${field(detail, "regime")} regime.`;
    case "invalid_trade_plan":
      return "The trade plan is invalid.";
    case "insufficient_evidence":
      return `Only ${field(detail, "n")} comparable out-of-sample setups; ${field(detail, "min")} required.`;
    case "insufficient_edge":
      return `Expected value ${field(detail, "ev_r")}R is below the ${field(detail, "min")}R minimum.`;
    case "risk_reward_below_floor":
      return `Net reward-to-risk ${field(detail, "rr")} is below the floor of ${field(detail, "floor")}.`;
    case "conflicting_signals":
      return "Signals disagree.";
    case "superseded":
      return "A newer proposal replaced this one.";
    case "already_in_position":
      return "A position in this instrument is already open.";
    case "short_not_permitted":
      return "This short is not permitted.";
    case "stale_data":
      return `Input data is stale (${field(detail, "input")}).`;
    case "missing_or_inconsistent_data":
      return `Input data is missing or inconsistent (${field(detail, "input")}).`;
    case "market_closed_or_halted":
      return "The market is closed or the instrument is halted.";
    case "price_moved_past_entry":
      return "The price already moved past the planned entry; the setup is on watch.";
    case "risk_limit": {
      const breach = typeof detail?.["breach"] === "string" ? (detail["breach"] as string) : "";
      return BREACHES[breach]?.(detail) ?? "A risk limit blocked the entry.";
    }
    case "kill_switch":
      return detail?.["block"] === "state_unknown"
        ? "The kill-switch state is unknown, so entries are halted."
        : "A halt (kill switch) blocks new entries.";
    case "position_too_small":
      return "The sized position is below the minimum tradable quantity.";
    case "uneconomic_after_costs":
      return "Costs at the final size make the trade uneconomic.";
    case "event_risk":
      return "A scheduled event makes the trade too risky.";
    case "abnormal_market":
      return "Market conditions are abnormal.";
    case "ai_veto":
      return "An AI review vetoed the trade under the strategy's AI policy.";
    default:
      return "No trade.";
  }
}
