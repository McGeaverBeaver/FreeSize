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
    // CSSOM, which the page's security policy allows (style attributes it doesn't).
    else if (key === "style") el.style.cssText = value;
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
  if (s < 86400 * 14) {
    const days = Math.round(s / 86400);
    return days === 1 ? "yesterday" : `${days} days ago`;
  }
  return day(secs);
}

function toast(message, error = false) {
  const el = h("div", { class: error ? "toast error" : "toast", role: "status" }, message);
  document.body.append(el);
  setTimeout(() => el.remove(), error ? 6000 : 3500);
}

const openDialogs = new Set();

/** Shows a dialog; `build(close)` returns its contents. Escape, a click
 * outside and moving to another page close it, then call `onClose`. */
function dialog(build, onClose) {
  const onKey = (e) => { if (e.key === "Escape") close(); };
  const close = () => {
    if (!openDialogs.delete(close)) return;
    backdrop.remove();
    document.removeEventListener("keydown", onKey);
    if (onClose) onClose();
  };
  const backdrop = h("div", { class: "backdrop", onclick: (e) => { if (e.target === backdrop) close(); } });
  const box = h("div", { class: "dialog", role: "dialog", "aria-modal": "true" }, build(close));
  backdrop.append(box);
  document.body.append(backdrop);
  openDialogs.add(close);
  document.addEventListener("keydown", onKey);
  const focus = box.querySelector("input, textarea, select, button.primary");
  if (focus) focus.focus();
  return close;
}

function closeDialogs() {
  for (const close of [...openDialogs]) close();
}

function bar(fraction, level) {
  const pct = Math.max(0, Math.min(100, fraction * 100));
  return h("div", { class: `bar ${level || ""}` }, h("div", { style: `width:${pct.toFixed(1)}%` }));
}

function usageLevel(fraction) {
  return fraction >= 0.95 ? "danger" : fraction >= 0.85 ? "warn" : "";
}

/** An audit entry's details as "key: value · key: value". */
function details(d) {
  return Object.entries(d || {})
    .filter(([, v]) => v !== null && v !== undefined && v !== "")
    .map(([k, v]) => `${k.replace(/_/g, " ")}: ${Array.isArray(v) ? v.join(", ") : typeof v === "object" ? JSON.stringify(v) : v}`)
    .join(" · ");
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

const PLATFORMS = { android: "Android", windows: "Windows", macos: "macOS", linux: "Linux", other: "Other" };

function diskRow(disk) {
  const used = disk.total - disk.free;
  const fraction = disk.total ? used / disk.total : 0;
  return h("div", { class: "disk" },
    h("div", { class: "row" },
      h("span", { class: "grow" }, disk.name || disk.path),
      h("span", { class: "dim" }, `${size(used)} of ${size(disk.total)}`)),
    bar(fraction, usageLevel(fraction)));
}

function seenText(device) {
  return device.online ? "online" : `seen ${ago(device.last_seen)}`;
}

function deviceCard(device) {
  const info = device.info || {};
  const latest = (device.scans || [])[0];
  return h("a", { class: "card device-card", href: `#/device/${encodeURIComponent(device.id)}` },
    h("div", { class: "row" },
      h("h2", { class: "grow", style: "margin:0" }, device.name),
      device.waiting ? h("span", { class: "badge warn" }, count(device.waiting, "request", "requests")) : null,
      h("span", { class: "badge" }, PLATFORMS[device.platform] || device.platform)),
    h("div", { class: "platform" },
      h("span", { class: device.online ? "dot online" : "dot" }), seenText(device),
      ` · ${info.os || PLATFORMS[device.platform] || device.platform} · ${device.group.name}`),
    (device.disks || []).slice(0, 4).map(diskRow),
    !device.disks.length ? h("p", { class: "dim small", style: "margin-top:10px" }, "No disks reported yet.") : null,
    latest ? h("p", { class: "dim small", style: "margin:10px 0 0" },
      `Last scan: ${latest.label || latest.root}, ${size(latest.allocated)}, ${ago(latest.taken)}`) : null);
}

PAGES.devices = async () => {
  const devices = await api("/api/devices");
  const head = h("div", { class: "page-head" },
    h("div", { class: "grow" }, h("h1", {}, "Devices"),
      h("p", { class: "dim" }, devices.length
        ? `${count(devices.length, "device", "devices")}, ${devices.filter((d) => d.online).length} online now`
        : "Phones, computers and servers you pair show their storage here.")),
    state.me.admin ? h("button", { class: "primary", onclick: pairDialog }, "Pair a device") : null);
  if (!devices.length) {
    return [head, h("div", { class: "card empty" },
      h("h2", {}, "No devices yet"),
      h("p", {}, "Pair the DiskTree app on a phone or computer, or the DiskTree agent on a server. Reading their storage needs no approval; anything that deletes is approved on the device itself."),
      state.me.admin ? h("button", { class: "primary", onclick: pairDialog }, "Pair a device") : null)];
  }
  return [head, h("div", { class: "grid" }, devices.map(deviceCard))];
};

async function copy(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast("Copied");
  } catch {
    toast("Select the text and copy it", true);
  }
}

