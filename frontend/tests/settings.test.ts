import { describe, expect, it } from "vitest";
import { fieldText } from "../src/views";

describe("settings fields", () => {
  it("keeps decimals as the exact strings the server sent", () => {
    expect(fieldText("0.0050000000000000001")).toBe("0.0050000000000000001");
    expect(fieldText("1000000")).toBe("1000000");
  });
  it("shows integers and empty values", () => {
    expect(fieldText(400)).toBe("400");
    expect(fieldText(null)).toBe("");
  });
});

import { diffSettings } from "../src/views";

describe("settings history", () => {
  it("lists only the fields that differ, keeping exact strings", () => {
    const a = { risk_per_trade: "0.005", stage_multipliers: { paper: "0", full: "1" }, enabled: true };
    const b = { risk_per_trade: "0.004", stage_multipliers: { paper: "0", full: "1" }, enabled: false };
    expect(diffSettings(a, b)).toEqual([
      { key: "enabled", from: "true", to: "false" },
      { key: "risk_per_trade", from: "0.005", to: "0.004" },
    ]);
    expect(diffSettings(a, a)).toEqual([]);
  });
});
