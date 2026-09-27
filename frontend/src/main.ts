import "@fontsource/geist-sans/400.css";
import "@fontsource/geist-sans/500.css";
import "@fontsource/geist-sans/600.css";
import "@fontsource/jetbrains-mono/400.css";
import "./styles.css";

import { ApiError, api, type Me, type Status } from "./api";
import { clear, h } from "./dom";
import { formatIst } from "./format";
import { aiView, backtestView, brokerView, dataView, portfolioView, type Ctx, decisionView, decisionsView, haltsView, journalView, paperView, reviewView, settingsView, strategiesView, validationView, withStepUp } from "./views";

const root = document.getElementById("app");

function modeBadge(status: Status | null): HTMLElement {
  const mode = status?.account?.mode ?? "unknown";
  const live = mode === "live";
  return h("span", { class: live ? "chip short strong" : "chip neutral strong", title: "Account mode" }, live ? "LIVE MONEY" : `${mode.toUpperCase()} MODE`);
}

function haltBadge(status: Status | null): HTMLElement {
  if (!status) return h("span", { class: "chip neutral" }, "status unknown");
  if (!status.halt_state_known) return h("span", { class: "chip short" }, "Kill switch state unknown: entries halted");
  return status.entries_halted
    ? h("a", { class: "chip short", href: "#/halts" }, `Entries halted (${status.active_halts.length})`)
    : h("span", { class: "chip long" }, "Entries allowed");
}

function alertBanner(status: Status | null): HTMLElement | null {
  const alerts = status?.alerts ?? [];
  if (alerts.length === 0) return null;
  return h("div", { class: "alerts", role: "alert" }, ...alerts.map((a) => h("p", { class: a.severity === "critical" ? "chip short" : "chip neutral" }, `${a.severity === "critical" ? "CRITICAL" : "Warning"}: ${a.message}`)));
}

const NAV: Array<[string, string]> = [
  ["#/decisions", "Decisions"],
  ["#/portfolio", "Portfolio"],
  ["#/paper", "Paper"],
  ["#/broker", "Broker"],
  ["#/data", "Data"],
  ["#/halts", "Kill switch"],
  ["#/strategies", "Strategies"],
  ["#/validation", "Validation"],
  ["#/review", "Review"],
  ["#/ai", "AI"],
  ["#/backtest", "Backtest"],
  ["#/journal", "Journal"],
  ["#/settings", "Settings"],
];

async function render(ctx: Ctx): Promise<void> {
  if (!root) return;
  const route = location.hash || "#/decisions";
  const main = h("main", {}, h("p", { class: "muted" }, "Loading…"));
  const banner = alertBanner(ctx.status);
  const header = h(
    "header",
    { class: "topbar" },
    h("div", { class: "brand" }, h("img", { src: "/mark.svg", alt: "", width: "28", height: "28" }), h("span", {}, "QuantDesk"), modeBadge(ctx.status), haltBadge(ctx.status)),
    h("nav", {}, ...NAV.map(([href, label]) => h("a", { href, class: route.startsWith(href) ? "active" : "" }, label))),
    h("div", { class: "user" }, h("span", { class: "muted" }, `${ctx.me.username} (${ctx.me.role})`), liveArmToggle(ctx), h("button", { class: "ghost", onclick: async () => { await api.logout(); location.reload(); } }, "Log out")),
  );
  clear(root);
  root.append(header, ...(banner ? [banner] : []), main, h("footer", { class: "muted small" }, ctx.status ? `Environment: ${ctx.status.environment} · times in IST · updated ${formatIst(ctx.status.now)}` : ""));
  const rerender = () => void render(ctx);
  try {
    let view: HTMLElement;
    if (route.startsWith("#/decisions/")) view = await decisionView(decodeURIComponent(route.slice("#/decisions/".length)));
    else if (route.startsWith("#/halts")) view = await haltsView(ctx, rerender);
    else if (route.startsWith("#/paper")) view = await paperView(ctx, rerender);
    else if (route.startsWith("#/broker")) view = await brokerView(ctx, rerender);
    else if (route.startsWith("#/data")) view = await dataView(ctx, rerender);
    else if (route.startsWith("#/portfolio")) view = await portfolioView();
    else if (route.startsWith("#/validation")) view = await validationView(ctx, rerender);
    else if (route.startsWith("#/review")) view = await reviewView(ctx, rerender);
    else if (route.startsWith("#/ai")) view = await aiView(ctx, rerender);
    else if (route.startsWith("#/settings")) view = await settingsView(ctx, rerender);
    else if (route.startsWith("#/strategies")) view = await strategiesView(ctx, rerender);
    else if (route.startsWith("#/backtest")) view = await backtestView(ctx);
    else if (route.startsWith("#/journal")) view = await journalView();
    else view = await decisionsView();
    clear(main);
    main.append(view);
  } catch (e) {
    clear(main);
    main.append(h("p", { class: "error" }, e instanceof Error ? e.message : String(e)));
  }
}

function liveArmToggle(ctx: Ctx): HTMLElement | null {
  const account = ctx.status?.account;
  if (ctx.me.role !== "owner" || account?.mode !== "live") return null;
  return h("button", { class: account.live_armed ? "danger" : "ghost", onclick: async () => {
    const arming = !account.live_armed;
    if (arming && !window.confirm("Arm LIVE trading with real money on this account?")) return;
    try { await withStepUp(() => api.setLiveArmed(arming)); await ctx.refreshStatus(); void render(ctx); } catch (e) { window.alert(e instanceof Error ? e.message : String(e)); }
  } }, account.live_armed ? "Live armed · disarm" : "Arm live");
}

function loginScreen(onDone: (me: Me) => void): HTMLElement {
  const username = h("input", { autocomplete: "username", "aria-label": "Username", placeholder: "Username" });
  const password = h("input", { type: "password", autocomplete: "current-password", "aria-label": "Password", placeholder: "Password" });
  const message = h("p", { class: "error", role: "alert" });
  return h("main", { class: "login" }, h("form", { class: "card stack", onsubmit: async (e: Event) => {
    e.preventDefault();
    try { onDone(await api.login(username.value, password.value)); } catch (err) { message.textContent = err instanceof ApiError && err.status === 429 ? "Too many attempts. Try again later." : "Invalid username or password."; }
  } }, h("div", { class: "brand" }, h("img", { src: "/mark.svg", alt: "", width: "32", height: "32" }), h("span", {}, "QuantDesk")), username, password, h("button", { class: "primary" }, "Log in"), message));
}

async function start(me: Me): Promise<void> {
  const ctx: Ctx = { me, status: null, refreshStatus: async () => { ctx.status = await api.status().catch(() => null); } };
  await ctx.refreshStatus();
  window.addEventListener("hashchange", () => void render(ctx));
  window.setInterval(() => void ctx.refreshStatus().then(() => {
    const brand = document.querySelector(".topbar .brand");
    if (brand) { clear(brand); brand.append(h("img", { src: "/mark.svg", alt: "", width: "28", height: "28" }), h("span", {}, "QuantDesk"), modeBadge(ctx.status), haltBadge(ctx.status)); }
  }), 15000);
  await render(ctx);
}

async function boot(): Promise<void> {
  if (!root) return;
  try {
    await start(await api.me());
  } catch {
    clear(root);
    root.append(loginScreen((me) => void start(me)));
  }
}

void boot();
