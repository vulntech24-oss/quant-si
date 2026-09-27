import { describe, expect, it } from "vitest";
import { ACTION_LABELS, actionLabel, reasonText } from "../src/labels";

describe("INV-12 labels", () => {
  it("uses the exact spec labels", () => {
    expect(ACTION_LABELS).toEqual({
      open_long: "BUY (open long)",
      close_long: "SELL (close long)",
      open_short: "SELL SHORT (open short)",
      close_short: "BUY TO COVER (close short)",
    });
    expect(actionLabel("unknown")).toBe("UNKNOWN ACTION");
  });

  it("never shows a bare BUY or SELL", () => {
    for (const label of Object.values(ACTION_LABELS)) {
      expect(label).toMatch(/\((open|close) (long|short)\)$/);
    }
  });
});

describe("INV-17 NO TRADE reasons", () => {
  it("explains risk blocks with their numbers", () => {
    expect(reasonText("risk_limit", { breach: "daily_loss", limit: "0.02", current: "0.021" })).toBe(
      "Daily loss 2.10% reached the 2.00% limit.",
    );
    expect(reasonText("insufficient_evidence", { n: 12, min: 30 })).toBe(
      "Only 12 comparable out-of-sample setups; 30 required.",
    );
    expect(reasonText("kill_switch", { block: "state_unknown" })).toContain("unknown");
    expect(reasonText("something_new", null)).toBe("No trade.");
  });
});
