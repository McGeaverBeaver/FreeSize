// DiskTree Hub dashboard. Plain JavaScript, no build step. Every piece of
// text from the server goes in through textContent, never innerHTML.
"use strict";

const state = { me: null, status: null };
const app = document.getElementById("app");

// ----- Helpers -------------------------------------------------------------

/** Creates an element: h("div", {class: "x", onclick: f}, "text", child). */
function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs || {})) {
    if (value === null || value === undefined || value === false) continue;
    if (key.startsWith("on")) el.addEventListener(key.slice(2), value);
    else if (key === "class") el.className = value;
    else if (key === "style") el.setAttribute("style", value);
    else if (value === true) el.setAttribute(key, "");
    else el.setAttribute(key, value);
  }
  for (const child of children.flat(Infinity)) {
    if (child === null || child === undefined || child === false) continue;
    el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return el;
}

class ApiError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
}

/** Calls the Hub's API. Changes carry the CSRF token. */
async function api(path, options = {}) {
  const method = options.method || "GET";
  const headers = { Accept: "application/json" };
  let body;
  if (options.body !== undefined) {
    headers["Content-Type"] = "application/json";
    body = JSON.stringify(options.body);
  }
  if (method !== "GET" && state.me) headers["X-CSRF-Token"] = state.me.csrf;
  const response = await fetch(path, { method, headers, body, credentials: "same-origin" });
  if (response.status === 401 && path !== "/api/me") {
    state.me = null;
    render();
    throw new ApiError(401, "Signed out");
  }
  const text = await response.text();
  const data = text ? JSON.parse(text) : null;
  if (!response.ok) throw new ApiError(response.status, (data && data.error) || response.statusText);
  return data;
}

const UNITS = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
/** Like the apps' sizes: binary units, three significant digits. */
function size(bytes) {
  if (bytes === null || bytes === undefined) return "–";
  if (bytes < 1024) return `${bytes} B`;
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit++;
  }
  const digits = value >= 100 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(digits)} ${UNITS[unit]}`;
}

function count(n, one, many) {
  return `${Number(n).toLocaleString("en-US")} ${n === 1 ? one : many}`;
}

function date(secs) {
  if (!secs) return "–";
  return new Date(secs * 1000).toLocaleString(undefined, {
    year: "numeric", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit",
  });
}

function day(secs) {
  if (!secs) return "–";
  return new Date(secs * 1000).toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
}

function ago(secs) {
  if (!secs) return "never";
  const s = Math.max(0, Date.now() / 1000 - secs);
  if (s < 90) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  if (s < 86400 * 14) return `${Math.round(s / 86400)} days ago`;
  return day(secs);
}

function toast(message, error = false) {
  const el = h("div", { class: error ? "toast error" : "toast", role: "status" }, message);
  document.body.append(el);
  setTimeout(() => el.remove(), error ? 6000 : 3500);
}

/** Shows a dialog; `build(close)` returns its contents. */
function dialog(build) {
  const close = () => backdrop.remove();
  const backdrop = h("div", { class: "backdrop", onclick: (e) => { if (e.target === backdrop) close(); } });
  const box = h("div", { class: "dialog", role: "dialog", "aria-modal": "true" }, build(close));
  backdrop.append(box);
  document.body.append(backdrop);
  const focus = box.querySelector("input, textarea, select, button.primary");
  if (focus) focus.focus();
  backdrop.addEventListener("keydown", (e) => { if (e.key === "Escape") close(); });
  return close;
}

function bar(fraction, level) {
  const pct = Math.max(0, Math.min(100, fraction * 100));
  return h("div", { class: `bar ${level || ""}` }, h("div", { style: `width:${pct.toFixed(1)}%` }));
}

function usageLevel(fraction) {
  return fraction >= 0.95 ? "danger" : fraction >= 0.85 ? "warn" : "";
}

function failed(error) {
  return h("div", { class: "callout danger" }, `Couldn't load this: ${error.message}`);
}

// ----- Layout ---------------------------------------------------------------

const NAV = [
  { route: "devices", label: "Devices" },
  { route: "audit", label: "Audit log", admin: true },
  { route: "settings", label: "Settings" },
];

