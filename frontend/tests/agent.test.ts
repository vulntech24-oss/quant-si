import { describe, expect, it } from "vitest";

import { agentSetupMissing, outcomeText, traceText } from "../src/views";

describe("AI agent page", () => {
  it("lists what the owner must set up first", () => {
    expect(agentSetupMissing({ enabled: true, key_set: true })).toEqual([]);
    const missing = agentSetupMissing({ enabled: false, key_set: false, provider: "gemini" });
    expect(missing).toHaveLength(2);
    expect(missing[1]).toContain("gemini");
  });

  it("describes prediction outcomes without parsing decimals into numbers", () => {
    expect(outcomeText(null)).toBe("open");
    expect(outcomeText({ correct: true, levels: "target", return_pct: "0.0523", as_of: "2026-10-09" })).toBe("right (target hit, 5.23% by 2026-10-09)");
    expect(outcomeText({ correct: false, levels: null, return_pct: "-0.01", as_of: "2026-10-09" })).toBe("wrong (-1.00% by 2026-10-09)");
  });

  it("renders trace values as text", () => {
    expect(traceText({ a: "<b>" })).toContain("\"<b>\"");
    expect(traceText(undefined)).toBe("");
  });
});