async function pairDialog() {
  const groups = await api("/api/groups");
  const before = new Set((await api("/api/devices")).map((d) => d.id));
  let timer = null;
  const stop = () => clearInterval(timer);
  dialog((close) => {
    const closeAll = () => { stop(); close(); render(); };
    const select = h("select", { id: "pair-group" }, groups.map((g) => h("option", { value: g.id }, g.name)));
    const body = h("div", {},
      h("p", { class: "dim" }, "A one-time code, valid for 10 minutes. The device checks it reached this Hub by its key fingerprint."),
      h("label", { for: "pair-group" }, "Device group"),
      select,
      h("p", { class: "hint" }, "The group decides who can see the device and which grant lengths its owner is offered."));
    const actions = h("div", { class: "actions" },
      h("button", { onclick: closeAll }, "Cancel"),
      h("button", { class: "primary", onclick: make }, "Make a pairing code"));

    async function make() {
      let code;
      try {
        code = await api("/api/pairing-codes", { method: "POST", body: { group_id: Number(select.value) } });
      } catch (e) {
        toast(e.message, true);
        return;
      }
      const status = h("p", { class: "callout" }, "Waiting for the device…");
      const left = h("span", { class: "dim small" });
      const tick = () => {
        const s = Math.max(0, code.expires - Math.floor(Date.now() / 1000));
        left.textContent = s ? `Valid for ${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}` : "Expired: make a new one";
      };
      tick();
      body.replaceChildren(
        h("div", { class: "row", style: "align-items:flex-start;gap:18px" },
          h("img", { class: "qr", src: code.qr, alt: "QR code of the pairing link" }),
          h("div", { class: "grow" },
            h("div", { class: "dim small" }, "Code"),
            h("div", { class: "code-big" }, code.code),
            left,
            h("div", { class: "dim small", style: "margin-top:10px" }, "Hub"),
            h("div", { class: "mono" }, code.hub_url),
            h("div", { class: "dim small", style: "margin-top:10px" }, "Hub key"),
            h("div", { class: "fingerprint" }, code.fingerprint))),
        h("h3", { style: "margin-top:16px" }, "On the device"),
        h("ul", { class: "small" },
          h("li", {}, "Phone: in DiskTree, open Hub → Connect and scan this QR code with the camera, or type the Hub address and the code."),
          h("li", {}, "Computer: in DiskTree, open Hub → Connect and paste the link."),
          h("li", {}, "Server: run ", h("code", {}, `disktree agent pair '${code.link}'`))),
        h("div", { class: "row" },
          h("input", { type: "text", value: code.link, readonly: true, class: "grow mono", onclick: (e) => e.target.select() }),
          h("button", { onclick: () => copy(code.link) }, "Copy link")),
        status);
      actions.replaceChildren(h("button", { onclick: closeAll }, "Done"));
      timer = setInterval(async () => {
        tick();
        const now = await api("/api/devices").catch(() => []);
        const fresh = now.find((d) => !before.has(d.id));
        if (fresh) {
          stop();
          status.className = "callout";
          status.replaceChildren(`${fresh.name} is paired.`);
          status.style.background = "var(--ok-soft)";
        }
      }, 3000);
    }
    return [h("h2", {}, "Pair a device"), body, actions];
  }, stop);
}

