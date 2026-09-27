import { describe, expect, it } from "vitest";

import type { KiteStatus, Status } from "../src/api";
import { kiteMissing, liveConditions, loginOutcome } from "../src/views";

const ready: KiteStatus = {
  enabled: true,
  user_id: "AB1234",
  api_key_set: true,
  api_secret_set: true,
  access_token_set: true,
  last_login: null,
  live_configured: true,
  live_trading_enabled: false,
  live_orders_compiled: false,
  redirect_path: "/api/kite/callback",
};

describe("Zerodha page", () => {
  it("reads the login outcome from the redirect", () => {
    expect(loginOutcome("#/broker?login=ok")).toBe("ok");
    expect(loginOutcome("#/broker?login=failed")).toBe("failed");
    expect(loginOutcome("#/broker")).toBeNull();
    expect(loginOutcome("#/broker?login=<script>")).toBeNull();
  });

  it("lists what is missing in the order the owner fixes it", () => {
    expect(kiteMissing(ready)).toEqual([]);
    const fresh = { ...ready, enabled: false, user_id: "", api_key_set: false, api_secret_set: false, access_token_set: false };
    expect(kiteMissing(fresh)).toHaveLength(5);
    expect(kiteMissing(fresh)[0]).toContain("API key");
    expect(kiteMissing({ ...ready, access_token_set: false })).toEqual(["Log in with Zerodha (today's session)."]);
  });

  it("shows every INV-14 condition, and real money needs all of them", () => {
    const status = { live_account: { id: "a", name: "live", mode: "live", currency: "INR", live_armed: false } } as unknown as Status;
    const conditions = liveConditions(ready, status);
    expect(conditions).toHaveLength(5);
    expect(conditions.every(([, ok]) => ok)).toBe(false);
    const all = liveConditions(
      { ...ready, live_trading_enabled: true, live_orders_compiled: true },
      { live_account: { id: "a", name: "live", mode: "live", currency: "INR", live_armed: true } } as unknown as Status,
    );
    expect(all.every(([, ok]) => ok)).toBe(true);
    expect(liveConditions(ready, null)[3]?.[1]).toBe(false);
  });
});

import { issueText } from "../src/views";

describe("data issues", () => {
  it("reads as one plain line each, with the exact prices", () => {
    expect(issueText({ issue: "missing_day", date: "2026-09-22" })).toBe("2026-09-22: no bar on a trading day");
    expect(issueText({ issue: "price_jump", date: "2026-09-24", previous: "101.50", close: "140.00" })).toContain("close 140.00 after 101.50");
  });
});
