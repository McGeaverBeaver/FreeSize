// DiskTree for Unraid: the page under Tools → DiskTree. It shows what the
// background part found (status.php) and asks for changes (action.php).
// Every change that removes or moves something first lists exactly what,
// with links to look at it in Unraid's file browser.
(function () {
  "use strict";
  if (window.DiskTreePage) return;
  window.DiskTreePage = true;

  const API = "/plugins/disktree/include/";
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
        ? "DiskTree's background part has stopped; what's shown is from when it last ran. Start it again with: /etc/rc.d/rc.disktree start"
        : "DiskTree's background part isn't running yet. Start it with: /etc/rc.d/rc.disktree start");
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
          h("p", {}, h("b", {}, `DiskTree ${s.version}`), ` on Unraid ${r.version || "?"}`, look ? "" : ` · now ${s.doing || "idle"}`),
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
          : notLooked() ? "not looked at yet" : "not enabled", "DiskTreeApps"),
        fact("Quarantine", s.quarantine && s.quarantine.length
          ? `${count(s.quarantine.length, "batch", "batches")}, ${size(quarantined)}` : "empty", "DiskTreeQuarantine"),
        fact("Hub", s.hub && s.hub.paired ? `${s.hub.name}${s.hub.connection ? ` (${s.hub.connection})` : ""}` : "not paired", "DiskTreeSettings")));
  }

  function fact(title, text, tab) {
    return h("div", { class: "dt-fact" }, h("div", { class: "dt-muted" }, title), h("div", {}, text), tabLink(tab));
  }

  function tabLink(tab) {
    // Unraid's tabs are inputs labelled by the page titles; click through.
    const tabs = { DiskTreeApps: 3, DiskTreeQuarantine: 5, DiskTreeSettings: 6 };
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
        moves: h("div"),
        look: h("div"),
      };
      built.shares.asOf = h("p", { class: "dt-muted" });
      fill(root,
        built.shares.look,
        h("h3", {}, "Shares and their disks"),
        built.shares.asOf,
        built.shares.table,
        h("h3", {}, "Move between disks"),
        h("p", { class: "dt-muted" }, "Moves a share, or a folder in it, from one disk to another. Each file is copied, the copy checked against the original, and only then the original moved into the source disk's quarantine, where it stays until you empty it. The share's path in /mnt/user stays the same."),
        built.shares.form,
        built.shares.plan,
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
    buildMoveForm();
    renderMoves();
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
    const select = (name, options, value) => h("select", { name }, options.map((o) => h("option", { value: o, selected: o === value }, o)));
    const f = {
      share: select("share", shares, old.share && old.share.value),
      folder: h("input", { type: "text", name: "folder", placeholder: "all of it, or a folder inside, e.g. Old Films", value: old.folder ? old.folder.value : "" }),
      from: select("from", disks, old.from && old.from.value),
      to: select("to", disks, old.to && old.to.value),
    };
    built.move = f;
    f.from.addEventListener("change", () => pickTarget(true));
    pickTarget(false);
    const plan = h("button", { type: "button", onclick: (e) => planMove(e.target) }, "Plan");
    fill(built.shares.form,
      h("div", { class: "dt-form" },
        h("label", {}, "Share", f.share),
        h("label", { class: "dt-grow" }, "Folder", f.folder),
        h("label", {}, "From", f.from),
        h("label", {}, "To", f.to),
        plan));
  }

  // Moving to the disk it's on can't be meant: offer the one with the
  // most room instead.
  function pickTarget(always) {
    const f = built.move;
    if (!always && f.to.value && f.to.value !== f.from.value) return;
    const roomiest = moveDisks().filter((d) => d.name !== f.from.value).sort((a, b) => b.free - a.free)[0];
    if (roomiest) f.to.value = roomiest.name;
  }

  function pickShare(name) {
    if (!built.move) return;
    built.move.share.value = name;
    const share = (report().shares || []).find((s) => s.name === name);
    const biggest = share && share.placements.filter((p) => !p.stale).sort((a, b) => b.bytes - a.bytes)[0];
    if (biggest) built.move.from.value = biggest.disk;
    pickTarget(false);
    built.move.folder.focus();
  }

  function moveArgs() {
    const f = built.move;
    return { share: f.share.value, folder: f.folder.value.trim(), from: f.from.value, to: f.to.value };
  }

  async function planMove(button) {
    const args = moveArgs();
    const reply = await busy(button, () => post("move_plan", args));
    if (!reply.ok) {
      fill(built.shares.plan, h("p", { class: "dt-note dt-bad" }, reply.output));
      return;
    }
    const p = reply.result;
    const job = p.job;
    const what = `/mnt/${args.from}/${args.share}${args.folder ? "/" + args.folder : ""}`;
    fill(built.shares.plan,
      h("div", { class: `dt-note ${p.ok ? "" : "dt-bad"}` },
        h("p", {}, h("b", {}, what), ` → ${args.to}: ${count(job.files, "file")}, ${size(job.bytes)}. `, p.ok ? "It can be moved." : "It can't be moved:"),
        p.problems.length ? h("ul", {}, p.problems.map((x) => h("li", {}, x))) : null,
        p.warnings.length ? h("ul", { class: "dt-warn" }, p.warnings.map((x) => h("li", {}, x))) : null,
        h("p", {}, browse(what),
          p.ok ? h("button", { type: "button", class: "dt-primary", onclick: () => confirmMove(args, job, what) }, "Start the move…") : null)));
  }

  function confirmMove(args, job, what) {
    dialog("Move between disks",
      h("div", {},
        h("p", {}, `Copy ${count(job.files, "file")} (${size(job.bytes)}) from ${what} to /mnt/${args.to}/${args.share}${args.folder ? "/" + args.folder : ""}.`),
        h("p", {}, `Each copy is checked against its original. Then the originals go to ${args.from}'s quarantine; nothing is deleted. Once you've checked the copies, empty the quarantine yourself.`),
        h("p", { class: "dt-muted" }, "Stopping the move stops it between files; copies made so far stay, the originals are untouched.")),
      [{ label: "Start", kind: "primary", onclick: async (close, button) => {
        const reply = await busy(button, () => post("move_start", args));
        if (!reply.ok) return toast(reply.output, "bad");
        close();
        fill(built.shares.plan);
        toast("The move started; it shows below.", "good");
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
        const kinds = { done: "safe", failed: "protected", cancelled: "check", running: "plain", planned: "plain" };
        return h("tr", {},
          h("td", { class: "dt-name" }, h("b", {}, `${m.share}${m.folder ? "/" + m.folder : ""}`), h("div", { class: "dt-muted" }, `${m.from} → ${m.to} · started ${ago(m.started)}`)),
          h("td", {}, badge(m.state, kinds[m.state])),
          h("td", { class: "dt-wide" },
            h("div", { class: "dt-bar" }, h("span", { style: `width:${pct}%` })),
            h("div", { class: "dt-muted" }, `${m.done_files} of ${m.files} files, ${size(m.done_bytes)} of ${size(m.bytes)}`, m.detail ? ` · ${m.detail}` : "")),
          h("td", { class: "dt-actions" }, m.state === "running" ? h("button", { type: "button", onclick: async (e) => {
            const reply = await busy(e.target, () => post("move_cancel", { id: m.id }));
            toast(reply.ok ? "It stops after the file it's on." : reply.output, reply.ok ? "good" : "bad");
            refreshSoon();
          } }, "Stop") : null));
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
      h("p", { class: "dt-muted" }, "Over the last day, from when DiskTree started. A disk that keeps waking up is usually read by an app on a schedule: move what it reads to a pool, or change the app's schedule."),
      h("h3", {}, "Permissions"),
      perms.length
        ? h("div", {},
          h("p", { class: "dt-muted" }, "Files not owned by nobody:users, or without read and write for their group, can't always be reached over SMB. Tools → New Permissions fixes them (Docker Safe New Perms leaves appdata alone). DiskTree only reports them."),
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
      built.quarantine = { list: h("div"), head: h("div") };
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
        built.quarantine.list);
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
        h("p", { class: "dt-muted" }, "Besides what DiskTree always keeps (the flash drive, the system share, Docker's and the VMs' files in use, whole disks and shares), these paths and everything in them are never offered, never moved and refused when the Hub asks. One per line, in a share or on a disk."),
        h("div", { class: "dt-form" }, h("label", { class: "dt-grow" }, "Paths", keep),
          h("button", { type: "button", onclick: async (e) => {
            const reply = await busy(e.target, () => post("keep", { paths: keep.value }));
            toast(reply.output, reply.ok ? "good" : "bad");
          } }, "Save")),
        built.settings.hub);
    }
    renderHub();
  }

  function renderHub() {
    const hub = (model.status && model.status.hub) || {};
    const key = JSON.stringify(hub);
    if (built.hubKey === key) return;
    built.hubKey = key;
    const parts = [h("h3", {}, "DiskTree Hub")];
    if (!hub.paired) {
      const link = h("textarea", { rows: 2, placeholder: "disktree://pair?hub=…&code=…&key=…" });
      const name = h("input", { type: "text", placeholder: "the server's host name" });
      parts.push(
        h("p", { class: "dt-muted" }, "The Hub shows all your devices in one dashboard. In the Hub, choose Pair a device and paste the link here. Admins can then ask this server to do things, but nothing happens until its approver allows it on their phone."),
        h("div", { class: "dt-form" }, h("label", { class: "dt-grow" }, "Pairing link", link)),
        h("div", { class: "dt-form" }, h("label", { class: "dt-grow" }, "Name in the Hub", name),
          h("button", { type: "button", class: "dt-primary", onclick: async (e) => {
            const reply = await busy(e.target, () => post("pair", { link: link.value.trim(), name: name.value.trim() }));
            if (!reply.ok) return toast(reply.output, "bad");
            toast("Paired. Now choose the approver below.", "good");
            built.hubKey = null;
            refreshSoon();
          } }, "Pair")));
    } else {
      parts.push(
        h("p", {}, "Paired with ", h("b", {}, hub.name), ` (${hub.url})`, hub.connection ? ` · ${hub.connection}` : " · waiting to hear back"),
        h("p", { class: "dt-muted" }, "This server's key: ", h("code", {}, hub.device_key), ". The dashboard shows the same key for it."),
        h("h4", {}, "Approver"),
        h("p", { class: "dt-muted" }, "Nobody sits at a server, so the Hub's requests (put back, delete for good, Docker cleanup) go to one phone or computer to allow or refuse, for just that request or for some minutes. It can only be chosen here or with the disktree approver command on the server, never from the Hub."),
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
        h("label", {}, "Key of the new approver (DiskTree app → Hub → This device's key)", fingerprint),
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