function route() {
  const parts = location.hash.replace(/^#\/?/, "").split("/").map(decodeURIComponent);
  return { name: parts[0] || "devices", args: parts.slice(1) };
}

function shell(content) {
  const current = route().name;
  const nav = NAV.filter((n) => !n.admin || state.me.admin).map((n) =>
    h("a", { href: `#/${n.route}`, class: current === n.route || (n.route === "devices" && current === "device") ? "active" : "" }, n.label));
  return [
    h("header", { class: "topbar" },
      h("a", { class: "brand", href: "#/devices" }, h("img", { src: "/icon.svg", alt: "" }), state.me.hub.name),
      h("nav", { class: "nav" }, nav),
      h("div", { class: "user" },
        h("span", { class: "who" }, state.me.name, state.me.admin ? " · admin" : " · viewer"),
        h("button", { class: "small", onclick: signOut }, "Sign out"))),
    h("main", {}, content),
  ];
}

async function signOut() {
  try {
    const answer = await api("/auth/logout", { method: "POST" });
    state.me = null;
    location.href = answer.redirect || "/";
  } catch (e) {
    toast(e.message, true);
  }
}

function signInPage() {
  const s = state.status || {};
  const next = "/" + location.hash;
  return h("div", { class: "signin" },
    h("img", { src: "/icon.svg", alt: "" }),
    h("h1", {}, s.name || "DiskTree Hub"),
    s.sign_in
      ? [h("p", { class: "dim" }, "Sign in with your account to see your devices."),
         h("a", { class: "button primary", href: `/auth/login?next=${encodeURIComponent(next)}` }, "Sign in")]
      : [h("p", {}, "Sign-in isn't set up yet."),
         h("p", { class: "dim small" }, `Missing: ${s.missing || "settings"}. See the README's DiskTree Hub section for setting up Authentik.`)]);
}

// ----- Pages ----------------------------------------------------------------

const PAGES = {};

PAGES.devices = async () => {
  return [
    h("div", { class: "page-head" }, h("div", { class: "grow" }, h("h1", {}, "Devices"))),
    h("div", { class: "card empty" }, h("h2", {}, "No devices yet"),
      h("p", {}, "Paired phones, computers and servers will show up here.")),
  ];
};

PAGES.audit = async () => {
  const list = h("div", { class: "card table-wrap" }, h("p", { class: "dim" }, "Loading…"));
  const result = h("span", { class: "dim small" });
  let oldest = null;

  async function load(append) {
    const entries = await api(`/api/audit${oldest ? `?before=${oldest}` : ""}`);
    if (!append) list.replaceChildren();
    if (!entries.length && !append) {
      list.append(h("p", { class: "dim" }, "Nothing logged yet."));
      return;
    }
    const table = append && list.querySelector("tbody")
      ? list.querySelector("tbody")
      : (() => {
          const t = h("table", {}, h("thead", {}, h("tr", {}, h("th", {}, "When"), h("th", {}, "Who"), h("th", {}, "What"), h("th", {}, "Details"))), h("tbody"));
          list.append(t);
          return t.querySelector("tbody");
        })();
    for (const e of entries) {
      table.append(h("tr", {},
        h("td", { class: "small" }, date(e.time)),
        h("td", {}, e.actor),
        h("td", {}, e.action),
        h("td", { class: "small dim mono" }, Object.keys(e.details || {}).length ? JSON.stringify(e.details) : "")));
      oldest = e.id;
    }
    list.querySelector(".more")?.remove();
    if (entries.length === 100) list.append(h("p", { class: "more" }, h("button", { onclick: () => load(true) }, "Older entries")));
  }

  async function verify() {
    result.textContent = "Checking…";
    try {
      const check = await api("/api/audit/verify");
      result.textContent = check.broken_at
        ? `Entry ${check.broken_at} ${check.problem}. ${check.entries} entries before it are intact.`
        : `All ${check.entries} entries are intact.`;
      result.className = check.broken_at ? "badge danger" : "badge ok";
    } catch (e) {
      result.textContent = e.message;
    }
  }

  load(false).catch((e) => list.replaceChildren(failed(e)));
  return [
    h("div", { class: "page-head" },
      h("div", { class: "grow" }, h("h1", {}, "Audit log"),
        h("p", { class: "dim" }, "Every sign-in, pairing, request and result. Entries are chained by hash, so a changed or missing entry shows.")),
      h("div", { class: "row" }, result, h("button", { onclick: verify }, "Check the chain"))),
    list,
  ];
};

PAGES.settings = async () => {
  const hub = state.me.hub;
  return [
    h("div", { class: "page-head" }, h("div", { class: "grow" }, h("h1", {}, "Settings"))),
    h("div", { class: "card" },
      h("h2", {}, "This Hub"),
      h("table", {}, h("tbody", {},
        h("tr", {}, h("td", { class: "dim" }, "Name"), h("td", {}, hub.name)),
        h("tr", {}, h("td", { class: "dim" }, "Version"), h("td", {}, hub.version)),
        h("tr", {}, h("td", { class: "dim" }, "Address"), h("td", {}, hub.url || "not set (DISKTREE_HUB_URL)")),
        h("tr", {}, h("td", { class: "dim" }, "Key fingerprint"), h("td", { class: "fingerprint" }, hub.fingerprint)))),
      h("p", { class: "hint" }, "Devices check this fingerprint when they pair, so they know they reached this Hub.")),
  ];
};

// ----- Start ----------------------------------------------------------------

let renderId = 0;

async function render() {
  const id = ++renderId;
  if (!state.me) {
    try {
      state.me = await api("/api/me");
    } catch (e) {
      if (!state.status) state.status = await api("/api/status").catch(() => ({}));
      app.replaceChildren(signInPage());
      return;
    }
  }
  const { name } = route();
  const page = PAGES[name] || PAGES.devices;
  let content;
  try {
    content = await page(...route().args);
  } catch (e) {
    content = failed(e);
  }
  if (id !== renderId) return;
  app.replaceChildren(...shell(content));
  window.scrollTo(0, 0);
}

window.addEventListener("hashchange", render);
render();
