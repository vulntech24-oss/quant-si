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
