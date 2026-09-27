import { describe, expect, it } from "vitest";

import { chartGeometry } from "../src/views";

describe("decision chart geometry", () => {
  const bars = [
    { date: "2026-09-01", open: "100", high: "102", low: "99", close: "101" },
    { date: "2026-09-02", open: "101", high: "104", low: "100", close: "103.5" },
  ];
  const levels = [
    { label: "Stop", kind: "stop" as const, price: "98.05" },
    { label: "Entry", kind: "entry" as const, price: "104.05" },
    { label: "Target", kind: "target" as const, price: "118.95" },
  ];

  it("puts higher prices higher on the chart, and keeps level labels exact", () => {
    const g = chartGeometry(bars, levels, 720, 280);
    const [stop, entry, target] = g.levels;
    expect(target!.y).toBeLessThan(entry!.y);
    expect(entry!.y).toBeLessThan(stop!.y);
    expect(target!.price).toBe("118.95");
    for (const l of g.levels) {
      expect(l.y).toBeGreaterThanOrEqual(0);
      expect(l.y).toBeLessThanOrEqual(280);
    }
    expect(g.candles[1]!.up).toBe(true);
    expect(g.candles[0]!.high).toBeLessThan(g.candles[0]!.low);
  });

  it("marks the decision date at its bar", () => {
    const g = chartGeometry(bars, levels, 720, 280);
    expect(g.xOf("2026-09-02")).toBe(g.candles[1]!.x);
    expect(g.xOf("2027-01-01")).toBe(720);
  });
});