// ----- One device ------------------------------------------------------------

const DEVICE_TABS = [
  ["overview", "Overview"], ["folders", "Folders"], ["files", "Largest files"], ["apps", "Apps"], ["activity", "Activity"],
];

PAGES.device = async (id, tab = "overview", ...rest) => {
  const device = await api(`/api/devices/${encodeURIComponent(id)}`);
  const info = device.info || {};
  const tabs = DEVICE_TABS.filter(([key]) => key !== "apps" || device.apps);
  const link = (key) => `#/device/${encodeURIComponent(id)}/${key}`;
  const header = h("div", { class: "page-head" },
    h("div", { class: "grow" },
      h("h1", {}, device.name),
      h("p", { class: "dim" },
        h("span", { class: device.online ? "dot online" : "dot" }), seenText(device),
        ` · ${info.os || PLATFORMS[device.platform] || device.platform}`,
        info.app ? ` · ${info.app}` : "", ` · group ${device.group.name}`)),
    state.me.admin ? h("div", { class: "row" },
      h("button", { onclick: () => renameDevice(device) }, "Rename"),
      h("button", { onclick: () => moveDevice(device) }, "Change group"),
      h("button", { onclick: () => removeDevice(device) }, "Remove")) : null);
  const nav = h("nav", { class: "tabs" }, tabs.map(([key, label]) =>
    h("a", { href: link(key), class: key === tab ? "active" : "" }, label)));
  const views = { overview: deviceOverview, folders: deviceFolders, files: deviceFiles, apps: deviceApps, activity: deviceActivity };
  const view = await (views[tab] || deviceOverview)(device, ...rest);
  return [header, nav, view];
};

/** A small line chart of used space, as SVG. */
function sparkline(samples, width = 280, height = 48) {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", `0 0 ${width} ${height}`);
  svg.setAttribute("width", "100%");
  svg.setAttribute("height", height);
  svg.setAttribute("preserveAspectRatio", "none");
  if (samples.length < 2) return svg;
  const days = samples.map((s) => s[0]);
  const used = samples.map((s) => s[2]);
  const total = Math.max(...samples.map((s) => s[1]));
  const x = (d) => ((d - days[0]) / Math.max(1, days[days.length - 1] - days[0])) * width;
  const y = (u) => height - 2 - (u / Math.max(1, total)) * (height - 4);
  const points = samples.map((s) => `${x(s[0]).toFixed(1)},${y(s[2]).toFixed(1)}`).join(" ");
  const area = document.createElementNS(ns, "polygon");
  area.setAttribute("points", `0,${height} ${points} ${width},${height}`);
  area.setAttribute("fill", "var(--accent-soft)");
  const line = document.createElementNS(ns, "polyline");
  line.setAttribute("points", points);
  line.setAttribute("fill", "none");
  line.setAttribute("stroke", "var(--accent)");
  line.setAttribute("stroke-width", "2");
  svg.append(area, line);
  return svg;
}

