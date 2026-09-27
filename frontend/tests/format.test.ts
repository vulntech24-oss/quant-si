import { describe, expect, it } from "vitest";
import { formatNumber, formatPercent, groupIndian, roundDecimal } from "../src/format";

describe("decimal formatting (no floats)", () => {
  it("rounds half away from zero", () => {
    expect(roundDecimal("1.005", 2)).toBe("1.01");
    expect(roundDecimal("-1.005", 2)).toBe("-1.01");
    expect(roundDecimal("2.4999", 2)).toBe("2.50");
    expect(roundDecimal("7", 2)).toBe("7.00");
    expect(roundDecimal("-0.001", 2)).toBe("0.00");
    expect(roundDecimal("abc", 2)).toBeNull();
  });

  it("keeps precision a float would lose", () => {
    expect(roundDecimal("0.1000000000000000055511151231257827", 20)).toBe("0.10000000000000000555");
    expect(formatNumber("12345678901234567890.125", 2)).toBe("1,23,45,67,89,01,23,45,67,890.13");
  });

  it("groups the Indian way", () => {
    expect(groupIndian("1248200")).toBe("12,48,200");
    expect(groupIndian("999")).toBe("999");
    expect(formatNumber("1248200.456")).toBe("12,48,200.46");
    expect(formatNumber(null)).toBe("—");
  });

  it("formats fractions as percentages", () => {
    expect(formatPercent("0.0213")).toBe("2.13%");
    expect(formatPercent("0.005")).toBe("0.50%");
    expect(formatPercent("1")).toBe("100.00%");
    expect(formatPercent("-0.025")).toBe("-2.50%");
  });
});
