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
  if (options.raw !== undefined) {
    // A file, sent as it is.
    headers["Content-Type"] = "application/octet-stream";
    body = options.raw;
  } else if (options.body !== undefined) {
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
  { route: "search", label: "Search" },
  { route: "requests", label: "Requests" },
  { route: "alerts", label: "Alerts" },
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
  if (device.agentless) return `no agent · updated ${ago(device.last_seen)}`;
  return device.online ? "online" : `seen ${ago(device.last_seen)}`;
}

function deviceCard(device) {
  const info = device.info || {};
  const latest = (device.scans || [])[0];
  return h("a", { class: "card device-card", href: `#/device/${encodeURIComponent(device.id)}` },
    h("div", { class: "row" },
      h("h2", { class: "grow", style: "margin:0" }, device.name),
      device.alerts ? h("span", { class: "badge danger" }, count(device.alerts, "alert", "alerts")) : null,
      device.waiting ? h("span", { class: "badge warn" }, count(device.waiting, "request", "requests")) : null,
      h("span", { class: "badge" }, PLATFORMS[device.platform] || device.platform)),
    h("div", { class: "platform" },
      h("span", { class: device.online ? "dot online" : "dot" }), seenText(device),
      ` · ${info.os || PLATFORMS[device.platform] || device.platform} · ${device.group.name}`),
    (device.disks || []).slice(0, 4).map(diskRow),
    !device.disks.length && !device.agentless ? h("p", { class: "dim small", style: "margin-top:10px" }, "No disks reported yet.") : null,
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
    h("div", { class: "row" },
      devices.length ? h("a", { class: "button", href: "/api/devices/export?format=csv" }, "Export CSV") : null,
      state.me.admin ? h("button", { onclick: addDeviceDialog }, "Add without an agent") : null,
      state.me.admin ? h("button", { class: "primary", onclick: pairDialog }, "Pair a device") : null));
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

async function addDeviceDialog() {
  const groups = await api("/api/groups");
  dialog((close) => {
    const name = h("input", { type: "text", id: "new-name", maxlength: 80, placeholder: "e.g. Router or VPS" });
    const group = h("select", { id: "new-group" }, groups.map((g) => h("option", { value: g.id }, g.name)));
    const platform = h("select", { id: "new-platform" },
      Object.entries(PLATFORMS).map(([value, label]) => h("option", { value, selected: value === "linux" }, label)));
    const add = async () => {
      try {
        const made = await api("/api/devices", {
          method: "POST",
          body: { name: name.value, group_id: Number(group.value), platform: platform.value },
        });
        close();
        location.hash = `#/device/${encodeURIComponent(made.id)}`;
      } catch (e) { toast(e.message, true); }
    };
    name.addEventListener("keydown", (e) => { if (e.key === "Enter") add(); });
    return [h("h2", {}, "Add a device without an agent"),
      h("p", { class: "dim" }, "For a machine that can't run the DiskTree agent, such as a router, a VPS or a Proxmox host: scan it with ncdu and upload the file. The Hub can look at it but can't ask it to do anything."),
      h("label", { for: "new-name" }, "Name"), name,
      h("label", { for: "new-group" }, "Device group"), group,
      h("label", { for: "new-platform" }, "System"), platform,
      h("div", { class: "actions" }, h("button", { onclick: close }, "Cancel"), h("button", { class: "primary", onclick: add }, "Add"))];
  });
}

function uploadDialog(device) {
  dialog((close) => {
    const file = h("input", { type: "file", id: "scan-file", accept: ".json,.gz,application/json,application/gzip" });
    const label = h("input", { type: "text", id: "scan-label", maxlength: 80, placeholder: "Optional, e.g. Root disk" });
    const status = h("p", { class: "dim small" });
    const send = h("button", { class: "primary", onclick: upload }, "Upload");
    async function upload() {
      const chosen = file.files[0];
      if (!chosen) {
        toast("Choose a scan file first.", true);
        return;
      }
      send.disabled = true;
      status.textContent = `Uploading ${size(chosen.size)}…`;
      try {
        const done = await api(`/api/devices/${encodeURIComponent(device.id)}/scans?label=${encodeURIComponent(label.value)}`,
          { method: "POST", raw: chosen });
        close();
        toast(`Added a scan of ${done.root}: ${size(done.size)} in ${count(done.files, "file", "files")}.`);
        render();
      } catch (e) {
        send.disabled = false;
        status.textContent = "";
        toast(e.message, true);
      }
    }
    return [h("h2", {}, `Upload a scan of ${device.name}`),
      h("p", {}, "On that machine, scan a folder with ncdu and save the result:"),
      h("pre", { class: "command" }, "ncdu -1xo- / | gzip > scan.json.gz"),
      h("p", { class: "hint" }, "Any ncdu writes this with -o (not ncdu 2.6's binary -O). DiskTree's own export works too: disktree /path --export scan.json. Up to 128 MB unpacked; for bigger machines, run the agent."),
      h("label", { for: "scan-file" }, "Scan file"), file,
      h("label", { for: "scan-label" }, "Name for the folder"), label,
      status,
      h("div", { class: "actions" }, h("button", { onclick: close }, "Cancel"), send)];
  });
}

// ----- One device ------------------------------------------------------------

const DEVICE_TABS = [
  ["overview", "Overview"], ["unraid", "Unraid"], ["folders", "Folders"], ["files", "Largest files"],
  ["changes", "Changes"], ["apps", "Apps"], ["quarantine", "Quarantine"], ["requests", "Requests"],
  ["activity", "Activity"],
];

PAGES.device = async (id, tab = "overview", ...rest) => {
  const device = await api(`/api/devices/${encodeURIComponent(id)}`);
  const info = device.info || {};
  const tabs = DEVICE_TABS.filter(([key]) => (key !== "apps" || device.apps)
    && (key !== "requests" || !device.agentless)
    && (key !== "unraid" || device.unraid)
    && (key !== "quarantine" || device.quarantine));
  const link = (key) => `#/device/${encodeURIComponent(id)}/${key}`;
  const header = h("div", { class: "page-head" },
    h("div", { class: "grow" },
      h("h1", {}, device.name),
      h("p", { class: "dim" },
        h("span", { class: device.online ? "dot online" : "dot" }),
        device.agentless ? `updated ${ago(device.last_seen)}` : seenText(device),
        ` · ${info.os || PLATFORMS[device.platform] || device.platform}`,
        info.app && !device.agentless ? ` · ${info.app}` : "", ` · group ${device.group.name}`),
      h("p", { class: "dim small" },
        device.agentless ? "No agent runs here: its scans are uploaded as files, and the Hub can't ask it to do anything."
          : info.attended ? "Requests are approved on the device itself."
          : info.approver ? ["Nobody is at this device; requests go to its approver, ",
              device.approver_name ? h("strong", {}, device.approver_name) : "a device", " (key ", h("span", { class: "mono" }, info.approver), ")."]
          : "Nobody is at this device and it has no approver yet, so it can't be asked to do anything.",
        (info.grants || []).filter((g) => g.expires * 1000 > Date.now()).map((g) =>
          h("span", { class: "badge accent", style: "margin-left:8px" }, `${g.admin_name} may ${g.kind === "delete" ? "delete files" : g.kind === "uninstall" ? "uninstall apps" : "clean up"} until ${new Date(g.expires * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`)))),
    h("div", { class: "row" },
      state.me.admin && device.agentless ? h("button", { class: "primary", onclick: () => uploadDialog(device) }, "Upload a scan") : null,
      h("a", { class: "button", href: `/api/devices/${encodeURIComponent(id)}/report`, target: "_blank", rel: "noopener" }, "Report"),
      state.me.admin ? [
        h("button", { onclick: () => renameDevice(device) }, "Rename"),
        h("button", { onclick: () => moveDevice(device) }, "Change group"),
        h("button", { onclick: () => removeDevice(device) }, "Remove")] : null));
  const nav = h("nav", { class: "tabs" }, tabs.map(([key, label]) =>
    h("a", { href: link(key), class: key === tab ? "active" : "" }, label)));
  const views = {
    overview: deviceOverview, unraid: deviceUnraid, quarantine: deviceQuarantine,
    folders: deviceFolders, files: deviceFiles, changes: deviceChanges, apps: deviceApps,
    requests: deviceRequests, activity: deviceActivity,
  };
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

async function deviceOverview(device) {
  const alerts = device.alerts ? (await api("/api/alerts").catch(() => ({ active: [] }))).active
    .filter((a) => a.device_id === device.id && a.kind !== "request") : [];
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
        h("p", {}, device.agentless
          ? "Upload a scan file made with ncdu to see this machine's folders."
          : "The device sends a summary after each scan: folder sizes and the biggest files."));
  return h("div", { class: "stack" },
    alerts.length ? h("div", { class: "callout danger" }, alerts.map((a) =>
      h("div", {}, h("strong", {}, a.title.replace(`${device.name}: `, "")), " ", h("span", { class: "dim" }, a.message)))) : null,
    disks.length ? h("div", { class: "grid" }, disks) : device.agentless ? null : h("div", { class: "card dim" }, "No disks reported yet."),
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
  const pick = canDo(device, "delete") ? deleteBar(device, folder.scan.root, folder.scan.label || folder.scan.root) : null;
  const check = (item) => pick ? h("td", { style: "width:28px" }, pick.box(item.path, item.allocated)) : null;
  const rows = [
    ...folder.folders.map((f) => h("tr", { class: "clickable", onclick: () => go(f.path) },
      check(f),
      h("td", {}, "📁 ", f.name), h("td", { class: "hide-narrow", style: "width:30%" }, bar(f.allocated / total)),
      h("td", { class: "num" }, size(f.allocated)))),
    ...folder.files.map((f) => h("tr", {},
      check(f),
      h("td", {}, f.name, h("div", { class: "sub" }, f.modified ? `changed ${day(f.modified)}` : "")),
      h("td", { class: "hide-narrow" }, bar(f.allocated / total)), h("td", { class: "num" }, size(f.allocated)))),
  ];
  if (folder.rest > 0) {
    rows.push(h("tr", {}, pick ? h("td") : null, h("td", { class: "dim" }, "Smaller files and folders"),
      h("td", { class: "hide-narrow" }, bar(folder.rest / total)), h("td", { class: "num dim" }, size(folder.rest))));
  }
  return h("div", { class: "card" },
    h("div", { class: "row", style: "margin-bottom:10px" },
      h("div", { class: "grow" }, crumbs),
      scanPicker(device, scanId, (id) => { location.hash = `#/device/${encodeURIComponent(device.id)}/folders/${id}`; }),
      exportLinks(`/api/scans/${scanId}/export`)),
    h("p", { class: "dim small" }, `${size(folder.allocated)} on disk (${size(folder.size)} in files) · scanned ${date(folder.scan.taken)}`),
    h("div", { class: "table-wrap" }, h("table", {}, h("tbody", {}, rows))),
    pick ? pick.bar : null,
    h("p", { class: "hint" }, "Devices send the sizes of their biggest folders and files, not every file, so small items are added up.",
      pick ? " Tick items to ask the device to delete them; it asks before doing anything." : ""));
}

async function deviceFiles(device, scanArg) {
  const roots = device.roots || [];
  if (!roots.length) return h("div", { class: "card empty" }, h("h2", {}, "No scans yet"));
  const scanId = Number(scanArg) || roots[0].scans[0].id;
  const listed = await api(`/api/scans/${scanId}/files`);
  const pick = canDo(device, "delete") ? deleteBar(device, listed.scan.root, listed.scan.label || listed.scan.root) : null;
  return h("div", { class: "card" },
    h("div", { class: "row", style: "margin-bottom:10px" },
      h("h2", { class: "grow", style: "margin:0" }, "Largest files"),
      scanPicker(device, scanId, (id) => { location.hash = `#/device/${encodeURIComponent(device.id)}/files/${id}`; })),
    h("div", { class: "table-wrap" }, h("table", {},
      h("thead", {}, h("tr", {}, pick ? h("th") : null, h("th", {}, "File"), h("th", { class: "hide-narrow" }, "Changed"), h("th", { class: "num" }, "Size"))),
      h("tbody", {}, listed.files.map((f) => h("tr", {},
        pick ? h("td", { style: "width:28px" }, pick.box(f.path, f.allocated)) : null,
        h("td", {}, f.name, h("div", { class: "sub" }, f.path.includes("/") ? f.path.slice(0, f.path.lastIndexOf("/")) : listed.scan.label || listed.scan.root)),
        h("td", { class: "hide-narrow small" }, day(f.modified)),
        h("td", { class: "num" }, size(f.allocated))))))),
    pick ? pick.bar : null);
}

/** CSV and JSON download links for an export address. */
function exportLinks(base) {
  const join = base.includes("?") ? "&" : "?";
  return h("span", { class: "row exports" },
    h("a", { class: "button small", href: `${base}${join}format=csv`, title: "Download as CSV" }, "CSV"),
    h("a", { class: "button small", href: `${base}${join}format=json`, title: "Download as JSON" }, "JSON"));
}

const CHANGE_KINDS = {
  grew: ["grew", "warn"], shrank: ["shrank", "ok"], new: ["new", "accent"], gone: ["gone", ""],
};

async function deviceChanges(device, toArg, fromArg) {
  const roots = (device.roots || []).filter((r) => r.scans.length >= 2);
  const root = roots.find((r) => r.scans.some((s) => s.id === Number(toArg))) || roots[0];
  if (!root) {
    return h("div", { class: "card empty" }, h("h2", {}, "Nothing to compare yet"),
      h("p", {}, "Changes show once a folder has been scanned twice. The Hub keeps every scan for a week, then one a day for three months, one a week for a year, and one a month after that."));
  }
  const to = root.scans.find((s) => s.id === Number(toArg)) || root.scans[0];
  const others = root.scans.filter((s) => s.id !== to.id);
  const from = others.find((s) => s.id === Number(fromArg))
    || others.find((s) => s.taken <= to.taken - 30 * 86400)
    || others[others.length - 1];
  const result = await api(`/api/scans/${to.id}/compare?with=${from.id}`);
  const base = `#/device/${encodeURIComponent(device.id)}/changes`;
  const go = (t, f) => { location.hash = `${base}/${t}/${f}`; };
  const pick = (current, onPick) => h("select", { class: "auto", onchange: (e) => onPick(Number(e.target.value)) },
    root.scans.map((s) => h("option", { value: s.id, selected: s.id === current }, `${date(s.taken)} · ${size(s.allocated)}`)));
  const rootPick = roots.length > 1
    ? h("select", { class: "auto", onchange: (e) => go(Number(e.target.value), "") },
        roots.map((r) => h("option", { value: r.scans[0].id, selected: r === root }, r.label || r.root)))
    : h("strong", {}, root.label || root.root);
  const older = result.from;
  const newer = result.to;
  const total = newer.allocated - older.allocated;
  let filter = "all";
  const tbody = h("tbody");
  const filters = h("div", { class: "filters" });
  const open = (change) => {
    const scan = change.kind === "gone" ? older.id : newer.id;
    location.hash = `#/device/${encodeURIComponent(device.id)}/folders/${scan}${change.path ? `/${encodeURIComponent(change.path)}` : ""}`;
  };
  const draw = () => {
    const rows = result.changes.filter((c) => filter === "all" || c.kind === filter);
    tbody.replaceChildren(...(rows.length ? rows.map((c) => {
      const delta = c.after - c.before;
      const [label, tone] = CHANGE_KINDS[c.kind];
      return h("tr", { class: "clickable", onclick: () => open(c) },
        h("td", { style: "width:80px" }, h("span", { class: `badge ${tone}` }, label)),
        h("td", {}, c.path || "(the folder itself)"),
        h("td", { class: "num hide-narrow dim" }, c.kind === "new" ? "–" : size(c.before)),
        h("td", { class: "num hide-narrow dim" }, c.kind === "gone" ? "–" : size(c.after)),
        h("td", { class: `num ${delta > 0 ? "up" : "down"}` }, `${delta >= 0 ? "+" : "−"}${size(Math.abs(delta))}`));
    }) : [h("tr", {}, h("td", { colspan: 5, class: "dim" }, "Nothing here."))]));
    filters.replaceChildren(...["all", "grew", "shrank", "new", "gone"].map((kind) => {
      const n = kind === "all" ? result.changes.length : result.changes.filter((c) => c.kind === kind).length;
      return h("button", { class: `small ${filter === kind ? "on" : ""}`, onclick: () => { filter = kind; draw(); } },
        `${kind === "all" ? "All" : CHANGE_KINDS[kind][0][0].toUpperCase() + CHANGE_KINDS[kind][0].slice(1)} (${n})`);
    }));
  };
  draw();
  return h("div", { class: "card" },
    h("div", { class: "row", style: "margin-bottom:12px" },
      h("div", { class: "grow row" }, rootPick, h("span", { class: "dim" }, "from"),
        pick(older.id, (id) => go(newer.id, id)), h("span", { class: "dim" }, "to"), pick(newer.id, (id) => go(id, older.id))),
      exportLinks(`/api/scans/${newer.id}/compare?with=${older.id}`)),
    h("p", {},
      h("strong", { class: total > 0 ? "up" : "down" }, `${total >= 0 ? "+" : "−"}${size(Math.abs(total))}`),
      ` in ${Math.max(1, Math.round((newer.taken - older.taken) / 86400))} days, from ${size(older.allocated)} to ${size(newer.allocated)}.`,
      h("span", { class: "dim" }, ` Folders that changed by less than ${size(result.min_change)} are left out; where one folder explains most of a change, it's listed instead of the folders around it.`)),
    filters,
    h("div", { class: "table-wrap" }, h("table", {},
      h("thead", {}, h("tr", {}, h("th", {}), h("th", {}, "Folder"), h("th", { class: "num hide-narrow" }, "Before"),
        h("th", { class: "num hide-narrow" }, "After"), h("th", { class: "num" }, "Change"))),
      tbody)));
}

// ----- Unraid ---------------------------------------------------------------

const SAFETY = {
  safe: ["safe to remove", "ok"],
  check: ["check first", "warn"],
  protected: ["in use", ""],
};
const DISK_KINDS = { parity: "parity", pool: "pool", flash: "flash" };

function bytesAgo(secs) {
  return secs ? ago(secs) : "never";
}

/** A finding's row, with what can be asked of it. */
function findingRow(device, f) {
  const [label, tone] = SAFETY[f.safety] || ["", ""];
  let ask = null;
  if (canDo(device, "clean") && f.safety !== "protected" && ["image", "dangling_image", "build_cache"].includes(f.kind)) {
    ask = h("button", { class: "small", onclick: () => requestDialog(device,
      { type: "docker_cleanup", items: [f.id], labels: [f.label], bytes: f.bytes },
      [`Remove ${f.label} from Docker (${size(f.bytes)})`, f.reason]) }, "Remove…");
  } else if (canDo(device, "delete") && f.safety === "check" && f.path && f.path.startsWith("/mnt/user/")) {
    ask = h("button", { class: "small", onclick: () => requestDialog(device,
      { type: "delete", root: "/mnt/user", paths: [f.path.slice("/mnt/user/".length)], bytes: f.bytes },
      [`Move to the quarantine: ${f.path} (${size(f.bytes)})`, f.reason,
       "It can be put back until the quarantine is emptied."]) }, "Quarantine…");
  }
  return h("tr", {},
    h("td", { style: "width:110px" }, h("span", { class: `badge ${tone}` }, label)),
    h("td", {}, f.label, h("div", { class: "sub" }, f.reason)),
    h("td", { class: "num" }, f.bytes ? size(f.bytes) : "–"),
    h("td", { class: "num", style: "width:110px" }, ask));
}

function findingsTable(device, findings) {
  if (!findings.length) return h("p", { class: "dim" }, "Nothing found.");
  return h("div", { class: "table-wrap" }, h("table", {}, h("tbody", {}, findings.map((f) => findingRow(device, f)))));
}

function deviceUnraid(device) {
  const u = device.unraid;
  const diskIndex = new Map(u.disks.filter((d) => d.kind !== "parity").map((d, i) => [d.name, i % 8]));
  const array = h("div", { class: "card table-wrap" },
    h("div", { class: "row" }, h("h2", { class: "grow", style: "margin:0" }, "Array"),
      h("span", { class: `badge ${u.started ? "ok" : "warn"}` }, u.started ? "started" : "stopped"),
      h("span", { class: "dim small" }, `Unraid ${u.version} · looked ${ago(u.taken)}`)),
    h("table", { style: "margin-top:8px" },
      h("thead", {}, h("tr", {}, h("th", {}, "Disk"), h("th", { class: "hide-narrow" }, "Used"),
        h("th", { class: "num" }, "Free"), h("th", { class: "num hide-narrow" }, "Temp"), h("th", { class: "hide-narrow" }, "Scanned"))),
      h("tbody", {}, u.disks.map((d) => {
        const fraction = d.size ? (d.size - d.free) / d.size : 0;
        return h("tr", {},
          h("td", {}, diskIndex.has(d.name) && d.kind !== "flash" ? h("span", { class: `swatch seg-${diskIndex.get(d.name)}` }) : h("span", { class: "swatch" }), d.name,
            DISK_KINDS[d.kind] ? h("span", { class: "badge", style: "margin-left:6px" }, DISK_KINDS[d.kind]) : null,
            d.asleep ? h("span", { class: "badge accent", style: "margin-left:6px" }, "asleep") : null,
            h("div", { class: "sub" }, [d.device, d.fs, d.pool ? `in ${d.pool}` : null].filter(Boolean).join(" · "))),
          h("td", { class: "hide-narrow", style: "width:30%" }, d.kind === "parity" ? h("span", { class: "dim small" }, size(d.size)) : bar(fraction, usageLevel(fraction))),
          h("td", { class: "num" }, d.kind === "parity" ? "–" : size(d.free)),
          h("td", { class: "num hide-narrow" }, d.temp != null ? `${d.temp} °C` : "–"),
          h("td", { class: "hide-narrow small" }, d.kind === "parity" || d.kind === "flash" ? "" : bytesAgo(d.scanned)));
      }))),
    h("p", { class: "hint" }, "Sleeping disks aren't woken to scan; their last scan stands until they're awake at scan time (or a week has passed)."));

  const largest = Math.max(1, ...u.shares.map((sh) => sh.total));
  const shares = h("div", { class: "card table-wrap" }, h("h2", {}, "Shares and their disks"),
    h("table", {}, h("tbody", {}, u.shares.map((sh) => h("tr", {},
      h("td", { style: "width:22%" }, sh.name, h("div", { class: "sub" }, [sh.comment, sh.cache].filter(Boolean).join(" · "))),
      h("td", {},
        h("div", { class: "stack-bar", style: `width:${Math.max(2, (sh.total / largest) * 100).toFixed(1)}%` },
          sh.placements.map((p) => h("div", {
            class: `seg seg-${diskIndex.get(p.disk) ?? 0}${p.stale ? " stale" : ""}`,
            style: `width:${sh.total ? ((p.bytes / sh.total) * 100).toFixed(2) : 0}%`,
            title: `${p.disk}: ${size(p.bytes)}${p.stale ? " (last scan)" : ""}`,
          }))),
        h("div", { class: "sub" }, sh.placements.map((p) => `${p.disk} ${size(p.bytes)}${p.stale ? " (earlier scan)" : ""}`).join(" · "))),
      h("td", { class: "num" }, size(sh.total), sh.complete ? null : h("div", { class: "sub" }, "partly from earlier scans")))))));

  const sections = [array, shares];
  const d = u.docker;
  if (d) {
    const safe = d.findings.filter((f) => f.safety === "safe" && ["dangling_image", "build_cache", "image"].includes(f.kind));
    const safeBytes = safe.reduce((sum, f) => sum + f.bytes, 0);
    sections.push(h("div", { class: "card" },
      h("div", { class: "row" }, h("h2", { class: "grow", style: "margin:0" }, "Docker"),
        safe.length && canDo(device, "clean") ? h("button", { class: "small", onclick: () => requestDialog(device,
          { type: "docker_cleanup", items: safe.map((f) => f.id), labels: safe.map((f) => f.label), bytes: safeBytes },
          [`Remove what's safe to remove (${size(safeBytes)}):`, ...safe.map((f) => `${f.label} (${size(f.bytes)})`)]) },
          `Remove all safe (${size(safeBytes)})…`) : null),
      h("p", { class: "dim small" }, d.running ? `${d.running_containers} of ${d.containers} containers running` : "Docker isn't running",
        ` · ${d.mode === "folder" ? "folder" : "docker.img"} ${d.location}`,
        d.image_size ? ` · ${size(d.image_used)} used of ${size(d.image_size)}` : ""),
      findingsTable(device, d.findings)));
  }
  const v = u.vms;
  if (v) {
    sections.push(h("div", { class: "card" }, h("h2", {}, "VMs"),
      v.vms.length ? h("div", { class: "table-wrap" }, h("table", {}, h("tbody", {}, v.vms.map((vm) => h("tr", {},
        h("td", {}, vm.name, " ", h("span", { class: `badge ${vm.running ? "ok" : ""}` }, vm.running ? "running" : "stopped"),
          vm.snapshots ? h("span", { class: "badge", style: "margin-left:6px" }, count(vm.snapshots, "snapshot", "snapshots")) : null),
        h("td", { class: "small" }, vm.disks.map((disk) => h("div", {}, disk.path,
          h("span", { class: "dim" }, disk.provisioned != null ? ` · ${size(disk.used)} used of ${size(disk.provisioned)}` : " · on a sleeping disk"))),
          vm.isos.map((iso) => h("div", { class: "dim" }, `CD: ${iso}`)))))))) : h("p", { class: "dim" }, v.enabled ? "No VMs." : "The VM service is off."),
      v.findings.some((f) => f.safety !== "protected") ? [h("h3", { style: "margin-top:14px" }, "Files no VM uses"),
        findingsTable(device, v.findings.filter((f) => f.safety !== "protected"))] : null));
  }
  if (u.activity.length) {
    sections.push(h("div", { class: "card table-wrap" }, h("h2", {}, "What keeps disks awake"),
      h("table", {}, h("thead", {}, h("tr", {}, h("th", {}, "Disk"), h("th", { class: "num" }, "Spin-ups"), h("th", { class: "num hide-narrow" }, "Awake"),
        h("th", { class: "num hide-narrow" }, "Read"), h("th", { class: "num hide-narrow" }, "Written"), h("th", {}, "Open now"))),
        h("tbody", {}, u.activity.map((a) => h("tr", {},
          h("td", {}, a.disk),
          h("td", { class: "num" }, a.wakeups),
          h("td", { class: "num hide-narrow" }, a.awake_minutes >= 60 ? `${(a.awake_minutes / 60).toFixed(1)} h` : `${a.awake_minutes} min`),
          h("td", { class: "num hide-narrow" }, size(a.reads * 512)),
          h("td", { class: "num hide-narrow" }, size(a.writes * 512)),
          h("td", { class: "small" }, a.open.length ? a.open.map((o) => h("div", {}, o.container ? `${o.container}: ` : `${o.process}: `, h("span", { class: "dim" }, o.path))) : h("span", { class: "dim" }, "nothing")))))),
      h("p", { class: "hint" }, "Over the last day. A disk that keeps waking up is usually read by an app on a schedule: move what it reads to a pool, or change the app's schedule.")));
  }
  if (u.permissions.length) {
    sections.push(h("div", { class: "card" }, h("h2", {}, "Permissions"),
      h("p", { class: "dim small" }, "Files not owned by nobody:users, or without read and write for its group, can't always be reached over SMB. Tools → Docker Safe New Perms on the server fixes them (it leaves appdata alone)."),
      u.permissions.map((p) => h("details", {}, h("summary", {}, `${p.share}: ${count(p.count, "item", "items")}`),
        h("div", { class: "paths" }, p.examples.map((e) => h("div", {}, e)))))));
  }
  if (u.moves.length) {
    sections.push(h("div", { class: "card table-wrap" }, h("h2", {}, "Moves between disks"),
      h("table", {}, h("tbody", {}, u.moves.slice().reverse().map((m) => h("tr", {},
        h("td", {}, m.folder ? `${m.share}/${m.folder}` : m.share, h("div", { class: "sub" }, `${m.from} → ${m.to}`)),
        h("td", {}, h("span", { class: `badge ${m.state === "done" ? "ok" : m.state === "failed" ? "danger" : "accent"}` }, m.state)),
        h("td", { class: "num" }, `${size(m.done_bytes)} of ${size(m.bytes)}`),
        h("td", { class: "small dim" }, m.detail))))),
      h("p", { class: "hint" }, "Moves are started on the server, in DiskTree's page under Tools.")));
  }
  return h("div", { class: "stack" }, sections);
}

function deviceQuarantine(device) {
  const batches = device.quarantine || [];
  const total = batches.reduce((sum, b) => sum + b.bytes, 0);
  const head = h("div", { class: "card" },
    h("div", { class: "row" },
      h("div", { class: "grow" }, h("h2", { style: "margin:0" }, "Quarantine"),
        h("p", { class: "dim small", style: "margin:4px 0 0" }, batches.length
          ? `${count(batches.length, "batch", "batches")}, ${size(total)}. Their space is freed only when the quarantine is emptied.`
          : "Empty. Deleting on this device moves things here first.")),
      batches.length && canDo(device, "delete") ? h("button", { onclick: () => requestDialog(device,
        { type: "empty_quarantine", batches: batches.map((b) => b.id), bytes: total },
        [`Empty the whole quarantine for good (${size(total)}):`, ...batches.flatMap((b) => b.items.slice(0, 20).map((i) => i.original))]) },
        "Empty all…") : null),
    h("p", { class: "hint" }, "Nothing here is deleted until someone asks to empty it and the device's approver agrees. On Unraid each disk keeps its own; batches on a sleeping disk show again when it wakes."));
  const cards = batches.map((b) => h("div", { class: "card" },
    h("div", { class: "row" },
      h("div", { class: "grow" }, h("strong", {}, `${count(b.items.length, "item", "items")}, ${size(b.bytes)}`),
        h("div", { class: "dim small" }, `${date(b.when)} · by ${b.by || "the device"}${b.reason ? ` · ${b.reason}` : ""}`)),
      canDo(device, "restore") ? h("button", { class: "small", onclick: () => requestDialog(device,
        { type: "restore", batch: b.id, label: "" }, ["Put back where they were:", ...b.items.map((i) => i.original)]) }, "Put back…") : null,
      canDo(device, "delete") ? h("button", { class: "small", onclick: () => requestDialog(device,
        { type: "empty_quarantine", batches: [b.id], bytes: b.bytes },
        [`Delete for good (${size(b.bytes)}):`, ...b.items.map((i) => i.original)]) }, "Empty…") : null),
    h("div", { class: "paths", style: "margin-top:8px" }, b.items.slice(0, 10).map((i) => h("div", {}, i.original, h("span", { class: "dim" }, ` · ${size(i.bytes)}`))),
      b.items.length > 10 ? h("div", { class: "dim" }, `… and ${b.items.length - 10} more`) : null)));
  return h("div", { class: "stack" }, head, cards);
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
        h("td", { class: "num" }, size(j.bytes)),
        canDo(device, "clean") ? h("td", { class: "num" }, h("button", { class: "small", onclick: () =>
          requestDialog(device, { type: "clean_junk", kind: j.kind, label: j.label, bytes: j.bytes },
            [`Clean up ${j.label}: ${count(j.items, "item", "items")}, ${size(j.bytes)}`]) }, "Clean…")) : null))))));
  }
  const apps = [...report.apps].sort((a, b2) => (b2.code + b2.data) - (a.code + a.data));
  parts.push(h("div", { class: "card table-wrap" },
    h("h2", {}, `Apps (${apps.length})`),
    h("table", {},
      h("thead", {}, h("tr", {}, h("th", {}, "App"), h("th", { class: "num" }, "Size"), h("th", { class: "num hide-narrow" }, "Cache"), h("th", { class: "hide-narrow" }, "Last used"), h("th"))),
      h("tbody", {}, apps.slice(0, 300).map((a) => h("tr", {},
        h("td", {}, a.label, h("div", { class: "sub" }, `${a.package}${a.source ? ` · ${a.source}` : ""}`)),
        h("td", { class: "num" }, size(a.code + a.data)),
        h("td", { class: "num hide-narrow" }, size(a.cache)),
        h("td", { class: "hide-narrow small" }, a.last_used ? ago(a.last_used) : "–"),
        h("td", { class: "num" }, canDo(device, "uninstall") && a.removable ? h("button", { class: "link", onclick: () =>
          requestDialog(device, { type: "uninstall", package: a.package, label: a.label },
            [`Uninstall ${a.label} (${a.package}), ${size(a.code + a.data)}.`, "Android on the phone asks to confirm as well."]) }, "Uninstall…") : null)))))));
  return h("div", { class: "stack" }, parts);
}