function deviceOverview(device) {
  const history = Object.fromEntries((device.history || []).map((h) => [h.path, h.samples]));
  const disks = (device.disks || []).map((disk) => {
    const samples = history[disk.path] || [];
    const used = disk.total - disk.free;
    const fraction = disk.total ? used / disk.total : 0;
    let trend = "";
    if (samples.length >= 2) {
      const first = samples.find((s) => s[0] >= samples[samples.length - 1][0] - 30) || samples[0];
      const change = samples[samples.length - 1][2] - first[2];
      const days = samples[samples.length - 1][0] - first[0];
      if (days > 0) trend = `${change >= 0 ? "+" : "−"}${size(Math.abs(change))} in ${days} days`;
    }
    return h("div", { class: "card" },
      h("div", { class: "row" }, h("h3", { class: "grow", style: "margin:0" }, disk.name || disk.path),
        h("span", { class: `badge ${usageLevel(fraction) || "ok"}` }, `${Math.round(fraction * 100)}% used`)),
      h("p", { class: "dim small" }, disk.path),
      bar(fraction, usageLevel(fraction)),
      h("div", { class: "row small", style: "margin-top:6px" },
        h("span", { class: "grow" }, `${size(used)} used of ${size(disk.total)}`),
        h("span", { class: "dim" }, `${size(disk.free)} free`)),
      samples.length >= 2 ? [sparkline(samples), h("p", { class: "dim small", style: "margin:0" }, trend)] : null);
  });
  const roots = device.roots || [];
  const scans = roots.length
    ? h("div", { class: "card table-wrap" }, h("h2", {}, "Scanned folders"),
        h("table", {}, h("thead", {}, h("tr", {}, h("th", {}, "Folder"), h("th", { class: "num" }, "Size"),
          h("th", { class: "num hide-narrow" }, "Files"), h("th", {}, "Last scan"), h("th", { class: "num hide-narrow" }, "Kept"))),
          h("tbody", {}, roots.map((r) => {
            const s = r.scans[0];
            return h("tr", { class: "clickable", onclick: () => { location.hash = `#/device/${encodeURIComponent(device.id)}/folders/${s.id}`; } },
              h("td", {}, r.label || r.root, h("div", { class: "sub" }, r.root)),
              h("td", { class: "num" }, size(s.allocated)),
              h("td", { class: "num hide-narrow" }, Number(s.files).toLocaleString("en-US")),
              h("td", {}, ago(s.taken), s.complete ? null : h("span", { class: "badge warn", style: "margin-left:6px" }, "partial")),
              h("td", { class: "num hide-narrow" }, r.scans.length));
          }))))
    : h("div", { class: "card empty" }, h("h2", {}, "No scans yet"),
        h("p", {}, "The device sends a summary after each scan: folder sizes and the biggest files."));
  return h("div", { class: "stack" },
    disks.length ? h("div", { class: "grid" }, disks) : h("div", { class: "card dim" }, "No disks reported yet."),
    scans);
}

function scanPicker(device, current, onPick) {
  const select = h("select", { onchange: () => onPick(Number(select.value)) });
  for (const root of device.roots || []) {
    const group = h("optgroup", { label: root.label || root.root });
    for (const s of root.scans) {
      group.append(h("option", { value: s.id, selected: s.id === current }, `${root.label || root.root} · ${date(s.taken)} · ${size(s.allocated)}`));
    }
    select.append(group);
  }
  return select;
}

async function deviceFolders(device, scanArg, pathArg = "") {
  const roots = device.roots || [];
  if (!roots.length) return h("div", { class: "card empty" }, h("h2", {}, "No scans yet"));
  const scanId = Number(scanArg) || roots[0].scans[0].id;
  const path = pathArg || "";
  const folder = await api(`/api/scans/${scanId}/folders?path=${encodeURIComponent(path)}`);
  const base = `#/device/${encodeURIComponent(device.id)}/folders/${scanId}`;
  const go = (p) => { location.hash = p ? `${base}/${encodeURIComponent(p)}` : base; };
  const crumbs = [h("a", { href: base }, folder.scan.label || folder.scan.root)];
  let acc = "";
  for (const part of path ? path.split("/") : []) {
    acc = acc ? `${acc}/${part}` : part;
    const target = acc;
    crumbs.push(" / ", h("a", { href: `${base}/${encodeURIComponent(target)}` }, part));
  }
  const total = Math.max(1, folder.allocated);
  const rows = [
    ...folder.folders.map((f) => h("tr", { class: "clickable", onclick: () => go(f.path) },
      h("td", {}, "📁 ", f.name), h("td", { class: "hide-narrow", style: "width:30%" }, bar(f.allocated / total)),
      h("td", { class: "num" }, size(f.allocated)))),
    ...folder.files.map((f) => h("tr", {},
      h("td", {}, f.name, h("div", { class: "sub" }, f.modified ? `changed ${day(f.modified)}` : "")),
      h("td", { class: "hide-narrow" }, bar(f.allocated / total)), h("td", { class: "num" }, size(f.allocated)))),
  ];
  if (folder.rest > 0) {
    rows.push(h("tr", {}, h("td", { class: "dim" }, "Smaller files and folders"),
      h("td", { class: "hide-narrow" }, bar(folder.rest / total)), h("td", { class: "num dim" }, size(folder.rest))));
  }
  return h("div", { class: "card" },
    h("div", { class: "row", style: "margin-bottom:10px" },
      h("div", { class: "grow" }, crumbs),
      scanPicker(device, scanId, (id) => { location.hash = `#/device/${encodeURIComponent(device.id)}/folders/${id}`; })),
    h("p", { class: "dim small" }, `${size(folder.allocated)} on disk (${size(folder.size)} in files) · scanned ${date(folder.scan.taken)}`),
    h("div", { class: "table-wrap" }, h("table", {}, h("tbody", {}, rows))),
    h("p", { class: "hint" }, "Devices send the sizes of their biggest folders and files, not every file, so small items are added up."));
}

