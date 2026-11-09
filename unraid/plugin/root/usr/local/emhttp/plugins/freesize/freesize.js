// FreeSize for Unraid: the page under Tools → FreeSize. It shows what the
// background part found (status.php) and asks for changes (action.php).
// Every change that removes or moves something first lists exactly what,
// with links to look at it in Unraid's file browser.
(function () {
  "use strict";
  if (window.FreeSizePage) return;
  window.FreeSizePage = true;

  const API = "/plugins/freesize/include/";
  const model = { status: null, running: false, config: null, now: 0, loaded: false };
  // What the person picked, kept across refreshes.
  const picked = { docker: new Set() };
  const built = {};

  // ----- Helpers ------------------------------------------------------

  function h(tag, attrs, ...children) {
    const el = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) {
      if (v === null || v === undefined || v === false) continue;
      if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
      else if (k === "class") el.className = v;
      else if (k === "value") el.value = v;
      else if (k === "checked" || k === "disabled" || k === "selected") el[k] = true;
      else el.setAttribute(k, v === true ? "" : v);
    }
    for (const child of children.flat(Infinity)) {
      if (child === null || child === undefined || child === false) continue;
      el.append(child instanceof Node ? child : document.createTextNode(String(child)));
    }
    return el;
  }

  function fill(el, ...children) {
    if (!el) return;
    el.replaceChildren(...children.flat(Infinity).filter((c) => c !== null && c !== undefined && c !== false));
  }

  function size(bytes) {
    const units = ["B", "KB", "MB", "GB", "TB", "PB"];
    let v = Number(bytes) || 0;
    let i = 0;
    while (v >= 1024 && i < units.length - 1) {
      v /= 1024;
      i++;
    }
    if (i === 0) return `${v} B`;
    return `${v < 10 ? v.toFixed(2) : v < 100 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
  }

  function count(n, one, many) {
    return `${n} ${n === 1 ? one : many || one + "s"}`;
  }

  function ago(t) {
    if (!t) return "never";
    const s = Math.max(0, (model.now || Date.now() / 1000) - t);
    if (s < 90) return "just now";
    if (s < 5400) return `${Math.round(s / 60)} min ago`;
    if (s < 172800) return `${Math.round(s / 3600)} h ago`;
    return `${Math.round(s / 86400)} days ago`;
  }

  function until(t) {
    const s = t - (model.now || Date.now() / 1000);
    if (s < 90) return "in a minute";
    if (s < 5400) return `in ${Math.round(s / 60)} min`;
    return `in ${Math.round(s / 3600)} h`;
  }

  function date(t) {
    return new Date(t * 1000).toLocaleString();
  }

  // Unraid's file browser, at a folder (for a file: the folder it's in).
  function browse(path, isFile) {
    let dir = path;
    if (isFile) dir = path.replace(/\/[^/]*$/, "") || "/";
    return h("a", { href: `/Main/Browse?dir=${encodeURIComponent(dir)}`, target: "_blank", class: "dt-browse", title: `Look at ${dir} in the file browser` }, "View");
  }

  function badge(text, kind) {
    return h("span", { class: `dt-badge dt-${kind || "plain"}` }, text);
  }

  async function post(action, data) {
    const body = new URLSearchParams();
    body.append("csrf_token", window.csrf_token || "");
    body.append("action", action);
    for (const [k, v] of Object.entries(data || {})) {
      if (Array.isArray(v)) v.forEach((x) => body.append(`${k}[]`, x));
      else body.append(k, v);
    }
    try {
      const res = await fetch(API + "action.php", { method: "POST", body, credentials: "same-origin" });
      const reply = await res.json().catch(() => ({ ok: false, output: `the server said ${res.status}` }));
      return reply;
    } catch (err) {
      return { ok: false, output: String(err) };
    }
  }

  let toastTimer = null;
  function toast(text, kind) {
    let el = document.getElementById("dt-toast");
    if (!el) {
      el = h("div", { id: "dt-toast", role: "status" });
      document.body.append(el);
    }
    el.className = `dt-toast dt-${kind || "info"}`;
    text = String(text || "");
    el.textContent = text.charAt(0).toUpperCase() + text.slice(1);
    el.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (el.hidden = true), kind === "bad" ? 12000 : 5000);
  }

  // A dialog; `buttons` are {label, kind, onclick(close, button)}.
  function dialog(title, body, buttons) {
    const close = () => overlay.remove();
    const row = h("div", { class: "dt-dialog-buttons" },
      h("button", { type: "button", onclick: close }, buttons.length ? "Cancel" : "Close"),
      buttons.map((b) => {
        const button = h("button", { type: "button", class: b.kind ? `dt-${b.kind}` : null, disabled: b.disabled }, b.label);
        button.addEventListener("click", () => b.onclick(close, button));
        return button;
      }));
    const box = h("div", { class: "dt-dialog", role: "dialog", "aria-modal": "true" }, h("h3", {}, title), body, row);
    const overlay = h("div", { class: "dt-overlay", onclick: (e) => e.target === overlay && close() }, box);
    document.body.append(overlay);
    return { close, box, buttons: row.querySelectorAll("button") };
  }

  async function busy(button, work) {
    const label = button.textContent;
    button.disabled = true;
    button.textContent = "Working…";
    try {
      return await work();
    } finally {
      button.disabled = false;
      button.textContent = label;
    }
  }

  // ----- Loading ------------------------------------------------------

  async function load() {
    try {
      const res = await fetch(API + "status.php", { credentials: "same-origin", cache: "no-store" });
      const data = await res.json();
      model.status = data.status;
      model.running = data.running;
      model.config = data.config;
      model.now = data.now;
      model.loaded = true;
    } catch (err) {
      model.loaded = true;
    }
    render();
  }

  function refreshSoon() {
    setTimeout(load, 1500);
  }

  function render() {
    renderOverview();
    renderShares();
    renderMap();
    renderApps();
    renderActivity();
    renderQuarantine();
    renderSettings();
  }

  function report() {
    return (model.status && model.status.report) || {};
  }

  function notRunning() {
    if (model.running) return null;
    return h("p", { class: "dt-note dt-bad" },
      model.status
        ? "FreeSize's background part has stopped; what's shown is from when it last ran. Start it again with: /etc/rc.d/rc.freesize start"
        : "FreeSize's background part isn't running yet. Start it with: /etc/rc.d/rc.freesize start");
  }

  // How far the background part's look has got, while one runs or before
  // the first has finished: what the page shows until then is incomplete.
  function lookNote() {
    const s = model.status;
    if (!s || !model.running) return null;
    const doing = s.doing && s.doing !== "idle" && s.doing !== "starting" ? s.doing : null;
    if (!s.last_round) {
      return h("div", { class: "dt-note dt-busy", role: "status" }, doing ? h("span", { class: "dt-spinner" }) : null,
        h("div", {},
          h("b", {}, doing ? `Looking at the server: ${doing}` : `The first look starts ${s.next_round ? until(s.next_round) : "in a minute"}`),
          h("div", { class: "dt-muted" }, "The disks first, then Docker, the VMs and permissions. Everything shows here when it's done, without reloading; a big array takes a few minutes.")));
    }
    if (!doing) return null;
    return h("div", { class: "dt-note dt-busy", role: "status" }, h("span", { class: "dt-spinner" }),
      h("div", {},
        h("b", {}, `Looking again: ${doing}`),
        h("div", { class: "dt-muted" }, `Until it's done, this shows the last look, ${ago(s.last_round)}.`)));
  }

  // Why a part of the report is missing: not looked at yet, or the look
  // was skipped; null once a look has run, when missing means it's off.
  function notLooked() {
    const s = model.status;
    if (!s || !s.last_round) return "Not looked at yet: that comes after the disks are scanned.";
    if (s.skipped) return `Not looked at: ${s.skipped}.`;
    return null;
  }

  // ----- Overview -----------------------------------------------------

  const SEGMENTS = ["#4f7ddb", "#3aa57a", "#d08a2e", "#9a5bd1", "#d0544f", "#2fa7b8", "#8a8f3c", "#c25b9a"];
  function diskColors() {
    const colors = {};
    let i = 0;
    for (const d of report().disks || []) {
      if (d.kind === "data" || d.kind === "pool") colors[d.name] = SEGMENTS[i++ % SEGMENTS.length];
    }
    return colors;
  }

  function renderOverview() {
    const root = document.getElementById("dt-overview");
    if (!root || !model.loaded) return;
    const s = model.status;
    if (!s) {
      fill(root, notRunning());
      return;
    }
    const r = report();
    const colors = diskColors();
    const scan = h("button", { type: "button", onclick: async (e) => {
      const reply = await busy(e.target, () => post("scan"));
      toast(reply.ok ? "Looking now; this page updates when it's done." : reply.output, reply.ok ? "good" : "bad");
      refreshSoon();
    } }, "Look now");
    const docker = r.docker;
    const safe = docker ? docker.findings.filter((f) => f.safety === "safe") : [];
    const quarantined = (s.quarantine || []).reduce((n, b) => n + b.bytes, 0);
    const look = lookNote();
    fill(root,
      notRunning(),
      look,
      h("div", { class: "dt-head" },
        h("div", {},
          h("p", {}, h("b", {}, `FreeSize ${s.version}`), ` on Unraid ${r.version || "?"}`, look ? "" : ` · now ${s.doing || "idle"}`),
          h("p", { class: "dt-muted" },
            `Last looked ${ago(s.last_round)}`,
            s.next_round && model.running ? ` · next ${until(s.next_round)}` : "",
            ". Sleeping disks aren't woken; their last scan stands.")),
        scan),
      s.skipped ? h("p", { class: "dt-note" }, `Not scanning: ${s.skipped}.`) : null,
      (s.errors || []).length ? h("div", { class: "dt-note dt-bad" }, h("b", {}, "Problems in the last look:"), h("ul", {}, s.errors.map((e) => h("li", {}, e)))) : null,
      h("table", { class: "dt-table" },
        h("thead", {}, h("tr", {}, ["Disk", "Used", "Free", "Size", "Temp", "Scanned"].map((t, i) => h("th", { class: i > 1 ? "dt-num" : null }, t)))),
        h("tbody", {}, (r.disks || []).map((d) => {
          const used = Math.max(0, d.size - d.free);
          const pct = d.size ? Math.round((used / d.size) * 100) : 0;
          return h("tr", {},
            h("td", {},
              colors[d.name] ? h("span", { class: "dt-swatch", style: `background:${colors[d.name]}` }) : null,
              h("b", {}, d.name), " ",
              d.kind !== "data" ? badge(d.kind) : null, " ",
              d.asleep ? badge("asleep", "check") : null,
              h("div", { class: "dt-muted" }, [d.device, d.fs].filter(Boolean).join(" · "))),
            h("td", {}, d.kind === "parity" ? h("span", { class: "dt-muted" }, size(d.size)) :
              h("div", { class: "dt-bar", title: `${pct}% used` }, h("span", { style: `width:${pct}%` }))),
            h("td", { class: "dt-num" }, d.kind === "parity" ? "–" : size(d.free)),
            h("td", { class: "dt-num" }, size(d.size)),
            h("td", { class: "dt-num" }, d.temp ? `${d.temp} °C` : "–"),
            h("td", { class: "dt-num" }, d.kind === "parity" || d.kind === "flash" ? "" : ago(d.scanned)));
        }))),
      h("div", { class: "dt-facts" },
        fact("Docker", docker
          ? (safe.length ? `${size(safe.reduce((n, f) => n + f.bytes, 0))} safe to remove` : "nothing to remove")
          : notLooked() ? "not looked at yet" : "not enabled", "FreeSizeApps"),
        fact("Quarantine", s.quarantine && s.quarantine.length
          ? `${count(s.quarantine.length, "batch", "batches")}, ${size(quarantined)}` : "empty", "FreeSizeQuarantine"),
        fact("Hub", s.hub && s.hub.paired ? `${s.hub.name}${s.hub.connection ? ` (${s.hub.connection})` : ""}` : "not paired", "FreeSizeSettings")));
  }

  function fact(title, text, tab) {
    return h("div", { class: "dt-fact" }, h("div", { class: "dt-muted" }, title), h("div", {}, text), tabLink(tab));
  }

  function tabLink(tab) {
    // Unraid's tabs are inputs labelled by the page titles; click through.
    const tabs = { FreeSizeApps: 4, FreeSizeQuarantine: 6, FreeSizeSettings: 7 };
    const n = tabs[tab];
    if (!n) return null;
    return h("a", { href: "#", class: "dt-link", onclick: (e) => {
      e.preventDefault();
      const input = document.getElementById(`tab${n}`);
      if (input) input.click();
    } }, "Open");
  }

  // ----- Shares and moves ---------------------------------------------

  function renderShares() {
    const root = document.getElementById("dt-shares");
    if (!root || !model.loaded) return;
    if (!built.shares) {
      built.shares = {
        table: h("div"),
        form: h("div"),
        plan: h("div", { class: "dt-plan" }),
        presets: h("div"),
        moves: h("div"),
        look: h("div"),
        nextcloud: h("div"),
      };
      built.shares.asOf = h("p", { class: "dt-muted" });
      fill(root,
        built.shares.look,
        h("h3", {}, "Shares and their disks"),
        built.shares.asOf,
        built.shares.table,
        built.shares.nextcloud,
        h("h3", {}, "Move between disks"),
        h("p", { class: "dt-muted" }, "Moves a share, or a folder in it, from one disk to another. Each file is copied, the copy checked against the original, and only then the original moved into the source disk's quarantine, where it stays until you empty it. The share's path in /mnt/user stays the same. Sparse files (VM disks) keep their holes."),
        built.shares.form,
        built.shares.plan,
        built.shares.presets,
        built.shares.moves);
    }
    const r = report();
    const colors = diskColors();
    const shares = r.shares || [];
    fill(built.shares.look, notRunning(), lookNote());
    fill(built.shares.asOf, model.status && model.status.last_round ? `As of the last look, ${ago(model.status.last_round)}; disks that slept through it show their earlier scan, paler.` : null);
    fill(built.shares.table,
      shares.length ? h("table", { class: "dt-table" },
        h("tbody", {}, shares.map((s) => {
          const total = s.total || 1;
          return h("tr", {},
            h("td", { class: "dt-name" }, h("b", {}, s.name), h("div", { class: "dt-muted" }, [s.comment, s.cache].filter(Boolean).join(" · "))),
            h("td", { class: "dt-wide" },
              h("div", { class: "dt-stack" }, s.placements.map((p) =>
                h("span", { style: `width:${(p.bytes / total) * 100}%;background:${colors[p.disk] || "#888"};${p.stale ? "opacity:.5" : ""}`, title: `${p.disk}: ${size(p.bytes)}${p.stale ? " (earlier scan)" : ""}` }))),
              h("div", { class: "dt-muted" }, s.placements.map((p) => `${p.disk} ${size(p.bytes)}${p.stale ? " (earlier scan)" : ""}`).join(" · "))),
            h("td", { class: "dt-num" }, size(s.total), !s.complete ? h("div", { class: "dt-muted" }, "partly from earlier scans") : null),
            h("td", { class: "dt-actions" }, browse(`/mnt/user/${s.name}`),
              h("button", { type: "button", onclick: () => pickShare(s.name) }, "Move…")));
        })))
        : h("p", { class: "dt-muted" }, "No shares found yet; they show after the first look."));
    renderNextcloud();
    buildMoveForm();
    renderPresets();
    renderMoves();
  }

  // Nextcloud's data folder: the people's files against previews, old
  // versions, the trash and uploads, which Nextcloud itself clears by its
  // settings. Nothing here is offered for deletion.
  function renderNextcloud() {
    const instances = report().nextcloud || [];
    if (!instances.length) {
      fill(built.shares.nextcloud);
      return;
    }
    fill(built.shares.nextcloud,
      h("h3", {}, "Nextcloud"),
      instances.map((nc) => {
        const extra = nc.previews + nc.appdata + nc.uploads + nc.users.reduce((n, u) => n + u.versions + u.trash + u.cache, 0);
        return h("div", { class: "dt-note" },
          h("p", {}, h("b", {}, nc.data), nc.version ? ` · Nextcloud ${nc.version}` : "", ` · ${size(nc.total)} on disk, ${size(extra)} of it not the people's files`),
          h("table", { class: "dt-table" },
            h("thead", {}, h("tr", {}, h("th", {}, "User"), h("th", { class: "dt-num" }, "Files"), h("th", { class: "dt-num" }, "Old versions"), h("th", { class: "dt-num" }, "Trash"), h("th", { class: "dt-num" }, "Uploads"), h("th", { class: "dt-num" }, "Cache"))),
            h("tbody", {}, nc.users.map((u) => h("tr", {},
              h("td", {}, u.name), h("td", { class: "dt-num" }, size(u.files)), h("td", { class: "dt-num" }, size(u.versions)),
              h("td", { class: "dt-num" }, size(u.trash)), h("td", { class: "dt-num" }, size(u.uploads)), h("td", { class: "dt-num" }, size(u.cache)))),
              h("tr", {}, h("td", {}, "Previews"), h("td", { class: "dt-num", colspan: 5 }, size(nc.previews))),
              h("tr", {}, h("td", {}, "Other app data"), h("td", { class: "dt-num", colspan: 5 }, size(nc.appdata))))),
          h("ul", { class: "dt-list" }, nc.notes.map((n) => h("li", {}, n))));
      }));
  }

  // ----- Presets: empty a disk, gather a share --------------------------

  function renderPresets() {
    if (built.presetsBuilt) return;
    built.presetsBuilt = true;
    const out = h("div", { class: "dt-plan" });
    fill(built.shares.presets,
      h("h3", {}, "Presets"),
      h("p", { class: "dt-muted" }, "A plan of moves worked out for you, each run like a move above: to empty a disk before pulling it, or to gather a share that's spread over several disks onto one."),
      h("div", { class: "dt-form" },
        h("button", { type: "button", onclick: (e) => showPreset(e.target, out, { kind: "empty-disk", disk: built.move.to.value }, "Empty a disk") }, "Empty a disk…"),
        h("button", { type: "button", onclick: (e) => showPreset(e.target, out, { kind: "gather", share: built.move.share.value }, "Gather a share") }, "Gather a share…")),
      out);
  }

  async function showPreset(button, out, args, what) {
    const disks = moveDisks().map((d) => d.name);
    const shares = (report().shares || []).map((s) => s.name);
    const pick = args.kind === "empty-disk"
      ? h("select", {}, disks.map((d) => h("option", { value: d, selected: d === args.disk }, d)))
      : h("select", {}, shares.map((s) => h("option", { value: s, selected: s === args.share }, s)));
    const to = args.kind === "gather" ? h("select", {}, h("option", { value: "" }, "the disk holding most of it"), disks.map((d) => h("option", { value: d }, d))) : null;
    const ui = dialog(what,
      h("div", {},
        h("label", { class: "dt-tick" }, args.kind === "empty-disk" ? "Disk to empty" : "Share to gather", pick),
        to ? h("label", { class: "dt-tick" }, "Onto", to) : null,
        h("p", { class: "dt-muted" }, "Nothing moves yet: the plan is shown first, with what can't be moved and why.")),
      [{ label: "Plan", kind: "primary", onclick: async (close, b) => {
        const query = args.kind === "empty-disk" ? { kind: "empty-disk", disk: pick.value } : { kind: "gather", share: pick.value, to: to.value };
        const reply = await busy(b, () => post("move_preset", query));
        if (!reply.ok) return toast(reply.output, "bad");
        close();
        renderPreset(out, reply.result);
      } }]);
    void ui;
  }

  function renderPreset(out, p) {
    const bytes = p.steps.reduce((n, s) => n + s.bytes, 0);
    const empty = h("input", { type: "checkbox" });
    fill(out,
      h("div", { class: `dt-note ${p.steps.length ? "" : "dt-bad"}` },
        h("p", {}, h("b", {}, p.title), p.steps.length ? `: ${count(p.steps.length, "move")}, ${size(bytes)}, one after another.` : ": nothing can be moved."),
        p.steps.length ? h("ul", { class: "dt-list" }, p.steps.map((s) => h("li", {}, `${s.share}${s.folder ? "/" + s.folder : ""}: ${s.from} → ${s.to}, ${size(s.bytes)}`))) : null,
        p.problems.length ? h("ul", {}, p.problems.map((m) => h("li", {}, m))) : null,
        p.warnings.length ? h("ul", { class: "dt-warn" }, p.warnings.map((m) => h("li", {}, m))) : null,
        p.steps.length ? h("label", { class: "dt-chip" }, empty, "Delete each move's originals once every copy checked out, instead of leaving them in the quarantine") : null,
        p.steps.length ? h("p", {}, h("button", { type: "button", class: "dt-primary", onclick: (e) => startPreset(e.target, out, p, empty.checked) }, `Start ${count(p.steps.length, "move")}…`)) : null));
  }

  async function startPreset(button, out, p, empty) {
    dialog(p.title,
      h("div", {},
        h("p", {}, `Queue ${count(p.steps.length, "move")} (${size(p.steps.reduce((n, s) => n + s.bytes, 0))})?`),
        h("p", {}, empty
          ? "Each move copies its files, checks every copy against its original, and then deletes the originals, as you ticked. Nothing is deleted before its copy checked out."
          : "Each copy is checked against its original. Then the originals go to their disk's quarantine; nothing is deleted. Once you've checked the copies, empty the quarantine yourself."),
        h("p", { class: "dt-muted" }, "The moves run one at a time, each planned again just before it starts; what no longer fits is skipped and says so.")),
      [{ label: "Start", kind: "primary", onclick: async (close, b) => {
        let failed = 0;
        for (const s of p.steps) {
          const reply = await busy(b, () => post("move_start", { share: s.share, folder: s.folder, from: [s.from], to: s.to, empty: empty ? "1" : "0" }));
          if (!reply.ok) { failed++; toast(`${s.share}: ${reply.output}`, "bad"); }
        }
        close();
        fill(out);
        toast(failed ? `${p.steps.length - failed} of ${p.steps.length} moves queued.` : "The moves are queued; they show below.", failed ? "bad" : "good");
        refreshSoon();
      } }]);
  }

  function moveDisks() {
    return (report().disks || []).filter((d) => (d.kind === "data" || d.kind === "pool") && d.path);
  }

  function buildMoveForm() {
    const shares = (report().shares || []).map((s) => s.name);
    const disks = moveDisks().map((d) => d.name);
    const key = JSON.stringify([shares, disks]);
    if (built.moveKey === key) return;
    built.moveKey = key;
    const old = built.move || {};
    const oldFrom = fromValues();
    const select = (name, options, value) => h("select", { name }, options.map((o) => h("option", { value: o, selected: o === value }, o)));
    const f = {
      share: select("share", shares, old.share && old.share.value),
      folder: h("input", { type: "text", name: "folder", placeholder: "all of it, or a folder inside, e.g. Old Films", value: old.folder ? old.folder.value : "" }),
      from: h("div", { class: "dt-chips" }, disks.map((d) =>
        h("label", { class: "dt-chip" }, h("input", { type: "checkbox", value: d, checked: oldFrom.includes(d), onchange: () => pickTarget(false) }), d))),
      to: select("to", disks, old.to && old.to.value),
      note: h("div"),
    };
    built.move = f;
    f.share.addEventListener("change", () => pickShare(f.share.value, false));
    f.folder.addEventListener("change", () => locate());
    f.to.addEventListener("change", () => syncFrom());
    if (!oldFrom.length && f.share.value) pickShare(f.share.value, false);
    pickTarget(!old.to);
    const plan = h("button", { type: "button", onclick: (e) => planMove(e.target) }, "Plan");
    fill(built.shares.form,
      h("div", { class: "dt-form" },
        h("label", {}, "Share", f.share),
        h("div", { class: "dt-field dt-grow" }, h("span", {}, "Folder"),
          h("div", { class: "dt-inline" }, f.folder, h("button", { type: "button", onclick: () => chooseFolder() }, "Choose…")))),
      h("div", { class: "dt-form" },
        h("div", { class: "dt-field dt-grow" }, h("span", {}, "From"), f.from),
        h("label", {}, "To", f.to),
        plan),
      f.note);
  }

  function fromValues() {
    if (!built.move) return [];
    return [...built.move.from.querySelectorAll("input:checked")].map((box) => box.value);
  }

  // Ticks just these disks to move from, then makes sure the disk moved
  // to isn't one of them.
  function setFrom(disks) {
    for (const box of built.move.from.querySelectorAll("input")) {
      box.disabled = false;
      box.checked = disks.includes(box.value);
    }
    pickTarget(false);
  }

  // The disk moved to can't also be moved from.
  function syncFrom() {
    const to = built.move.to.value;
    for (const box of built.move.from.querySelectorAll("input")) {
      box.disabled = box.value === to;
      if (box.disabled) box.checked = false;
    }
  }

  // Moving to a disk it's moved from can't be meant: offer the one with
  // the most room instead.
  function pickTarget(always) {
    const f = built.move;
    const froms = fromValues();
    if (always || !f.to.value || froms.includes(f.to.value)) {
      const roomiest = moveDisks().filter((d) => !froms.includes(d.name)).sort((a, b) => b.free - a.free)[0];
      if (roomiest) f.to.value = roomiest.name;
    }
    syncFrom();
  }

  // A share, all of it: from the disk that holds the most of it.
  function pickShare(name, focus) {
    if (!built.move) return;
    built.move.share.value = name;
    built.move.folder.value = "";
    fill(built.move.note);
    const share = (report().shares || []).find((s) => s.name === name);
    const biggest = share && share.placements.filter((p) => !p.stale).sort((a, b) => b.bytes - a.bytes)[0];
    if (biggest) setFrom([biggest.disk]);
    if (focus !== false) built.move.folder.focus();
  }

  // A folder chosen or typed: move it from the disks it's on. When part of
  // it is on the disk moved to already, the rest is gathered there.
  function useFolder(path, found) {
    const f = built.move;
    f.folder.value = path;
    const to = f.to.value;
    const froms = found.disks.filter((d) => d !== to);
    if (found.disks.length) setFrom(froms.length ? froms : found.disks);
    fill(f.note,
      found.disks.length ? null : h("p", { class: "dt-note dt-bad" }, `${path || "It"} isn't on any disk that's awake.`),
      found.asleep && found.asleep.length
        ? h("p", { class: "dt-muted" }, `Not looked at, asleep: ${found.asleep.join(", ")}. If part of this folder is there too, tick it.`)
        : null);
  }

  function folderOf(input) {
    return input.value.trim().replace(/^\/+|\/+$/g, "");
  }

  // Typed by hand: find which disks it's on.
  async function locate() {
    const f = built.move;
    const folder = folderOf(f.folder);
    if (!folder) return fill(f.note);
    const reply = await post("folders", { share: f.share.value, folder });
    if (folderOf(f.folder) !== folder) return;
    if (reply.ok && reply.result) useFolder(folder, reply.result);
    else fill(f.note, h("p", { class: "dt-note dt-bad" }, reply.output));
  }

  // The share's folders, from the disks that are awake (sleeping ones only
  // when asked), each with the disks it's on.
  function chooseFolder() {
    const share = built.move.share.value;
    if (!share) return toast("Choose a share first.", "bad");
    let at = folderOf(built.move.folder);
    let wake = false;
    let here = null;
    const crumbs = h("p", { class: "dt-crumbs" });
    const where = h("p", { class: "dt-muted" });
    const list = h("div", { class: "dt-scroll dt-picker" });
    const asleep = h("div");
    const ui = dialog(`Choose a folder in ${share}`, h("div", {}, crumbs, where, list, asleep),
      [{ label: "Choose", kind: "primary", onclick: (close) => {
        useFolder(at, here);
        close();
      } }]);
    const choose = ui.buttons[ui.buttons.length - 1];
    const link = (text, path) => h("a", { href: "#", onclick: (e) => {
      e.preventDefault();
      go(path);
    } }, text);
    async function go(path) {
      at = path;
      const parts = path ? path.split("/") : [];
      fill(crumbs, parts.length ? link(share, "") : h("b", {}, share),
        parts.map((part, i) => [" / ", i === parts.length - 1 ? h("b", {}, part) : link(part, parts.slice(0, i + 1).join("/"))]));
      fill(where);
      fill(list, h("p", { class: "dt-muted" }, "Looking…"));
      choose.disabled = true;
      choose.textContent = parts.length ? `Choose ${parts[parts.length - 1]}` : `Choose all of ${share}`;
      const reply = await post("folders", { share, folder: path, wake: wake ? "yes" : "no" });
      if (at !== path) return;
      if (!reply.ok || !reply.result) {
        fill(list, h("p", { class: "dt-note dt-bad" }, reply.output || "Couldn't look."));
        return;
      }
      here = reply.result;
      choose.disabled = !here.disks.length;
      fill(list,
        here.disks.length ? null : h("p", { class: "dt-note" }, "It isn't on any disk that's awake."),
        here.children.map((c) => {
          const sub = path ? `${path}/${c.name}` : c.name;
          return h("div", { class: "dt-pick-row" },
            link(c.name, sub),
            h("span", { class: "dt-muted" }, c.disks.join(", ")),
            h("button", { type: "button", onclick: () => {
              useFolder(sub, { disks: c.disks, asleep: here.asleep });
              ui.close();
            } }, "Choose"));
        }),
        here.disks.length && !here.children.length ? h("p", { class: "dt-muted" }, "No folders in it.") : null,
        here.more ? h("p", { class: "dt-muted" }, `… and ${here.more} more: type the path in the Folder box instead.`) : null);
      fill(asleep, here.asleep.length
        ? h("p", { class: "dt-muted" }, `Asleep, so not listed: ${here.asleep.join(", ")}. `,
          h("button", { type: "button", onclick: () => {
            wake = true;
            go(at);
          } }, "List them too (spins them up)"))
        : null);
      fill(where, here.disks.length ? `On ${here.disks.join(", ")}. Open a folder to look inside, or choose it.` : null);
    }
    go(at);
  }

  function moveArgs() {
    const f = built.move;
    return { share: f.share.value, folder: folderOf(f.folder), from: fromValues(), to: f.to.value };
  }

  function movePath(disk, args) {
    return `/mnt/${disk}/${args.share}${args.folder ? "/" + args.folder : ""}`;
  }

  async function planMove(button) {
    const args = moveArgs();
    if (!args.from.length) {
      fill(built.shares.plan, h("p", { class: "dt-note dt-bad" }, "Tick at least one disk to move from."));
      return;
    }
    const reply = await busy(button, () => post("move_plan", args));
    if (!reply.ok) {
      fill(built.shares.plan, h("p", { class: "dt-note dt-bad" }, reply.output));
      return;
    }
    const p = reply.result;
    const files = p.plans.reduce((n, x) => n + x.job.files, 0);
    const bytes = p.plans.reduce((n, x) => n + x.job.bytes, 0);
    fill(built.shares.plan,
      h("div", { class: `dt-note ${p.ok ? "" : "dt-bad"}` },
        p.plans.map((x) => h("div", {},
          h("p", {}, h("b", {}, movePath(x.job.from, args)), ` → ${args.to}: ${count(x.job.files, "file")}, ${size(x.job.bytes)}. `,
            x.ok ? "It can be moved. " : "It can't be moved: ", browse(movePath(x.job.from, args))),
          x.problems.length ? h("ul", {}, x.problems.map((m) => h("li", {}, m))) : null,
          x.warnings.length ? h("ul", { class: "dt-warn" }, x.warnings.map((m) => h("li", {}, m))) : null)),
        p.problems.length ? h("ul", {}, p.problems.map((m) => h("li", {}, m))) : null,
        p.plans.length > 1 ? h("p", {}, `Together ${count(files, "file")}, ${size(bytes)}, from one disk after another.`) : null,
        p.ok ? h("p", {}, h("button", { type: "button", class: "dt-primary", onclick: () => confirmMove(args, p.plans) },
          p.plans.length > 1 ? `Start ${p.plans.length} moves…` : "Start the move…")) : null));
  }

  function confirmMove(args, plans) {
    const empty = h("input", { type: "checkbox" });
    dialog("Move between disks",
      h("div", {},
        h("p", {}, `Copy to ${movePath(args.to, args)}:`),
        h("ul", { class: "dt-list" }, plans.map((x) => h("li", {}, `${movePath(x.job.from, args)}: ${count(x.job.files, "file")}, ${size(x.job.bytes)}`))),
        h("p", {}, "Each copy is checked against its original. Then the originals go to their disk's quarantine; nothing is deleted. Once you've checked the copies, empty the quarantine yourself."),
        h("label", { class: "dt-chip" }, empty, "Delete the originals once every copy checked out, instead of leaving them in the quarantine"),
        plans.length > 1 ? h("p", { class: "dt-muted" }, "The moves run one at a time, each planned again just before it starts.") : null,
        h("p", { class: "dt-muted" }, "Stopping a move stops it between files; copies made so far stay, the originals are untouched.")),
      [{ label: "Start", kind: "primary", onclick: async (close, button) => {
        const reply = await busy(button, () => post("move_start", { ...args, empty: empty.checked ? "1" : "0" }));
        if (!reply.ok) return toast(reply.output, "bad");
        close();
        fill(built.shares.plan);
        toast(plans.length > 1 ? "The moves are queued; they show below." : "The move started; it shows below.", "good");
        refreshSoon();
      } }]);
  }

  function renderMoves() {
    const moves = (report().moves || []).slice().sort((a, b) => b.started - a.started);
    if (!moves.length) {
      fill(built.shares.moves);
      return;
    }
    fill(built.shares.moves,
      h("h3", {}, "Moves"),
      h("table", { class: "dt-table" }, h("tbody", {}, moves.map((m) => {
        const pct = m.bytes ? Math.round((m.done_bytes / m.bytes) * 100) : m.state === "done" ? 100 : 0;
        const kinds = { done: "safe", failed: "protected", cancelled: "check", running: "plain", planned: "plain", queued: "plain" };
        return h("tr", {},
          h("td", { class: "dt-name" }, h("b", {}, `${m.share}${m.folder ? "/" + m.folder : ""}`), h("div", { class: "dt-muted" }, `${m.from} → ${m.to} · started ${ago(m.started)}`)),
          h("td", {}, badge(m.state, kinds[m.state])),
          h("td", { class: "dt-wide" },
            h("div", { class: "dt-bar" }, h("span", { style: `width:${pct}%` })),
            h("div", { class: "dt-muted" }, m.state === "queued" ? "waits for the move before it" : `${m.done_files} of ${m.files} files, ${size(m.done_bytes)} of ${size(m.bytes)}`, m.current ? ` · copying ${m.current}` : "", m.detail ? ` · ${m.detail}` : "")),
          h("td", { class: "dt-actions" }, m.state === "running" || m.state === "queued" ? h("button", { type: "button", onclick: async (e) => {
            const reply = await busy(e.target, () => post("move_cancel", { id: m.id }));
            toast(reply.ok ? (m.state === "queued" ? "It won't start." : "It stops after the file it's on.") : reply.output, reply.ok ? "good" : "bad");
            refreshSoon();
          } }, m.state === "queued" ? "Don't start" : "Stop") : null));
      }))));
  }

  // ----- Docker and VMs -----------------------------------------------

  const REMOVABLE = ["image", "dangling_image", "build_cache"];
  // Groups folded open, kept across refreshes.
  const unfolded = new Set();

  function canQuarantine(f) {
    return f.path && f.safety !== "protected" && ["appdata", "iso", "vdisk", "snapshot", "volume_folder"].includes(f.kind);
  }

  function canRemove(f) {
    return REMOVABLE.includes(f.kind) && f.safety !== "protected";
  }

  function bytesOf(findings) {
    return findings.reduce((n, f) => n + f.bytes, 0);
  }

  // A row: what it is, why it's listed and where, its size, and what can
  // be done with it. `docker` rows can be picked for removal.
  function findingRow(f, docker, pickColumn) {
    const box = docker && canRemove(f) ? h("input", { type: "checkbox", checked: picked.docker.has(f.id), title: "Pick to remove", onchange: (e) => {
      if (e.target.checked) picked.docker.add(f.id);
      else picked.docker.delete(f.id);
      renderApps();
    } }) : null;
    return h("div", { class: "dt-row" },
      pickColumn ? h("div", { class: "dt-pick" }, box) : null,
      h("div", { class: "dt-main" },
        h("b", { class: "dt-label" }, f.label),
        h("div", { class: "dt-muted" }, f.reason),
        f.path ? h("div", { class: "dt-muted dt-path" }, f.path) : null),
      h("div", { class: "dt-size" }, size(f.bytes)),
      h("div", { class: "dt-actions" },
        f.path ? browse(f.path, f.kind !== "appdata") : null,
        canQuarantine(f) ? h("button", { type: "button", onclick: () => askQuarantine([f.path], f.reason) }, "Quarantine…") : null));
  }

  // Findings under a heading with their count and size. A folded group
  // opens on a click and stays open across refreshes.
  function findingGroup(key, title, findings, { docker, note, folded } = {}) {
    if (!findings.length) return null;
    const pickColumn = docker && findings.some(canRemove);
    const chosen = findings.filter((f) => picked.docker.has(f.id));
    const head = [h("b", {}, title), h("span", { class: "dt-muted" }, ` · ${count(findings.length, "item")}, ${size(bytesOf(findings))}`)];
    const remove = !pickColumn ? null : chosen.length
      ? h("button", { type: "button", class: "dt-danger", onclick: () => confirmDocker(chosen) }, `Remove ${count(chosen.length, "item")} (${size(bytesOf(chosen))})…`)
      : h("span", { class: "dt-muted" }, "Tick what to remove");
    const rows = h("div", { class: `dt-rows${pickColumn ? " dt-pickable" : ""}` }, findings.map((f) => findingRow(f, docker, pickColumn)));
    const body = [note ? h("p", { class: "dt-muted" }, note) : null, rows];
    if (folded) {
      return h("details", { class: "dt-group", open: unfolded.has(key), ontoggle: (e) => {
        if (e.target.open) unfolded.add(key);
        else unfolded.delete(key);
      } }, h("summary", {}, head), body);
    }
    return h("div", { class: "dt-group" }, h("div", { class: "dt-group-head" }, h("div", {}, head), remove), body);
  }

  function dockerPart(d) {
    for (const id of [...picked.docker]) {
      if (!d.findings.some((f) => f.id === id)) picked.docker.delete(id);
    }
    const of = (safety) => d.findings.filter((f) => f.safety === safety);
    return [
      h("p", { class: "dt-muted" },
        d.running ? `${d.running_containers} of ${d.containers} containers running` : "Docker isn't running, so everything it holds is treated as in use",
        d.location ? ` · ${d.mode === "folder" ? "folder" : "docker.img"} ${d.location}` : "",
        d.image_size ? ` (${size(d.image_used)} used of ${size(d.image_size)})` : ""),
      d.findings.some((f) => f.safety !== "protected") ? null : h("p", {}, "Nothing to clean up."),
      findingGroup("docker-safe", "Safe to remove", of("safe"), { docker: true,
        note: "Unused images can be downloaded again; the build cache is rebuilt when needed." }),
      findingGroup("docker-check", "Check first", of("check"), { docker: true,
        note: "Probably not needed, but you may want them back. Appdata folders go to the quarantine, where they can be put back; volumes, logs and what's inside containers are only shown: deal with them in the Docker tab." }),
      findingGroup("docker-used", "In use", of("protected"), { docker: true, folded: true,
        note: "Used by a container or by Docker itself, so never offered." }),
    ];
  }

  function vmPart(vm) {
    return [
      vm.vms.length ? h("div", { class: "dt-rows" }, vm.vms.map((v) => h("div", { class: "dt-row" },
        h("div", { class: "dt-main" },
          h("b", { class: "dt-label" }, v.name), " ", v.running ? badge("running", "safe") : badge("stopped"),
          v.disks.map((disk) => h("div", { class: "dt-path" }, disk.path,
            disk.used !== null && disk.used !== undefined ? h("span", { class: "dt-muted" }, ` · ${size(disk.used)} used of ${size(disk.provisioned || disk.used)}`) : "")),
          v.isos.map((iso) => h("div", { class: "dt-muted dt-path" }, `CD: ${iso}`)),
          v.snapshots ? h("div", { class: "dt-muted" }, count(v.snapshots, "snapshot")) : null),
        h("div", { class: "dt-size" }, size(v.disks.reduce((n, disk) => n + (disk.used || 0), 0))),
        h("div", { class: "dt-actions" }, v.disks.length ? browse(v.disks[0].path, true) : null))))
        : h("p", { class: "dt-muted" }, "No VMs."),
      findingGroup("vm-unused", "Files no VM uses", vm.findings, {}),
    ];
  }

  function renderApps() {
    const root = document.getElementById("dt-apps");
    if (!root || !model.loaded) return;
    const s = model.status;
    const r = report();
    // Every refresh would otherwise rebuild the lists under the pointer.
    const key = JSON.stringify([model.running, s && [s.last_round, s.doing, s.skipped], r.docker, r.vms, [...picked.docker]]);
    if (built.appsKey === key) return;
    built.appsKey = key;
    if (!s) {
      fill(root, notRunning());
      return;
    }
    const missing = (off) => h("p", { class: "dt-muted" }, notLooked() || off);
    fill(root,
      notRunning(),
      lookNote(),
      h("h3", {}, "Docker"),
      r.docker ? dockerPart(r.docker) : missing("Docker isn't enabled (Settings → Docker)."),
      h("h3", {}, "VMs"),
      r.vms ? vmPart(r.vms) : missing("VMs aren't enabled (Settings → VM Manager)."));
  }

  function confirmDocker(items) {
    const ui = dialog("Remove from Docker",
      h("div", {},
        h("p", {}, "These are removed from Docker for good:"),
        h("ul", { class: "dt-list" }, items.map((f) => h("li", {}, `${f.label} · ${size(f.bytes)}`, h("div", { class: "dt-muted" }, f.reason)))),
        h("p", { class: "dt-muted" }, "Images still used by a container are never removed; Docker refuses those itself too.")),
      [{ label: `Remove ${size(items.reduce((n, f) => n + f.bytes, 0))}`, kind: "danger", onclick: async (close, button) => {
        const reply = await busy(button, () => post("docker_cleanup", { items: items.map((f) => f.id) }));
        if (!reply.ok) return toast(reply.output, "bad");
        close();
        picked.docker.clear();
        toast(reply.output || "Removed.", "good");
        refreshSoon();
      } }]);
    return ui;
  }

  // ----- Disk activity ------------------------------------------------

  function renderActivity() {
    const root = document.getElementById("dt-activity");
    if (!root || !model.loaded) return;
    const r = report();
    const activity = r.activity || [];
    const perms = r.permissions || [];
    fill(root,
      notRunning(),
      h("h3", {}, "What keeps disks awake"),
      activity.length ? h("table", { class: "dt-table" },
        h("thead", {}, h("tr", {}, ["Disk", "Spin-ups", "Awake", "Read", "Written", "Open now"].map((t, i) => h("th", { class: i && i < 5 ? "dt-num" : null }, t)))),
        h("tbody", {}, activity.map((a) => h("tr", {},
          h("td", {}, h("b", {}, a.disk)),
          h("td", { class: "dt-num" }, a.wakeups),
          h("td", { class: "dt-num" }, a.awake_minutes < 120 ? `${a.awake_minutes} min` : `${Math.round(a.awake_minutes / 60)} h`),
          h("td", { class: "dt-num" }, size(a.reads * 512)),
          h("td", { class: "dt-num" }, size(a.writes * 512)),
          h("td", { class: "dt-wide" }, a.open.length ? a.open.map((o) => h("div", { class: "dt-path" }, o.path, h("span", { class: "dt-muted" }, ` · ${o.process}${o.container ? ` in ${o.container}` : ""}`))) : h("span", { class: "dt-muted" }, "nothing")))))) : h("p", { class: "dt-muted" }, "Nothing yet; the first numbers show after a minute."),
      h("p", { class: "dt-muted" }, "Over the last day, from when FreeSize started. A disk that keeps waking up is usually read by an app on a schedule: move what it reads to a pool, or change the app's schedule."),
      h("h3", {}, "Permissions"),
      perms.length
        ? h("div", {},
          h("p", { class: "dt-muted" }, "Files not owned by nobody:users, or without read and write for their group, can't always be reached over SMB. Tools → New Permissions fixes them (Docker Safe New Perms leaves appdata alone). FreeSize only reports them."),
          perms.map((p) => h("details", { class: "dt-details" },
            h("summary", {}, `${p.share}: ${count(p.count, "item")}`),
            h("ul", { class: "dt-list" }, p.examples.map((e) => h("li", { class: "dt-path" }, e))))))
        : h("p", { class: "dt-muted" }, model.config && !model.config.permissions ? "Not checked (Settings)." : "Nothing odd found."));
  }

  // ----- Quarantine ---------------------------------------------------

  function renderQuarantine() {
    const root = document.getElementById("dt-quarantine");
    if (!root || !model.loaded) return;
    if (!built.quarantine) {
      const paths = h("textarea", { rows: 3, placeholder: "/mnt/user/Movies/Old Film (1999)\n/mnt/disk2/Backups/pc-2019.tar" });
      const reason = h("input", { type: "text", placeholder: "Why (optional), e.g. watched" });
      built.quarantine = { list: h("div"), head: h("div"), junk: h("div") };
      fill(root,
        built.quarantine.head,
        h("details", { class: "dt-details" },
          h("summary", {}, "Move something to the quarantine"),
          h("p", { class: "dt-muted" }, "One path per line, in a share (/mnt/user/…) or on a disk (/mnt/disk1/…). Each is moved aside on the disk it's on, so nothing is copied and it can be put back as it was. What the server needs is refused."),
          h("div", { class: "dt-form" }, h("label", { class: "dt-grow" }, "Paths", paths)),
          h("div", { class: "dt-form" }, h("label", { class: "dt-grow" }, "Reason", reason),
            h("button", { type: "button", onclick: () => {
              const list = paths.value.split("\n").map((p) => p.trim()).filter(Boolean);
              if (!list.length) return toast("Type at least one path.", "bad");
              askQuarantine(list, reason.value, () => {
                paths.value = "";
                reason.value = "";
              });
            } }, "Check…"))),
        built.quarantine.list,
        built.quarantine.junk);
    }
    const batches = (model.status && model.status.quarantine) || [];
    const total = batches.reduce((n, b) => n + b.bytes, 0);
    fill(built.quarantine.head,
      notRunning(),
      h("div", { class: "dt-head" },
        h("div", {},
          h("p", {}, h("b", {}, batches.length ? `${count(batches.length, "batch", "batches")}, ${size(total)}` : "The quarantine is empty.")),
          h("p", { class: "dt-muted" }, "Things moved aside wait here, on the disk they came from, until you put them back or delete them. Their space is freed only then. Nothing is deleted on a timer, and batches on a sleeping disk show again when it wakes.")),
        batches.length ? h("button", { type: "button", class: "dt-danger", onclick: () => confirmEmpty(batches) }, "Delete all for good…") : null));
    fill(built.quarantine.list, batches.slice().sort((a, b) => b.when - a.when).map((b) =>
      h("div", { class: "dt-card" },
        h("div", { class: "dt-head" },
          h("div", {},
            h("b", {}, `${count(b.items.length, "item")}, ${size(b.bytes)}`),
            h("div", { class: "dt-muted" }, `${date(b.when)} · by ${b.by}${b.reason ? ` · ${b.reason}` : ""}`)),
          h("div", { class: "dt-actions" },
            h("button", { type: "button", onclick: () => confirmRestore(b) }, "Put back…"),
            h("button", { type: "button", class: "dt-danger", onclick: () => confirmEmpty([b]) }, "Delete for good…"))),
        h("ul", { class: "dt-list" }, b.items.map((i) => h("li", {}, h("span", { class: "dt-path" }, i.original), ` · ${size(i.bytes)} `, browse(i.stored, true)))))));
    renderJunk();
  }

  // Trash folders computers and the Recycle Bin plugin leave in the
  // shares: the junk cleaner's findings on a server. Emptying one is for
  // good, so the paths are listed first.
  function renderJunk() {
    const junk = (model.status && model.status.junk) || [];
    const total = junk.reduce((n, j) => n + j.bytes, 0);
    fill(built.quarantine.junk,
      h("div", { class: "dt-head", style: "margin-top:16px" },
        h("div", {},
          h("p", {}, h("b", {}, junk.length ? `Trash folders in the shares: ${size(total)}` : "No trash folders with anything in them.")),
          h("p", { class: "dt-muted" }, "What the Recycle Bin plugin and computers' own trash (.Trash-1000, .Trashes) keep in the shares. Emptying them is for good, so each lists what goes. The Hub can ask to empty them too, with your approval."))),
      junk.map((j) => h("div", { class: "dt-card" },
        h("div", { class: "dt-head" },
          h("div", {}, h("b", {}, `${j.label}: ${size(j.bytes)}`), h("div", { class: "dt-muted" }, j.blurb)),
          h("div", { class: "dt-actions" },
            h("button", { type: "button", class: "dt-danger", onclick: () => confirmJunk(j) }, "Empty for good…"))),
        h("ul", { class: "dt-list" }, (j.paths || []).slice(0, 20).map((p) => h("li", {}, h("span", { class: "dt-path" }, p), " ", browse(p, false))),
          (j.paths || []).length > 20 ? h("li", { class: "dt-muted" }, `… and ${j.paths.length - 20} more`) : null))));
  }

  function confirmJunk(j) {
    const tick = h("input", { type: "checkbox" });
    const ui = dialog("Empty for good",
      h("div", {},
        h("p", {}, `${j.label}, ${count(j.items, "item")}, ${size(j.bytes)}, is deleted and can't be put back:`),
        h("ul", { class: "dt-list dt-scroll" }, (j.paths || []).map((p) => h("li", {}, h("span", { class: "dt-path" }, p), " ", browse(p, false)))),
        h("label", { class: "dt-tick" }, tick, " I've looked at these; delete them.")),
      [{ label: `Delete ${size(j.bytes)}`, kind: "danger", disabled: true, onclick: async (close, button) => {
        const reply = await busy(button, () => post("junk_clean", { rule: j.kind }));
        if (!reply.ok) return toast(reply.output, "bad");
        close();
        toast(reply.output || "Deleted.", "good");
        refreshSoon();
      } }]);
    const go = ui.buttons[ui.buttons.length - 1];
    tick.addEventListener("change", () => (go.disabled = !tick.checked));
  }

  // Lists what would go, with what deleting it later would really free,
  // then moves it aside.
  async function askQuarantine(paths, reason, done) {
    const estimate = h("p", { class: "dt-muted" }, "Working out what it holds…");
    const why = h("input", { type: "text", value: reason || "", placeholder: "Why (optional)" });
    const ui = dialog("Move to the quarantine",
      h("div", {},
        h("p", {}, "These move aside, on the disks they're on:"),
        h("ul", { class: "dt-list" }, paths.map((p) => h("li", {}, h("span", { class: "dt-path" }, p), " ", browse(p)))),
        estimate,
        h("label", { class: "dt-field" }, "Reason", why),
        h("p", { class: "dt-muted" }, "Nothing is deleted: you can put it back, or delete it for good later from this tab.")),
      [{ label: "Move to the quarantine", kind: "primary", onclick: async (close, button) => {
        const reply = await busy(button, () => post("quarantine_add", { paths, reason: why.value }));
        if (!reply.ok) return toast(reply.output, "bad");
        const results = reply.result || [];
        const refused = results.filter((r) => r.error);
        close();
        if (done && !refused.length) done();
        if (refused.length) {
          const moved = results.length - refused.length;
          dialog(moved ? "Some weren't moved" : "Nothing was moved",
            h("div", {},
              moved ? h("p", {}, `${count(moved, "item")} went to the quarantine. These stayed where they are:`) : null,
              h("ul", { class: "dt-list" }, refused.map((r) => h("li", {}, h("span", { class: "dt-path" }, r.path), h("div", { class: "dt-muted" }, r.error))))),
            []);
        } else {
          toast(`Moved ${count(results.length, "item")} to the quarantine.`, "good");
        }
        refreshSoon();
      } }]);
    const reply = await post("estimate", { paths });
    if (reply.ok && reply.result) estimate.textContent = `If deleted for good later: ${reply.result.text.replace(/^Frees/, "frees")}`;
    else estimate.textContent = reply.output || "";
    return ui;
  }

  function confirmRestore(batch) {
    dialog("Put back",
      h("div", {},
        h("p", {}, "These go back where they were:"),
        h("ul", { class: "dt-list" }, batch.items.map((i) => h("li", { class: "dt-path" }, i.original))),
        h("p", { class: "dt-muted" }, "Anything that's been put in their place since is left alone; that item stays in the quarantine.")),
      [{ label: "Put back", kind: "primary", onclick: async (close, button) => {
        const reply = await busy(button, () => post("quarantine_restore", { batch: batch.id }));
        if (!reply.ok) return toast(reply.output, "bad");
        close();
        toast(reply.output || "Put back.", "good");
        refreshSoon();
      } }]);
  }

  // The only place the page deletes files: everything listed, a box to
  // tick, then the button.
  function confirmEmpty(batches) {
    const items = batches.flatMap((b) => b.items);
    const bytes = batches.reduce((n, b) => n + b.bytes, 0);
    const tick = h("input", { type: "checkbox" });
    const ui = dialog("Delete for good",
      h("div", {},
        h("p", {}, `${count(items.length, "item")}, ${size(bytes)}, are deleted and can't be put back:`),
        h("ul", { class: "dt-list dt-scroll" }, items.map((i) => h("li", {}, h("span", { class: "dt-path" }, i.original), ` · ${size(i.bytes)} `, browse(i.stored, true)))),
        h("label", { class: "dt-tick" }, tick, " I've looked at these; delete them.")),
      [{ label: `Delete ${size(bytes)}`, kind: "danger", disabled: true, onclick: async (close, button) => {
        const reply = await busy(button, () => post("quarantine_empty", { batches: batches.map((b) => b.id) }));
        if (!reply.ok) return toast(reply.output, "bad");
        close();
        toast(reply.output || "Deleted.", "good");
        refreshSoon();
      } }]);
    const go = ui.buttons[ui.buttons.length - 1];
    tick.addEventListener("change", () => (go.disabled = !tick.checked));
  }

  // ----- Settings and Hub ---------------------------------------------

  function renderSettings() {
    const root = document.getElementById("dt-settings");
    if (!root || !model.loaded) return;
    if (!built.settings) {
      const c = model.config || { scan_hours: 24, wake_days: 7, permissions: true, report: true };
      const yesNo = (name, value) => h("select", { name }, h("option", { value: "yes", selected: value }, "Yes"), h("option", { value: "no", selected: !value }, "No"));
      const f = {
        scan_hours: h("input", { type: "number", min: 1, max: 720, value: c.scan_hours }),
        wake_days: h("input", { type: "number", min: 0, max: 365, value: c.wake_days }),
        permissions: yesNo("permissions", c.permissions),
        report: yesNo("report", c.report),
      };
      const keep = h("textarea", { rows: 4, placeholder: "/mnt/user/Backups\n/mnt/disk2/Photos", value: c.keep || "" });
      built.settings = { hub: h("div") };
      fill(root,
        h("h3", {}, "Settings"),
        h("div", { class: "dt-settings" },
          h("label", {}, "Look at the disks every", h("span", {}, f.scan_hours, " hours")),
          h("label", {}, "Wake a sleeping disk to scan it after", h("span", {}, f.wake_days, " days (0: never)")),
          h("label", {}, "Look for files with odd permissions", f.permissions),
          h("label", {}, "Report to the Hub, once paired", f.report),
          h("div", {}, h("button", { type: "button", onclick: async (e) => {
            const reply = await busy(e.target, () => post("settings", {
              scan_hours: f.scan_hours.value, wake_days: f.wake_days.value,
              permissions: f.permissions.value, report: f.report.value,
            }));
            toast(reply.output, reply.ok ? "good" : "bad");
            refreshSoon();
          } }, "Apply"))),
        h("h3", {}, "Never touch"),
        h("p", { class: "dt-muted" }, "Besides what FreeSize always keeps (the flash drive, the system share, Docker's and the VMs' files in use, whole disks and shares), these paths and everything in them are never offered, never moved and refused when the Hub asks. One per line, in a share or on a disk."),
        h("div", { class: "dt-form" }, h("label", { class: "dt-grow" }, "Paths", keep),
          h("button", { type: "button", onclick: async (e) => {
            const reply = await busy(e.target, () => post("keep", { paths: keep.value }));
            toast(reply.output, reply.ok ? "good" : "bad");
          } }, "Save")),
        built.settings.hub);
    }
    renderHub();
  }

  // An http address other than this machine's own: what's sent can be read
  // on the network. The same test as the freesize command's warning.
  function plainHttp(url) {
    try {
      const u = new URL(url);
      return u.protocol === "http:" && !["localhost", "127.0.0.1", "[::1]", "0.0.0.0"].includes(u.hostname);
    } catch {
      return false;
    }
  }

  function renderHub() {
    const hub = (model.status && model.status.hub) || {};
    const key = JSON.stringify(hub);
    if (built.hubKey === key) return;
    built.hubKey = key;
    const parts = [h("h3", {}, "FreeSize Hub")];
    if (!hub.paired) {
      const link = h("textarea", { rows: 2, placeholder: "freesize://pair?hub=…&code=…&key=…" });
      const name = h("input", { type: "text", placeholder: "the server's host name" });
      // Stays until the next try, unlike the toast.
      const problem = h("p", { class: "dt-note dt-bad" });
      problem.hidden = true;
      parts.push(
        h("p", { class: "dt-muted" }, "The Hub shows all your devices in one dashboard. In the Hub, choose Pair a device and paste the link here. Admins can then ask this server to do things, but nothing happens until its approver allows it on their phone."),
        h("div", { class: "dt-form" }, h("label", { class: "dt-grow" }, "Pairing link", link)),
        h("div", { class: "dt-form" }, h("label", { class: "dt-grow" }, "Name in the Hub", name),
          h("button", { type: "button", class: "dt-primary", onclick: async (e) => {
            const reply = await busy(e.target, () => post("pair", { link: link.value.trim(), name: name.value.trim() }));
            if (!reply.ok) {
              const text = String(reply.output || "pairing failed");
              problem.textContent = text.charAt(0).toUpperCase() + text.slice(1);
              problem.hidden = false;
              return toast(reply.output, "bad");
            }
            toast("Paired. Now choose the approver below.", "good");
            built.hubKey = null;
            refreshSoon();
          } }, "Pair")),
        problem);
    } else {
      parts.push(
        h("p", {}, "Paired with ", h("b", {}, hub.name), ` (${hub.url})`, hub.connection ? ` · ${hub.connection}` : " · waiting to hear back"),
        plainHttp(hub.url) ? h("p", { class: "dt-note" }, "The Hub's address is plain http, so what this server reports (folder and file names) can be read on your network. Put HTTPS in front of the Hub.") : null,
        h("p", { class: "dt-muted" }, "This server's key: ", h("code", {}, hub.device_key), ". The dashboard shows the same key for it."),
        h("h4", {}, "Approver"),
        h("p", { class: "dt-muted" }, "Nobody sits at a server, so the Hub's requests (put back, delete for good, Docker cleanup) go to one phone or computer to allow or refuse, for just that request or for some minutes. It can only be chosen here or with the freesize approver command on the server, never from the Hub."),
        hub.approver
          ? h("p", {}, "Requests go to ", h("b", {}, hub.approver), " (key ", h("code", {}, hub.approver_key), ").")
          : h("p", { class: "dt-note" }, "No approver: the Hub's requests are refused until you choose one."),
        approverForm(hub),
        h("p", {},
          hub.approver ? h("button", { type: "button", onclick: (e) => simple(e.target, "revoke", "End all grants now? Requests then need a new approval.") }, "End all grants") : null,
          hub.approver ? h("button", { type: "button", onclick: (e) => simple(e.target, "approver_clear", "Remove the approver? The Hub's requests are refused until you choose one.") }, "Remove approver") : null,
          h("button", { type: "button", class: "dt-danger", onclick: (e) => simple(e.target, "unpair", `Forget ${hub.name}? Remove this server in the dashboard too.`) }, "Unpair")));
    }
    fill(built.settings.hub, parts);
  }

  function approverForm() {
    const fingerprint = h("input", { type: "text", placeholder: "ABCD-EFGH-JKMN-PQRS", maxlength: 19, class: "dt-key" });
    const found = h("div");
    return h("div", {},
      h("div", { class: "dt-form" },
        h("label", {}, "Key of the new approver (FreeSize app → Hub → This device's key)", fingerprint),
        h("button", { type: "button", onclick: async (e) => {
          const reply = await busy(e.target, () => post("approver_check", { fingerprint: fingerprint.value }));
          if (!reply.ok) {
            fill(found, h("p", { class: "dt-note dt-bad" }, reply.output));
            return;
          }
          fill(found, h("div", { class: "dt-note" }, h("p", {}, reply.output),
            h("button", { type: "button", class: "dt-primary", onclick: async (e2) => {
              const set = await busy(e2.target, () => post("approver_set", { fingerprint: fingerprint.value }));
              if (!set.ok) return toast(set.output, "bad");
              toast("The approver is set.", "good");
              built.hubKey = null;
              refreshSoon();
            } }, "Make it the approver")));
        } }, "Look up")),
      found);
  }

  async function simple(button, action, question) {
    if (!window.confirm(question)) return;
    const reply = await busy(button, () => post(action));
    toast(reply.output, reply.ok ? "good" : "bad");
    built.hubKey = null;
    refreshSoon();
  }

  // ----- Map ----------------------------------------------------------
  //
  // The map is built only when asked for here, never by the scheduled
  // look. The background part keeps it in a file; the page asks for one
  // folder of it at a time, laid out for the size it has to draw in.

  // One hue per thing in the folder shown: [hue, saturation %, lightness %].
  const MAP_HUES = [[214, 58, 50], [152, 48, 42], [32, 62, 46], [354, 55, 52], [186, 55, 40], [322, 48, 50], [268, 48, 56], [88, 42, 40], [16, 62, 50], [232, 42, 58], [48, 60, 40], [170, 40, 34]];
  const map = { key: "", path: "", style: "treemap", colour: "folder", view: null, picked: null, moved: new Set(), width: 0, asking: 0, poll: null };

  function num(n) {
    return (Number(n) || 0).toLocaleString();
  }

  function mapStatus() {
    return (model.status && model.status.map) || {};
  }

  function targetLabel(code) {
    if (!code || code === "all") return "the whole array";
    const [kind, name] = [code.slice(0, code.indexOf(":")), code.slice(code.indexOf(":") + 1)];
    return kind === "share" ? `the ${name} share` : name;
  }

  function mapFull(path) {
    const meta = mapStatus().meta || {};
    return path ? `${meta.root}/${path}` : meta.root || "";
  }

  function renderMap() {
    const root = document.getElementById("dt-map");
    if (!root || !model.loaded) return;
    if (!built.map) {
      const b = {
        note: h("div"),
        target: h("select", { "aria-label": "What to map" }),
        build: h("button", { type: "button", class: "dt-primary", onclick: () => buildMap() }),
        state: h("div"),
        tools: h("div", { class: "dt-map-tools" }),
        stage: h("div", { class: "dt-map-stage", role: "img", "aria-label": "Map of what takes the space" }),
        legend: h("div", { class: "dt-map-legend" }),
        picked: h("div", { class: "dt-map-picked" }),
        about: h("div"),
      };
      b.body = h("div", { class: "dt-map-body", hidden: true }, b.tools, b.stage, b.legend, b.picked);
      built.map = b;
      fill(root,
        b.note,
        h("div", { class: "dt-form dt-map-pick" },
          h("label", {}, "Map of", b.target),
          b.build),
        b.state,
        b.body,
        b.about);
      // Drawn again when the page's width changes, and when the tab is
      // first shown (it has no width while another tab is in front).
      if (window.ResizeObserver) {
        new ResizeObserver(() => {
          const w = Math.round(b.stage.clientWidth);
          if (w && Math.abs(w - map.width) > 8) {
            clearTimeout(map.resize);
            map.resize = setTimeout(() => askView(), 150);
          }
        }).observe(b.stage);
      }
    }
    const b = built.map;
    const s = mapStatus();
    const building = s.state === "building";
    const ready = s.state === "ready" && s.meta;
    fill(b.note, notRunning());

    // What can be mapped: kept as chosen while the lists refresh.
    const disks = (report().disks || []).filter((d) => (d.kind === "data" || d.kind === "pool") && d.path);
    const shares = (report().shares || []).map((x) => x.name);
    const options = `all|${disks.map((d) => d.name).join(",")}|${shares.join("/")}`;
    if (b.options !== options) {
      b.options = options;
      const chosen = b.target.value || (ready ? s.target : "all");
      fill(b.target,
        h("option", { value: "all" }, "The whole array"),
        disks.length ? h("optgroup", { label: "One disk or pool" }, disks.map((d) => h("option", { value: `disk:${d.name}` }, d.name))) : null,
        shares.length ? h("optgroup", { label: "One share" }, shares.map((n) => h("option", { value: `share:${n}` }, n))) : null);
      b.target.value = chosen;
      if (!b.target.value) b.target.value = "all";
    }
    b.build.textContent = ready ? "Build again" : "Build map";
    b.build.disabled = building || !model.running;

    if (building) {
      fill(b.state, h("div", { class: "dt-note dt-busy", role: "status" }, h("span", { class: "dt-spinner" }),
        h("div", { class: "dt-grow" },
          h("b", {}, `Building the map of ${targetLabel(s.target)}`),
          h("div", { class: "dt-muted" }, s.doing && s.doing !== "starting" ? `${s.doing.charAt(0).toUpperCase()}${s.doing.slice(1)}.` : "Starting…", " Each disk is read in turn; the map shows here when the last one is done.")),
        h("button", { type: "button", onclick: async (e) => {
          await busy(e.target, () => post("map_cancel"));
          refreshSoon();
        } }, "Stop")));
    } else if (!ready) {
      fill(b.state,
        s.error ? h("p", { class: "dt-note dt-bad" }, `The map couldn't be built: ${s.error}.`) : null,
        h("p", { class: "dt-muted" }, "See what takes the space as boxes or bubbles, sized by what they hold. Choose the whole array, one disk or one share, then Build map. A folder spread over several disks shows once, with its parts added together. The map is only built when you ask for it here."));
    } else {
      fill(b.state, s.error ? h("p", { class: "dt-note dt-bad" }, `A new map couldn't be built (${s.error}), so this is still the one from before.`) : null);
    }
    b.body.hidden = !ready;

    // While it builds, look more often than every ten seconds.
    clearTimeout(map.poll);
    if (building) map.poll = setTimeout(load, 2000);

    if (!ready) {
      map.key = "";
      map.view = null;
      fill(b.about);
      return;
    }
    const meta = s.meta;
    const key = `${meta.target}:${meta.built}`;
    if (map.key !== key) {
      map.key = key;
      map.path = "";
      map.view = null;
      map.picked = null;
      map.moved = new Set();
      if ((meta.disks || []).length < 2) map.colour = "folder";
      askView();
    }
    const cut = [];
    if (meta.floor) cut.push(`Files under ${size(meta.floor)} are added up per folder as "smaller files".`);
    if (meta.folder_floor) cut.push(`Folders under ${size(meta.folder_floor)} are drawn closed.`);
    fill(b.about,
      h("p", { class: "dt-muted" },
        `Map of ${targetLabel(meta.target)}: ${size(meta.size)} in ${num(meta.files)} files, on ${count((meta.disks || []).length, "disk")}. Built ${ago(meta.built)} in ${meta.seconds < 90 ? `${Math.max(1, meta.seconds)} s` : `${Math.round(meta.seconds / 60)} min`}; it shows the server as it was then. `,
        cut.join(" "), " ",
        h("a", { href: "#", class: "dt-link", onclick: async (e) => {
          e.preventDefault();
          if (!window.confirm("Remove this map? Nothing on the disks changes; you can build it again.")) return;
          await post("map_discard");
          refreshSoon();
        } }, "Remove this map")),
      (meta.missed || []).length ? h("p", { class: "dt-note" }, `Not on the map: ${meta.missed.join("; ")}.`) : null,
      meta.unreadable ? h("p", { class: "dt-muted" }, `${count(meta.unreadable, "folder or file", "folders or files")} couldn't be read and aren't counted.`) : null);
  }

  async function buildMap() {
    const b = built.map;
    const target = b.target.value || "all";
    const reply = await busy(b.build, () => post("map_build", { target }));
    if (!reply.ok) return toast(reply.output, "bad");
    b.build.disabled = true;
    setTimeout(load, 800);
  }

  // Asks for the folder shown, laid out for the stage's size now.
  async function askView() {
    const b = built.map;
    if (!b || mapStatus().state !== "ready") return;
    const width = Math.round(b.stage.clientWidth);
    if (!width) return;
    const height = Math.round(Math.max(300, Math.min(680, width * 0.62, window.innerHeight * 0.72)));
    map.width = width;
    b.stage.style.height = `${height}px`;
    const asking = ++map.asking;
    b.stage.classList.add("dt-map-loading");
    const reply = await post("map_view", { path: map.path, style: map.style, width, height });
    if (asking !== map.asking) return;
    b.stage.classList.remove("dt-map-loading");
    if (!reply.ok || !reply.result) {
      // The folder isn't on a map built since: back to the top.
      if (map.path) {
        map.path = "";
        return askView();
      }
      map.view = null;
      fill(b.tools);
      fill(b.legend);
      fill(b.picked);
      fill(b.stage, h("p", { class: "dt-map-empty" }, reply.output || "The map couldn't be read."));
      return;
    }
    map.view = reply.result;
    if (map.picked && !map.view.items.some((i) => sameItem(i, map.picked))) map.picked = null;
    drawMap();
  }

  function sameItem(a, b) {
    return a.path === b.path && a.kind === b.kind && a.name === b.name;
  }

  function openFolder(path) {
    map.path = path;
    map.picked = null;
    askView();
  }

  function itemName(i) {
    if (i.kind === "smaller") return `${num(i.files)} smaller ${i.files === 1 ? "file" : "files"}`;
    if (i.kind === "other") return `${num(i.files)} smaller ${i.files === 1 ? "thing" : "things"}`;
    return i.name;
  }

  function onDisks(disks) {
    if (!disks || !disks.length) return "";
    if (disks.length === 1) return `on ${disks[0][0]}`;
    return `on ${disks.map(([name, bytes]) => `${name} (${size(bytes)})`).join(", ")}`;
  }

  function mapColour(i, colours) {
    if (map.colour === "disk") {
      const disk = i.disks && i.disks.length ? i.disks[0][0] : null;
      const base = colours[disk] || "#7a7f87";
      // Folders with their contents inside are frames, darker.
      return i.nested ? `color-mix(in srgb, ${base} 62%, #000)` : base;
    }
    const [hue, sat, light] = MAP_HUES[i.group % MAP_HUES.length];
    if (i.nested) return `hsl(${hue} ${sat}% ${Math.max(18, light - 16)}%)`;
    return `hsl(${hue} ${sat}% ${Math.min(62, light + i.depth * 3)}%)`;
  }

  function drawMap() {
    const b = built.map;
    const view = map.view;
    if (!b || !view) return;
    const meta = mapStatus().meta || {};
    const colours = diskColors();
    const bubbles = map.style === "bubbles";
    const several = (meta.disks || []).length > 1;

    const seg = (value, current, label, set) => h("button", { type: "button", class: value === current ? "dt-on" : null, "aria-pressed": value === current ? "true" : "false", onclick: () => set(value) }, label);
    const setStyle = (v) => {
      if (map.style === v) return;
      map.style = v;
      askView();
    };
    const setColour = (v) => {
      map.colour = v;
      drawMap();
    };
    const top = targetLabel(meta.target);
    fill(b.tools,
      h("div", { class: "dt-map-crumbs" },
        view.crumbs.map((c, n) => {
          const name = n === 0 ? top.charAt(0).toUpperCase() + top.slice(1) : c.name;
          const last = n === view.crumbs.length - 1;
          return [n ? h("span", { class: "dt-muted" }, " › ") : null,
            last ? h("b", {}, name) : h("a", { href: "#", onclick: (e) => {
              e.preventDefault();
              openFolder(c.path);
            } }, name)];
        }),
        h("span", { class: "dt-muted" }, `  ${size(view.size)}`)),
      h("div", { class: "dt-map-switches" },
        several ? h("div", { class: "dt-seg", role: "group", "aria-label": "Colour by" },
          seg("folder", map.colour, "By folder", setColour), seg("disk", map.colour, "By disk", setColour)) : null,
        h("div", { class: "dt-seg", role: "group", "aria-label": "Kind of map" },
          seg("treemap", map.style, "Treemap", setStyle), seg("bubbles", map.style, "Bubbles", setStyle))));

    // A bubble's name when its contents are drawn inside it: on top of
    // them all, along its upper edge.
    const names = [];
    const shapes = view.items.map((i) => {
      const full = mapFull(i.path);
      const label = itemName(i);
      const tip = [label, `${size(i.size)}${i.kind === "file" || i.kind === "other" || i.kind === "smaller" ? "" : ` · ${num(i.files)} ${i.files === 1 ? "file" : "files"}`}`, onDisks(i.disks)].filter(Boolean).join("\n");
      const classes = ["dt-map-item", `dt-map-${i.kind}`];
      if (i.nested) classes.push("dt-map-nested");
      if (map.picked && sameItem(i, map.picked)) classes.push("dt-map-sel");
      if ([...map.moved].some((p) => full === p || full.startsWith(`${p}/`)) && i.kind !== "smaller" && i.kind !== "other") classes.push("dt-map-gone");
      let style;
      let text = null;
      if (bubbles) {
        style = `left:${i.x - i.r}px;top:${i.y - i.r}px;width:${2 * i.r}px;height:${2 * i.r}px;`;
        if (i.nested) {
          if (i.r >= 60) names.push(h("span", { class: "dt-map-name", style: `left:${i.x - i.r * 0.62}px;top:${i.y - i.r + 3}px;width:${i.r * 1.24}px;` }, label));
        } else if (i.r >= 26) {
          text = h("span", { class: "dt-map-text" }, h("span", {}, label), i.r >= 34 ? h("small", {}, size(i.size)) : null);
        }
      } else {
        style = `left:${i.x}px;top:${i.y}px;width:${i.w}px;height:${i.h}px;`;
        if (i.nested) {
          if (i.w >= 40) text = h("span", { class: "dt-map-head" }, label, i.w >= 120 ? h("small", {}, ` ${size(i.size)}`) : null);
        } else if (i.w >= 46 && i.h >= 18) {
          text = h("span", { class: "dt-map-text" }, h("span", {}, label), i.h >= 34 ? h("small", {}, size(i.size)) : null);
        }
      }
      return h("div", {
        class: classes.join(" "),
        style: `${style}background:${mapColour(i, colours)};`,
        title: tip,
        onclick: (e) => {
          e.stopPropagation();
          map.picked = i;
          drawMap();
        },
        ondblclick: (e) => {
          e.stopPropagation();
          if (i.kind === "folder") openFolder(i.path);
        },
      }, text);
    });
    b.stage.classList.toggle("dt-map-bubbles", bubbles);
    b.stage.onclick = () => {
      if (!map.picked) return;
      map.picked = null;
      drawMap();
    };
    fill(b.stage, shapes.length ? [shapes, names] : h("p", { class: "dt-map-empty" }, "Nothing here takes any space."));

    // Which colour is which disk.
    fill(b.legend, map.colour === "disk" && several
      ? (view.disks || []).map(([name, bytes]) => h("span", {}, h("span", { class: "dt-swatch", style: `background:${colours[name] || "#7a7f87"}` }), `${name} ${size(bytes)}`))
      : null);

    drawPicked();
  }

  function drawPicked() {
    const b = built.map;
    const view = map.view;
    const i = map.picked;
    const up = view.crumbs.length > 1
      ? h("button", { type: "button", onclick: () => openFolder(view.crumbs[view.crumbs.length - 2].path) }, "Up")
      : null;
    if (!i) {
      fill(b.picked,
        h("div", { class: "dt-grow dt-muted" }, "Click anything on the map to see what it is; double-click a folder to open it."),
        h("div", { class: "dt-actions" }, up, view.path ? browse(mapFull(view.path)) : null));
      return;
    }
    const full = mapFull(i.path);
    const real = i.kind === "file" || i.kind === "folder" || i.kind === "closed";
    const moved = map.moved.has(full);
    const meta = mapStatus().meta || {};
    let more = "";
    if (i.kind === "smaller") more = `Files under ${size(meta.floor)} in this folder, added together.`;
    else if (i.kind === "other") more = "The things here too small for a bubble of their own.";
    else if (i.kind === "closed") more = "Too small for the map to keep what's inside it.";
    if (moved) more = "Moved to the quarantine since the map was built.";
    fill(b.picked,
      h("div", { class: "dt-grow" },
        h("b", { class: "dt-path" }, real ? full : itemName(i)),
        h("div", { class: "dt-muted" },
          [size(i.size), i.kind === "folder" || i.kind === "closed" ? `${num(i.files)} ${i.files === 1 ? "file" : "files"}` : null, onDisks(i.disks), real ? null : `in ${full}`, more].filter(Boolean).join(" · "))),
      h("div", { class: "dt-actions" },
        up,
        i.kind === "folder" ? h("button", { type: "button", onclick: () => openFolder(i.path) }, "Open") : null,
        browse(full, i.kind === "file"),
        real && !moved ? h("button", { type: "button", onclick: () => askQuarantine([full], "", () => {
          map.moved.add(full);
          drawMap();
        }) }, "Move to the quarantine…") : null));
  }

  // ----- Start --------------------------------------------------------

  function start() {
    load();
    setInterval(() => {
      if (!document.hidden) load();
    }, 10000);
  }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
  else start();
})();