async function deviceRequests(device) {
  const rows = await api(`/api/commands?device=${encodeURIComponent(device.id)}`);
  const body = h("div", { class: "card" }, requestTable(rows, false));
  pageTimer = setInterval(async () => {
    const fresh = await api(`/api/commands?device=${encodeURIComponent(device.id)}`).catch(() => null);
    if (fresh) body.replaceChildren(requestTable(fresh, false));
  }, 5000);
  return body;
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

// ----- Alerts -----------------------------------------------------------------

const SEVERITY = { critical: "danger", warning: "warn", info: "accent" };
const CHANNEL_KINDS = {
  ntfy: { label: "ntfy", fields: [["server", "Server", "https://ntfy.sh"], ["topic", "Topic", "e.g. disktree-a8f3"], ["token", "Access token (for a protected topic)", "", true]] },
  gotify: { label: "Gotify", fields: [["server", "Server", "https://gotify.example.lan"], ["token", "Application token", "", true]] },
  pushover: { label: "Pushover", fields: [["user", "User key", ""], ["token", "Application API token", "", true]] },
  webhook: { label: "Webhook", fields: [["url", "Address", "https://homeassistant.lan/api/webhook/disktree"], ["secret", "Signing secret (optional)", "", true]] },
};

function alertRow(a) {
  const link = a.device_id ? `#/device/${encodeURIComponent(a.device_id)}` : null;
  return h("div", { class: "alert-row" },
    h("span", { class: `badge ${a.resolved ? "" : SEVERITY[a.severity] || ""}` }, a.resolved ? "over" : a.severity),
    h("div", { class: "grow" },
      link ? h("a", { href: link }, a.title) : h("strong", {}, a.title),
      h("div", { class: "sub" }, a.message)),
    h("span", { class: "dim small" }, a.resolved ? `over ${ago(a.resolved)}` : `since ${ago(a.opened)}`));
}

function channelDialog(channel) {
  dialog((close) => {
    let kind = channel ? channel.target.kind : "ntfy";
    const name = h("input", { type: "text", id: "ch-name", maxlength: 60, value: channel ? channel.name : "", placeholder: "e.g. My phone" });
    const kindSelect = h("select", { id: "ch-kind", disabled: !!channel, onchange: (e) => { kind = e.target.value; drawFields(); } },
      Object.entries(CHANNEL_KINDS).map(([value, k]) => h("option", { value, selected: value === kind }, k.label)));
    const fields = h("div");
    const inputs = {};
    const drawFields = () => {
      fields.replaceChildren();
      for (const [key, label, placeholder, secret] of CHANNEL_KINDS[kind].fields) {
        const value = channel && channel.target.kind === kind ? channel.target[key] || "" : key === "server" && kind === "ntfy" ? "https://ntfy.sh" : "";
        inputs[key] = h("input", {
          type: secret ? "password" : "text", id: `ch-${key}`, value, autocomplete: "off",
          placeholder: secret && channel && channel.has_secret ? "Stored; leave empty to keep it" : placeholder,
        });
        fields.append(h("label", { for: `ch-${key}` }, label), inputs[key]);
      }
      if (kind === "ntfy") fields.append(h("p", { class: "hint" }, "Anyone who knows a topic on ntfy.sh can read it: pick a long, random one, or use your own server with an access token."));
      if (kind === "webhook") fields.append(h("p", { class: "hint" }, "Gets a JSON POST. With a secret, an X-DiskTree-Signature header carries sha256= and the HMAC-SHA256 of the body."));
    };
    drawFields();
    const save = async () => {
      const target = { kind };
      for (const [key] of CHANNEL_KINDS[kind].fields) target[key] = inputs[key].value.trim();
      try {
        if (channel) await api(`/api/alerts/channels/${channel.id}`, { method: "PATCH", body: { name: name.value, target } });
        else await api("/api/alerts/channels", { method: "POST", body: { name: name.value, target } });
        close();
        render();
      } catch (e) { toast(e.message, true); }
    };
    return [h("h2", {}, channel ? `Edit ${channel.name}` : "Send alerts to…"),
      h("label", { for: "ch-name" }, "Name"), name,
      h("label", { for: "ch-kind" }, "Service"), kindSelect,
      fields,
      h("div", { class: "actions" }, h("button", { onclick: close }, "Cancel"), h("button", { class: "primary", onclick: save }, "Save"))];
  });
}

function channelTarget(t) {
  if (t.kind === "ntfy") return `${t.server.replace(/\/$/, "")}/${t.topic}`;
  if (t.kind === "gotify") return t.server;
  if (t.kind === "pushover") return `user ${t.user}`;
  return t.url;
}

function rulesCard(rules) {
  const number = (key, min, max) => h("input", { type: "number", min, max, value: rules[key], class: "inline-number", "aria-label": key });
  const fields = {
    disk_full_percent: number("disk_full_percent", 0, 99),
    growth_week_percent: number("growth_week_percent", 0, 100),
    forecast_days: number("forecast_days", 0, 365),
    offline_hours: number("offline_hours", 0, 720),
  };
  const requests = h("input", { type: "checkbox", id: "rule-requests", checked: rules.requests });
  const editable = state.me.admin;
  for (const input of [...Object.values(fields), requests]) input.disabled = !editable;
  const save = async () => {
    const body = { requests: requests.checked };
    for (const [key, input] of Object.entries(fields)) body[key] = Number(input.value) || 0;
    try {
      await api("/api/alerts/rules", { method: "PUT", body });
      toast("Saved. Alerts are checked every minute.");
    } catch (e) { toast(e.message, true); }
  };
  return h("div", { class: "card rules" },
    h("h2", {}, "When to alert"),
    h("p", {}, "A disk is at least ", fields.disk_full_percent, " % full."),
    h("p", {}, "A disk grew by ", fields.growth_week_percent, " % of its size within a week."),
    h("p", {}, "A disk will be full within ", fields.forecast_days, " days at the past month's rate."),
    h("p", {}, "A server (a device nobody is at) hasn't reported for ", fields.offline_hours, " hours."),
    h("p", {}, h("label", { class: "check", for: "rule-requests" }, requests,
      " A request has waited 2 minutes for its device or approver. The DiskTree app on a phone only hears from the Hub while it's open; this tells you to open it.")),
    h("p", { class: "hint" }, "0 turns a rule off."),
    editable ? h("button", { class: "primary", onclick: save }, "Save") : null);
}

PAGES.alerts = async () => {
  const [alerts, rules, channels] = await Promise.all([
    api("/api/alerts"), api("/api/alerts/rules"),
    state.me.admin ? api("/api/alerts/channels") : Promise.resolve(null),
  ]);
  const active = alerts.active.length
    ? h("div", { class: "card" }, h("h2", {}, "Now"), alerts.active.map(alertRow))
    : h("div", { class: "card empty" }, h("h2", {}, "All quiet"), h("p", {}, "No disk is near full, growing fast or about to fill, and every server is reporting."));
  const recent = alerts.recent.length
    ? h("div", { class: "card" }, h("h2", {}, "Over in the past 30 days"), alerts.recent.slice(0, 30).map(alertRow))
    : null;
  const channelCard = channels ? h("div", { class: "card table-wrap" },
    h("div", { class: "row" }, h("h2", { class: "grow", style: "margin:0" }, "Where alerts go"),
      h("button", { onclick: () => channelDialog() }, "Add")),
    channels.length ? h("table", { style: "margin-top:8px" }, h("tbody", {}, channels.map((c) => h("tr", {},
      h("td", {}, c.name, h("div", { class: "sub" }, `${CHANNEL_KINDS[c.target.kind].label} · ${channelTarget(c.target)}`)),
      h("td", { class: "small hide-narrow" }, !c.enabled ? h("span", { class: "dim" }, "off")
        : c.last_error ? h("span", { class: "badge danger", title: c.last_error }, "failed")
        : c.last_sent ? h("span", { class: "dim" }, `sent ${ago(c.last_sent)}`) : h("span", { class: "dim" }, "nothing sent yet")),
      h("td", { class: "num" },
        h("button", { class: "link", onclick: async () => {
          try { await api(`/api/alerts/channels/${c.id}/test`, { method: "POST" }); toast(`Sent a test to ${c.name}.`); } catch (e) { toast(e.message, true); }
          render();
        } }, "Test"),
        h("button", { class: "link", onclick: () => channelDialog(c) }, "Edit"),
        h("button", { class: "link", onclick: async () => {
          try { await api(`/api/alerts/channels/${c.id}`, { method: "PATCH", body: { enabled: !c.enabled } }); render(); } catch (e) { toast(e.message, true); }
        } }, c.enabled ? "Turn off" : "Turn on"),
        h("button", { class: "link", onclick: async () => {
          try { await api(`/api/alerts/channels/${c.id}`, { method: "DELETE" }); render(); } catch (e) { toast(e.message, true); }
        } }, "Remove")))))) : h("p", { class: "dim", style: "margin-top:8px" }, "Nowhere yet: alerts only show here. Add ntfy, Gotify, Pushover or a webhook to get them on your phone."),
    h("p", { class: "hint" }, "Tokens and secrets stay on the Hub; they're never shown again.")) : null;
  return [
    h("div", { class: "page-head" }, h("div", { class: "grow" }, h("h1", {}, "Alerts"),
      h("p", { class: "dim" }, "Checked every minute. You hear when something starts and when it's over."))),
    h("div", { class: "stack" }, active, recent,
      rulesCard(rules),
      channelCard,
      h("div", { class: "card" }, h("h2", {}, "Prometheus and Home Assistant"),
        h("p", {}, "With DISKTREE_METRICS_TOKEN set, ", h("code", {}, "/metrics"), " serves Prometheus metrics and ",
          h("code", {}, "/metrics/json"), " the same as JSON for a Home Assistant REST sensor. Both want the token as ",
          h("code", {}, "Authorization: Bearer <token>"), ".")))];
};

// ----- Search -----------------------------------------------------------------

const MIN_SIZES = [[0, "Any size"], [100 * 1024 ** 2, "100 MB or more"], [1024 ** 3, "1 GB or more"], [10 * 1024 ** 3, "10 GB or more"]];
const HIT_ICONS = { folder: "📁", file: "📄", app: "📱" };

function hitLink(hit) {
  const device = `#/device/${encodeURIComponent(hit.device_id)}`;
  if (hit.kind === "app") return `${device}/apps`;
  const folder = hit.kind === "folder" ? hit.path : hit.path.includes("/") ? hit.path.slice(0, hit.path.lastIndexOf("/")) : "";
  return `${device}/folders/${hit.scan_id}${folder ? `/${encodeURIComponent(folder)}` : ""}`;
}

function saveSearchDialog(query, min) {
  dialog((close) => {
    const name = h("input", { type: "text", id: "save-name", maxlength: 60, value: query });
    const save = async () => {
      try {
        await api("/api/searches", { method: "POST", body: { name: name.value, query, min: Number(min) || 0 } });
        close();
        render();
      } catch (e) { toast(e.message, true); }
    };
    name.addEventListener("keydown", (e) => { if (e.key === "Enter") save(); });
    return [h("h2", {}, "Save this search"),
      h("p", { class: "dim" }, "Saved searches are yours alone; others who sign in don't see them."),
      h("label", { for: "save-name" }, "Name"), name,
      h("div", { class: "actions" }, h("button", { onclick: close }, "Cancel"), h("button", { class: "primary", onclick: save }, "Save"))];
  });
}

PAGES.search = async (queryArg = "", minArg = "0", deviceArg = "") => {
  const [saved, devices] = await Promise.all([api("/api/searches"), api("/api/devices")]);
  const input = h("input", { type: "search", id: "search-q", value: queryArg, placeholder: "e.g. ubuntu *.iso, or plex", autocomplete: "off", "aria-label": "Search for" });
  const min = h("select", { class: "auto", "aria-label": "Smallest size" },
    MIN_SIZES.map(([value, label]) => h("option", { value, selected: String(value) === minArg }, label)));
  const device = h("select", { class: "auto", "aria-label": "Device" }, h("option", { value: "" }, "All devices"),
    devices.map((d) => h("option", { value: d.id, selected: d.id === deviceArg }, d.name)));
  const go = () => {
    const q = input.value.trim();
    if (q) location.hash = `#/search/${encodeURIComponent(q)}/${min.value}/${encodeURIComponent(device.value)}`;
  };
  input.addEventListener("keydown", (e) => { if (e.key === "Enter") go(); });
  setTimeout(() => input.focus(), 0);
  const head = h("div", { class: "page-head" },
    h("div", { class: "grow" }, h("h1", {}, "Search"),
      h("p", { class: "dim" }, "Find files, folders and apps on every device at once.")));
  const form = h("div", { class: "card" },
    h("div", { class: "row" }, h("div", { class: "grow", style: "min-width:220px" }, input), min, device,
      h("button", { class: "primary", onclick: go }, "Search")),
    h("p", { class: "hint", style: "margin-bottom:0" }, "Every word has to appear in the path; *.iso or .iso finds a file type. It looks through the newest scan of each folder, which keeps the biggest folders and files, not every file."),
    saved.length ? h("div", { class: "row chips", style: "margin-top:12px" }, h("span", { class: "dim small" }, "Saved:"),
      saved.map((s) => h("span", { class: "chip" },
        h("a", { href: `#/search/${encodeURIComponent(s.query)}/${s.min}/` }, s.name),
        h("button", {
          class: "link", title: `Remove ${s.name}`, "aria-label": `Remove ${s.name}`,
          onclick: async () => {
            try { await api(`/api/searches/${s.id}`, { method: "DELETE" }); render(); } catch (e) { toast(e.message, true); }
          },
        }, "×")))) : null);
  if (!queryArg) return [head, form];
  const result = await api(`/api/search?q=${encodeURIComponent(queryArg)}&min=${Number(minArg) || 0}${deviceArg ? `&device=${encodeURIComponent(deviceArg)}` : ""}`);
  const found = result.hits.length
    ? h("div", { class: "card table-wrap" },
        h("div", { class: "row", style: "margin-bottom:6px" },
          h("h2", { class: "grow", style: "margin:0" }, `${result.more ? "The biggest " : ""}${count(result.hits.length, "match", "matches")}`),
          saved.some((s) => s.query === queryArg) ? null : h("button", { class: "small", onclick: () => saveSearchDialog(queryArg, minArg) }, "Save this search")),
        h("table", {},
          h("thead", {}, h("tr", {}, h("th", {}, "Found"), h("th", { class: "hide-narrow" }, "Device"), h("th", { class: "num" }, "Size"))),
          h("tbody", {}, result.hits.map((hit) => {
            const name = hit.kind === "app" ? hit.path : hit.path.slice(hit.path.lastIndexOf("/") + 1);
            const where = hit.kind === "app" ? "App" : [hit.label || hit.root, ...hit.path.split("/").slice(0, -1)].join(" / ");
            return h("tr", { class: "clickable", onclick: () => { location.hash = hitLink(hit); } },
              h("td", {}, `${HIT_ICONS[hit.kind]} `, name, h("div", { class: "sub" }, where, h("span", { class: "show-narrow" }, ` · ${hit.device_name}`))),
              h("td", { class: "hide-narrow" }, hit.device_name),
              h("td", { class: "num" }, size(hit.allocated)));
          }))))
    : h("div", { class: "card empty" }, h("h2", {}, "Nothing found"),
        h("p", {}, `Nothing on ${count(result.devices, "device", "devices")} matches. Only the biggest folders and files of each scan are kept, so small files can't be found.`));
  return [head, form, found];
};

// ----- Requests ---------------------------------------------------------------

const REQUEST_STATES = {
  queued: ["waiting to be picked up", ""],
  delivered: ["sent to the device", "accent"],
  shown: ["waiting on the device", "warn"],
  needs_approver: ["waiting for the approver", "warn"],
  running: ["running", "accent"],
  done: ["done", "ok"],
  failed: ["failed", "danger"],
  denied: ["denied", "danger"],
  expired: ["expired", ""],
  cancelled: ["withdrawn", ""],
};
const OPEN_STATES = ["queued", "delivered", "shown", "needs_approver"];

function stateBadge(s) {
  const [label, level] = REQUEST_STATES[s] || [s, ""];
  return h("span", { class: `badge ${level}` }, label);
}

function approvalText(a) {
  if (!a || !a.by) return "";
  const by = a.by.type === "device" ? "on the device"
    : a.by.type === "approver" ? "by the approver"
    : "under a grant";
  return `approved ${by}${a.minutes ? `, grant for ${a.minutes} min` : ""}`;
}

function requestTable(rows, showDevice) {
  if (!rows.length) return h("p", { class: "dim" }, "No requests yet.");
  return h("div", { class: "table-wrap" }, h("table", {},
    h("thead", {}, h("tr", {}, h("th", {}, "Asked"), showDevice ? h("th", {}, "Device") : null, h("th", {}, "What"),
      h("th", { class: "hide-narrow" }, "By"), h("th", {}, "State"), h("th", {}))),
    h("tbody", {}, rows.map((r) => h("tr", {},
      h("td", { class: "small" }, ago(r.created)),
      showDevice ? h("td", {}, h("a", { href: `#/device/${encodeURIComponent(r.device_id)}/requests` }, r.device_name)) : null,
      h("td", {}, r.summary,
        r.detail ? h("div", { class: "sub" }, r.detail) : null,
        r.approval ? h("div", { class: "sub" }, approvalText(r.approval)) : null,
        r.failed && r.failed.length ? h("div", { class: "sub" }, `Couldn't: ${r.failed.slice(0, 3).join("; ")}`) : null),
      h("td", { class: "hide-narrow" }, r.admin_name),
      h("td", {}, stateBadge(r.state)),
      h("td", { class: "num" }, state.me.admin && OPEN_STATES.includes(r.state)
        ? h("button", { class: "link", onclick: () => withdraw(r) }, "Withdraw") : null))))));
}

async function withdraw(r) {
  try {
    await api(`/api/commands/${encodeURIComponent(r.id)}/cancel`, { method: "POST" });
    toast("Withdrawn");
    render();
  } catch (e) { toast(e.message, true); }
}

PAGES.requests = async () => {
  const rows = await api("/api/commands");
  const open = rows.filter((r) => OPEN_STATES.includes(r.state));
  const body = h("div", { class: "card" }, requestTable(rows, true));
  // Watch the states change while the page is open.
  pageTimer = setInterval(async () => {
    const fresh = await api("/api/commands").catch(() => null);
    if (fresh) body.replaceChildren(requestTable(fresh, true));
  }, 5000);
  return [
    h("div", { class: "page-head" }, h("div", { class: "grow" }, h("h1", {}, "Requests"),
      h("p", { class: "dim" }, open.length
        ? `${count(open.length, "request is", "requests are")} waiting. Each device asks its owner, or its approver, before doing anything.`
        : "What admins asked devices to do, and what came of it. Each device asks its owner, or its approver, before doing anything."))),
    body,
  ];
};

/** Asks a device to do something, after showing exactly what. */
function requestDialog(device, action, lines) {
  dialog((close) => {
    const reason = h("textarea", { maxlength: 500, placeholder: "Optional: shown on the device with the request" });
    const who = device.info && device.info.attended
      ? `${device.name} shows the request to whoever is at it.`
      : `Nobody is at ${device.name}, so its approver's phone gets the request.`;
    const send = async () => {
      try {
        await api(`/api/devices/${encodeURIComponent(device.id)}/commands`, { method: "POST", body: { action, reason: reason.value } });
        close();
        toast("Sent. The device will ask before doing it.");
        location.hash = `#/device/${encodeURIComponent(device.id)}/requests`;
      } catch (e) { toast(e.message, true); }
    };
    return [
      h("h2", {}, "Ask the device"),
      h("div", { class: "paths" }, lines.map((l) => h("div", {}, l))),
      h("p", { class: "callout", style: "margin-top:12px" }, who,
        " Nothing happens until someone approves it there. The request expires in 24 hours."),
      h("label", {}, "Reason"), reason,
      h("div", { class: "actions" }, h("button", { onclick: close }, "Cancel"), h("button", { class: "primary", onclick: send }, "Send request")),
    ];
  });
}

function canDo(device, kind) {
  const info = device.info || {};
  // A device nobody is at, without an approver, can't be asked anything.
  return state.me.admin && (info.actions || []).includes(kind) && (info.attended || Boolean(info.approver));
}

/** A bar under a list for asking to delete the checked items. */
function deleteBar(device, root, label) {
  const chosen = new Map();
  const summary = h("span", { class: "grow small" });
  const button = h("button", { class: "danger small", disabled: true }, "Ask to delete…");
  const bar = h("div", { class: "row callout", style: "margin-top:12px;display:none" }, summary, button);
  const refresh = () => {
    const total = [...chosen.values()].reduce((a, b) => a + b, 0);
    summary.textContent = `${count(chosen.size, "item", "items")} chosen, ${size(total)}`;
    button.disabled = !chosen.size;
    bar.style.display = chosen.size ? "flex" : "none";
  };
  button.addEventListener("click", () => {
    const paths = [...chosen.keys()];
    const total = [...chosen.values()].reduce((a, b) => a + b, 0);
    requestDialog(device, { type: "delete", root, paths, bytes: total },
      [`Delete ${count(paths.length, "item", "items")} (${size(total)}) in ${label}:`, ...paths.map((p) => `${root}/${p}`)]);
  });
  const box = (path, bytes) => h("input", {
    type: "checkbox", "aria-label": `Choose ${path}`,
    onclick: (e) => e.stopPropagation(),
    onchange: (e) => { if (e.target.checked) chosen.set(path, bytes); else chosen.delete(path); refresh(); },
  });
  return { bar, box };
}

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
/** A page's refresh timer, stopped when another page shows. */
let pageTimer = null;

async function render() {
  const id = ++renderId;
  clearInterval(pageTimer);
  pageTimer = null;
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