async function deviceFiles(device, scanArg) {
  const roots = device.roots || [];
  if (!roots.length) return h("div", { class: "card empty" }, h("h2", {}, "No scans yet"));
  const scanId = Number(scanArg) || roots[0].scans[0].id;
  const listed = await api(`/api/scans/${scanId}/files`);
  return h("div", { class: "card" },
    h("div", { class: "row", style: "margin-bottom:10px" },
      h("h2", { class: "grow", style: "margin:0" }, "Largest files"),
      scanPicker(device, scanId, (id) => { location.hash = `#/device/${encodeURIComponent(device.id)}/files/${id}`; })),
    h("div", { class: "table-wrap" }, h("table", {},
      h("thead", {}, h("tr", {}, h("th", {}, "File"), h("th", { class: "hide-narrow" }, "Changed"), h("th", { class: "num" }, "Size"))),
      h("tbody", {}, listed.files.map((f) => h("tr", {},
        h("td", {}, f.name, h("div", { class: "sub" }, f.path.includes("/") ? f.path.slice(0, f.path.lastIndexOf("/")) : listed.scan.label || listed.scan.root)),
        h("td", { class: "hide-narrow small" }, day(f.modified)),
        h("td", { class: "num" }, size(f.allocated))))))));
}

function deviceApps(device) {
  const report = device.apps;
  if (!report) return h("div", { class: "card empty" }, h("h2", {}, "No app report"));
  const parts = [];
  const b = report.breakdown;
  if (b) {
    const seg = (value, color, label) => value > 0
      ? h("div", { title: `${label}: ${size(value)}`, style: `width:${(value / b.total * 100).toFixed(2)}%;background:${color};height:100%` }) : null;
    parts.push(h("div", { class: "card" },
      h("h2", {}, "Storage"),
      h("div", { style: "display:flex;height:14px;border-radius:7px;overflow:hidden;background:var(--track)" },
        seg(b.apps, "#2f6fde", "Apps"), seg(b.files, "#23a36b", "Your files"), seg(b.system, "#8a93a3", "System")),
      h("div", { class: "row small", style: "margin-top:8px;gap:16px" },
        h("span", {}, `Apps ${size(b.apps)}`), h("span", {}, `Files ${size(b.files)}`),
        h("span", {}, `System ${size(b.system)}`), h("span", { class: "dim" }, `Free ${size(b.free)} of ${size(b.total)}`))));
  }
  if ((report.junk || []).length) {
    parts.push(h("div", { class: "card" }, h("h2", {}, "Space that can go"),
      h("table", {}, h("tbody", {}, report.junk.map((j) => h("tr", {},
        h("td", {}, j.label, h("div", { class: "sub" }, count(j.items, "item", "items"))),
        h("td", { class: "num" }, size(j.bytes))))))));
  }
  const apps = [...report.apps].sort((a, b2) => (b2.code + b2.data) - (a.code + a.data));
  parts.push(h("div", { class: "card table-wrap" },
    h("h2", {}, `Apps (${apps.length})`),
    h("table", {},
      h("thead", {}, h("tr", {}, h("th", {}, "App"), h("th", { class: "num" }, "Size"), h("th", { class: "num hide-narrow" }, "Cache"), h("th", { class: "hide-narrow" }, "Last used"))),
      h("tbody", {}, apps.slice(0, 300).map((a) => h("tr", {},
        h("td", {}, a.label, h("div", { class: "sub" }, `${a.package}${a.source ? ` · ${a.source}` : ""}`)),
        h("td", { class: "num" }, size(a.code + a.data)),
        h("td", { class: "num hide-narrow" }, size(a.cache)),
        h("td", { class: "hide-narrow small" }, a.last_used ? ago(a.last_used) : "–")))))));
  return h("div", { class: "stack" }, parts);
}

async function deviceActivity(device) {
  const entries = await api(`/api/devices/${encodeURIComponent(device.id)}/audit`);
  if (!entries.length) return h("div", { class: "card dim" }, "Nothing yet.");
  return h("div", { class: "card table-wrap" }, h("table", {},
    h("thead", {}, h("tr", {}, h("th", {}, "When"), h("th", {}, "Who"), h("th", {}, "What"))),
    h("tbody", {}, entries.map((e) => h("tr", {},
      h("td", { class: "small" }, date(e.time)), h("td", {}, e.actor),
      h("td", {}, e.action, details(e.details) ? h("div", { class: "sub" }, details(e.details)) : null))))));
}

function renameDevice(device) {
  dialog((close) => {
    const input = h("input", { type: "text", value: device.name, maxlength: 80 });
    const save = async () => {
      try {
        await api(`/api/devices/${encodeURIComponent(device.id)}`, { method: "PATCH", body: { name: input.value } });
        close();
        render();
      } catch (e) { toast(e.message, true); }
    };
    input.addEventListener("keydown", (e) => { if (e.key === "Enter") save(); });
    return [h("h2", {}, "Rename"), h("label", {}, "Name"), input,
      h("div", { class: "actions" }, h("button", { onclick: close }, "Cancel"), h("button", { class: "primary", onclick: save }, "Save"))];
  });
}

async function moveDevice(device) {
  const groups = await api("/api/groups");
  dialog((close) => {
    const select = h("select", {}, groups.map((g) => h("option", { value: g.id, selected: g.id === device.group.id }, g.name)));
    const save = async () => {
      try {
        await api(`/api/devices/${encodeURIComponent(device.id)}`, { method: "PATCH", body: { group_id: Number(select.value) } });
        close();
        render();
      } catch (e) { toast(e.message, true); }
    };
    return [h("h2", {}, "Change group"), h("label", {}, "Device group"), select,
      h("p", { class: "hint" }, "The group decides who can see the device and the grant lengths offered on it."),
      h("div", { class: "actions" }, h("button", { onclick: close }, "Cancel"), h("button", { class: "primary", onclick: save }, "Move"))];
  });
}

function removeDevice(device) {
  dialog((close) => {
    const go = async () => {
      try {
        await api(`/api/devices/${encodeURIComponent(device.id)}`, { method: "DELETE" });
        close();
        location.hash = "#/devices";
      } catch (e) { toast(e.message, true); }
    };
    return [h("h2", {}, `Remove ${device.name}?`),
      h("p", {}, "The Hub forgets this device and everything it reported. The device stops reporting the next time it calls, and can be paired again with a new code."),
      h("div", { class: "actions" }, h("button", { onclick: close }, "Cancel"), h("button", { class: "danger", onclick: go }, "Remove"))];
  });
}

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
        h("td", { class: "small dim" }, details(e.details))));
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
  const groups = await api("/api/groups");
  const groupCard = h("div", { class: "card table-wrap" },
    h("div", { class: "row" }, h("h2", { class: "grow", style: "margin:0" }, "Device groups"),
      state.me.admin ? h("button", { onclick: () => groupDialog() }, "Add a group") : null),
    h("p", { class: "dim small" }, "A group sets who may look at its devices, and the grant lengths a device offers when it approves a request: \"approve for 10 minutes\" lets the same admin do the same kind of thing again without asking, until the time runs out."),
    h("table", {},
      h("thead", {}, h("tr", {}, h("th", {}, "Group"), h("th", {}, "Grant lengths"), h("th", { class: "hide-narrow" }, "Also visible to"), h("th", { class: "num" }, "Devices"), h("th", {}))),
      h("tbody", {}, groups.map((g) => h("tr", {},
        h("td", {}, g.name),
        h("td", {}, g.grant_minutes.length ? g.grant_minutes.map((m) => `${m} min`).join(", ") : "once only"),
        h("td", { class: "hide-narrow" }, g.viewer_groups.length ? g.viewer_groups.join(", ") : h("span", { class: "dim" }, "admins and viewers")),
        h("td", { class: "num" }, g.devices),
        h("td", { class: "num" }, state.me.admin ? [
          h("button", { class: "link", onclick: () => groupDialog(g) }, "Edit"),
          g.devices === 0 && groups.length > 1 ? h("button", { class: "link", onclick: () => deleteGroup(g) }, "Delete") : null,
        ] : null))))));
  return [
    h("div", { class: "page-head" }, h("div", { class: "grow" }, h("h1", {}, "Settings"))),
    h("div", { class: "stack" },
      h("div", { class: "card" },
        h("h2", {}, "This Hub"),
        h("table", {}, h("tbody", {},
          h("tr", {}, h("td", { class: "dim" }, "Name"), h("td", {}, hub.name)),
          h("tr", {}, h("td", { class: "dim" }, "Version"), h("td", {}, hub.version)),
          h("tr", {}, h("td", { class: "dim" }, "Address"), h("td", {}, hub.url || "not set (DISKTREE_HUB_URL)")),
          h("tr", {}, h("td", { class: "dim" }, "Key fingerprint"), h("td", { class: "fingerprint" }, hub.fingerprint)))),
        h("p", { class: "hint" }, "Devices check this fingerprint when they pair, so they know they reached this Hub.")),
      groupCard,
      h("div", { class: "card" },
        h("h2", {}, "Approvers"),
        h("p", {}, "A server or NAS with nobody at its screen has a designated approver: a phone whose approval it accepts. It can't be set or changed here, only on the device itself, with ",
          h("code", {}, "disktree approver set <fingerprint>"),
          ". So a stolen dashboard sign-in can't send approvals to itself."))),
  ];
};

function groupDialog(group) {
  dialog((close) => {
    const name = h("input", { type: "text", value: group ? group.name : "", maxlength: 60 });
    const minutes = h("input", { type: "text", value: group ? group.grant_minutes.join(", ") : "10, 30" });
    const viewers = h("input", { type: "text", value: group ? group.viewer_groups.join(", ") : "" });
    const save = async () => {
      const body = {
        name: name.value,
        grant_minutes: minutes.value.split(/[,\s]+/).filter(Boolean).map(Number),
        viewer_groups: viewers.value.split(",").map((s) => s.trim()).filter(Boolean),
      };
      if (body.grant_minutes.some((m) => !Number.isInteger(m))) {
        toast("Grant lengths are whole minutes", true);
        return;
      }
      try {
        if (group) await api(`/api/groups/${group.id}`, { method: "PATCH", body });
        else await api("/api/groups", { method: "POST", body });
        close();
        render();
      } catch (e) { toast(e.message, true); }
    };
    return [h("h2", {}, group ? `Edit ${group.name}` : "Add a device group"),
      h("label", {}, "Name"), name,
      h("label", {}, "Grant lengths, in minutes"), minutes,
      h("p", { class: "hint" }, "Offered on the device next to \"Approve once\". Leave empty to only allow approving one request at a time. At most 480."),
      h("label", {}, "Also visible to these sign-in groups"), viewers,
      h("p", { class: "hint" }, "Comma-separated group names from Authentik. Their members see only this group's devices. Admins and the viewer group see everything."),
      h("div", { class: "actions" }, h("button", { onclick: close }, "Cancel"), h("button", { class: "primary", onclick: save }, "Save"))];
  });
}

function deleteGroup(group) {
  dialog((close) => [h("h2", {}, `Delete ${group.name}?`),
    h("div", { class: "actions" }, h("button", { onclick: close }, "Cancel"),
      h("button", { class: "danger", onclick: async () => {
        try { await api(`/api/groups/${group.id}`, { method: "DELETE" }); close(); render(); }
        catch (e) { toast(e.message, true); }
      } }, "Delete"))]);
}

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

window.addEventListener("hashchange", () => {
  closeDialogs();
  render();
});
render();
