// p2pagents web UI. Plain JS, no build step. All peer-provided text is rendered
// with textContent (never innerHTML), so chat and task output can't inject markup.
"use strict";

const $ = (sel) => document.querySelector(sel);

function el(tag, attrs = {}, ...children) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value === undefined || value === null || value === false) continue;
    if (key === "class") node.className = value;
    else if (key === "text") node.textContent = value;
    else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
    else node.setAttribute(key, value === true ? "" : value);
  }
  for (const child of children.flat()) {
    if (child === null || child === undefined || child === false) continue;
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
}

function icon(paths) {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  for (const d of paths) {
    const p = document.createElementNS("http://www.w3.org/2000/svg", "path");
    p.setAttribute("d", d);
    svg.append(p);
  }
  return svg;
}
const ICONS = {
  trash: ["M4 7h16", "M10 11v6M14 11v6", "M6 7l1 13h10l1-13", "M9 7V4h6v3"],
  copy: ["M9 9h11v11H9z", "M5 15V5a2 2 0 0 1 2-2h10"],
  stop: ["M6 6h12v12H6z"],
  peers: ["M8 11a3 3 0 1 0 0-6 3 3 0 0 0 0 6z", "M16 11a3 3 0 1 0 0-6", "M2 20a6 6 0 0 1 12 0", "M15 14a6 6 0 0 1 7 6"],
};

// ---------- theme ----------
const THEME_KEY = "p2pagents-theme";
const media = window.matchMedia("(prefers-color-scheme: dark)");
function themeChoice() {
  const saved = localStorage.getItem(THEME_KEY);
  return ["light", "dark", "system"].includes(saved) ? saved : "system";
}
function applyTheme() {
  const choice = themeChoice();
  const effective = choice === "system" ? (media.matches ? "dark" : "light") : choice;
  document.documentElement.dataset.theme = effective;
  for (const btn of document.querySelectorAll("[data-theme-choice]")) {
    const on = btn.dataset.themeChoice === choice;
    btn.setAttribute("aria-checked", String(on));
    btn.tabIndex = on ? 0 : -1;
  }
}
media.addEventListener("change", applyTheme);

// Radio groups: click and arrow keys (WAI-ARIA radiogroup pattern).
function radioGroup(container, onSelect) {
  const buttons = () => [...container.querySelectorAll('[role="radio"]')];
  container.addEventListener("click", (e) => {
    const btn = e.target.closest('[role="radio"]');
    if (btn) onSelect(btn, false);
  });
  container.addEventListener("keydown", (e) => {
    const keys = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 };
    if (!(e.key in keys)) return;
    e.preventDefault();
    const list = buttons();
    const i = list.indexOf(document.activeElement);
    const next = list[(i + keys[e.key] + list.length) % list.length];
    next.focus();
    onSelect(next, true);
  });
}

// Minimal, safe Markdown for task results: builds DOM nodes with textContent only.
function inlineMd(text) {
  const nodes = [];
  const re = /(`[^`]+`|\*\*[^*]+\*\*)/g;
  let last = 0;
  let m;
  while ((m = re.exec(text))) {
    if (m.index > last) nodes.push(text.slice(last, m.index));
    const t = m[0];
    nodes.push(t.startsWith("`") ? el("code", { text: t.slice(1, -1) }) : el("strong", { text: t.slice(2, -2) }));
    last = m.index + t.length;
  }
  if (last < text.length) nodes.push(text.slice(last));
  return nodes;
}

function mdTable(rows) {
  const cells = (row) => row.trim().replace(/^\||\|$/g, "").split(/(?<!\\)\|/).map((c) => c.trim().replace(/\\\|/g, "|"));
  const [head, ...body] = rows.filter((r) => !/^\s*\|?\s*:?-{3,}/.test(r));
  // Not a real table (e.g. only a separator line): show the text as is.
  if (head === undefined) return el("p", { text: rows.join("\n") });
  return el("div", { class: "table-wrap" }, el("table", {},
    el("thead", {}, el("tr", {}, ...cells(head).map((c) => el("th", { scope: "col" }, inlineMd(c))))),
    el("tbody", {}, ...body.map((r) => el("tr", {}, ...cells(r).map((c) => el("td", {}, inlineMd(c))))))));
}

function renderMarkdown(text) {
  const root = el("div", { class: "md" });
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  let list = null;
  let listType = null;
  let para = [];
  const flushPara = () => {
    if (para.length) root.append(el("p", {}, inlineMd(para.join(" "))));
    para = [];
  };
  const endList = () => { list = null; listType = null; };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    // Collapsible sections in evidence reports: rendered as <details>, content as Markdown.
    const details = line.match(/^<details><summary>(.*)<\/summary>$/);
    if (details) {
      flushPara(); endList();
      const inner = [];
      for (i++; i < lines.length && lines[i].trim() !== "</details>"; i++) inner.push(lines[i]);
      root.append(el("details", {}, el("summary", { text: details[1] }), renderMarkdown(inner.join("\n"))));
      continue;
    }
    if (/^\s*\|/.test(line)) {
      flushPara(); endList();
      const rows = [];
      for (; i < lines.length && /^\s*\|/.test(lines[i]); i++) rows.push(lines[i]);
      i--;
      root.append(mdTable(rows));
      continue;
    }
    if (/^\s*```/.test(line)) {
      flushPara(); endList();
      const code = [];
      for (i++; i < lines.length && !/^\s*```/.test(lines[i]); i++) code.push(lines[i]);
      root.append(el("pre", {}, el("code", { text: code.join("\n") })));
      continue;
    }
    const heading = line.match(/^(#{1,4})\s+(.*)$/);
    if (heading) {
      flushPara(); endList();
      root.append(el(`h${Math.min(6, heading[1].length + 2)}`, {}, inlineMd(heading[2])));
      continue;
    }
    const item = line.match(/^\s*([-*+]|\d+[.)])\s+(.*)$/);
    if (item) {
      flushPara();
      const type = /\d/.test(item[1]) ? "ol" : "ul";
      if (!list || listType !== type) {
        list = el(type);
        listType = type;
        root.append(list);
      }
      list.append(el("li", {}, inlineMd(item[2])));
      continue;
    }
    if (!line.trim()) { flushPara(); endList(); continue; }
    if (list && /^\s{2,}/.test(line)) { list.lastChild.append(" ", ...inlineMd(line.trim())); continue; }
    endList();
    para.push(line.trim());
  }
  flushPara();
  return root;
}

// ---------- API ----------
const hashToken = new URLSearchParams(location.hash.slice(1)).get("t");
if (hashToken) {
  sessionStorage.setItem("p2pagents-token", hashToken);
  history.replaceState(null, "", location.pathname);
}
const token = sessionStorage.getItem("p2pagents-token") || "";
// Opening a new link (#t=…) in an existing tab only changes the hash: adopt it.
window.addEventListener("hashchange", () => {
  const t = new URLSearchParams(location.hash.slice(1)).get("t");
  if (t) {
    sessionStorage.setItem("p2pagents-token", t);
    location.replace(location.pathname);
  }
});

let authLost = false;
function showAuthError() {
  if (authLost) return;
  authLost = true;
  events?.close();
  const pill = $("#net-status");
  pill.dataset.state = "offline";
  pill.querySelector(".label").textContent = "Session ended";
  $("#feed").replaceChildren(el("div", { class: "feed-empty" },
    el("h2", { text: "This page's session has ended" }),
    el("p", { text: "The agent was restarted, so its access link changed. Open the new link p2pagents printed (it ends with #t=…)." })));
  for (const c of document.querySelectorAll("#composer textarea, #composer button, #add-peer-btn")) c.disabled = true;
}

async function api(path, options = {}) {
  const res = await fetch(path, {
    ...options,
    headers: { "Content-Type": "application/json", Authorization: `Bearer ${token}`, ...(options.headers || {}) },
  });
  if (res.status === 401) {
    showAuthError();
    throw new Error("Session ended: open the new link printed by p2pagents");
  }
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(body.error || `${res.status} ${res.statusText}`);
  return body;
}

// ---------- state ----------
const state = { me: null, peers: [], trusted: [], feed: [], tasks: [], mode: "chat" };

function hue(text) {
  let h = 0;
  for (const ch of text) h = (h * 31 + ch.codePointAt(0)) % 360;
  return h;
}
function avatar(name, online) {
  const node = el("span", { class: "avatar", "aria-hidden": "true" },
    (name[0] || "?").toUpperCase(),
    online === undefined ? null : el("span", { class: `presence${online ? " on" : ""}` }));
  // CSSOM, not a style attribute: the page's CSP forbids inline style attributes.
  node.style.setProperty("--hue", String(hue(name)));
  return node;
}
function providerLabel(p) {
  return { copilot: "Copilot", claude: "Claude", codex: "Codex" }[p] || p;
}
function clock(ms) {
  return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}
function duration(ms) {
  const s = Math.max(0, Math.round(ms / 1000));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
}

function toast(text, kind = "") {
  const node = el("div", { class: `toast ${kind}`, role: kind === "error" ? "alert" : "status", text });
  $("#toasts").append(node);
  setTimeout(() => node.remove(), kind === "error" ? 6000 : 2600);
}
async function copy(text, what) {
  try {
    await navigator.clipboard.writeText(text);
    toast(`${what} copied`);
  } catch {
    toast(`Could not copy ${what.toLowerCase()}`, "error");
  }
}

// ---------- rendering ----------
function renderMe() {
  const me = state.me;
  if (!me) return;
  document.title = `${me.name} · p2pagents`;
  $("#me-name").textContent = me.name;
  const idChip = $("#me-id");
  idChip.textContent = me.short;
  idChip.title = `Endpoint id ${me.id} (click to copy)`;
  $("#ticket").textContent = me.ticket;

  const pill = $("#net-status");
  const online = me.network === "local" || !!me.relay;
  pill.dataset.state = online ? "online" : "offline";
  pill.querySelector(".label").textContent =
    me.network === "local" ? "Local network" : me.relay ? "Online" : "Offline";
  pill.title = me.relay ? `Home relay: ${me.relay}` : me.network === "local" ? "Direct IP connections only" : "No relay reachable";

  const facts = $("#facts");
  facts.replaceChildren();
  const fact = (k, v) => facts.append(el("dt", { text: k }), el("dd", {}, v));
  fact("Profile", me.profile);
  fact("Network", me.network === "local" ? "Local (direct IPs)" : "Internet");
  if (me.providers.length) {
    fact("Providers", me.providers.map(providerLabel).join(", "));
    for (const a of me.accounts) fact(providerLabel(a.provider), a.account);
    fact("Serves peers", me.serving ? "Yes" : "No");
    fact("Your own tasks", el("span", {}, `${ACCESS_SHORT[me.access] ?? me.access} in `,
      el("span", { class: "mono", text: me.workspace })));
    if (me.serving) {
      fact("Shared folders", el("ul", { class: "share-list" },
        ...me.shares.map((s) => el("li", {},
          el("strong", { text: s.name }), ` · at most ${ACCESS_SHORT[s.max_access] ?? s.max_access}`,
          el("div", { class: "mono small", text: s.path })))));
    }
  } else {
    fact("Providers", "None (delegates only)");
  }
}

function renderPeers() {
  const list = $("#peer-list");
  list.replaceChildren();
  $("#peer-count").textContent = state.peers.length;
  $("#peer-count").setAttribute("aria-label", `${state.peers.length} connected`);
  if (!state.peers.length) {
    list.append(el("li", { class: "empty" },
      state.trusted.length
        ? "Waiting for allowlisted peers to come online…"
        : el("span", {}, "No peers yet. ", el("strong", { text: "Add a peer" }), " to the allowlist and share your ticket.")));
  }
  for (const p of state.peers) {
    list.append(el("li", {},
      avatar(p.name, true),
      el("div", { class: "who" },
        el("span", { class: "name", text: p.name }),
        p.owner || p.role
          ? el("span", { class: "role", text: [p.owner ? `Digital worker of ${p.owner}` : null, p.role].filter(Boolean).join(" · ") })
          : null,
        el("span", { class: "sub" },
          el("span", { class: "mono", text: p.short }),
          p.capabilities.length
            ? p.capabilities.map((c) => el("span", { class: "badge", text: providerLabel(c) }))
            : el("span", { class: "badge neutral", text: "no providers" })))));
  }

  const trust = $("#trust-list");
  trust.replaceChildren();
  if (!state.trusted.length) {
    trust.append(el("li", { class: "empty", text: "Only allowlisted peers can connect to this agent." }));
  }
  for (const t of state.trusted) {
    trust.append(el("li", {},
      avatar(t.name, t.connected),
      el("div", { class: "who" },
        el("span", { class: "name", text: t.name }),
        el("span", { class: "sub" },
          el("span", { text: t.connected ? "Connected" : "Offline" }),
          t.can_delegate
            ? el("span", { class: "badge ok", title: `May run tasks in: ${(t.shares ?? []).join(", ")}`, text: `Tasks: ${ACCESS_SHORT[t.access] ?? "Read only"} · ${t.tasks_per_hour}/h · ${(t.shares ?? []).join(", ")}` })
            : el("span", { class: "badge neutral", text: "Chat only" }))),
      el("button", {
        type: "button", class: "icon-btn", title: `Remove ${t.name}`, "aria-label": `Remove ${t.name} from the allowlist`,
        onclick: () => removePeer(t),
      }, icon(ICONS.trash))));
  }
  renderTaskOptions();
}

function feedNode(item) {
  if (item.kind === "chat") {
    return el("div", { class: `msg${item.self ? " self" : ""}` },
      item.self ? null : avatar(item.from),
      el("div", {},
        el("div", { class: "meta" }, item.self ? "You" : item.from, " · ", el("time", { text: clock(item.ts) })),
        el("div", { class: "bubble", text: item.text })));
  }
  return el("div", { class: "sys", "data-level": item.level },
    el("span", { text: item.text }), el("time", { text: clock(item.ts) }));
}

function renderFeed() {
  const feed = $("#feed");
  feed.replaceChildren();
  const meaningful = state.feed.some((i) => i.kind === "chat" || i.level !== "info");
  if (!meaningful) {
    feed.append(el("div", { class: "feed-empty" },
      icon(ICONS.peers),
      el("h2", { text: "No activity yet" }),
      el("p", { text: "Messages, peers joining and leaving, and tasks from peers show up here." })));
  }
  for (const item of state.feed) feed.append(feedNode(item));
  feed.scrollTop = feed.scrollHeight;
}

function appendFeed(item) {
  state.feed.push(item);
  const feed = $("#feed");
  const nearBottom = feed.scrollHeight - feed.scrollTop - feed.clientHeight < 80;
  if (feed.querySelector(".feed-empty") && (item.kind === "chat" || item.level !== "info")) {
    renderFeed();
    return;
  }
  feed.append(feedNode(item));
  if (nearBottom || item.self) feed.scrollTop = feed.scrollHeight;
}

const openResults = new Map();

function taskNode(t) {
  const statusText = {
    running: "Running", done: "Done", failed: "Failed", cancelled: "Cancelled", waiting: "Waiting", skipped: "Skipped",
  }[t.status];
  const elapsed = (t.finished_ms || Date.now()) - t.started_ms;
  const title = t.step
    ? `Step ${t.step} · ${t.approval ? "Approval" : providerLabel(t.provider)} → ${t.target}`
    : `#${t.id} · ${providerLabel(t.provider)} → ${t.target}`;
  const meta = t.status === "waiting"
    ? el("div", { class: "task-meta", text: t.after?.length ? `Waits for: ${t.after.join(", ")}` : "Starting…" })
    : el("div", { class: "task-meta" }, `Started ${clock(t.started_ms)} · `,
      el("span", { class: "elapsed", "data-start": t.status === "running" ? t.started_ms : null },
        t.status === "running" ? `running ${duration(elapsed)}` : `took ${duration(elapsed)}`),
      t.after?.length ? ` · after ${t.after.join(", ")}` : null);
  const node = el("li", { class: `task ${t.status}`, "data-task": t.id },
    el("div", { class: "task-top" },
      el("span", { class: "task-title", text: title }),
      t.evidence ? el("span", {
        class: `badge evidence ${t.evidence}`,
        title: t.evidence === "verified"
          ? "The result carries evidence signed by the peer that did the work"
          : "The evidence is NOT validly signed by that peer",
        text: t.evidence === "verified" ? "Signed ✓" : "Unverified ✗",
      }) : null,
      el("span", { class: `status ${t.status}` },
        t.status === "running" ? el("span", { class: "spinner", "aria-hidden": "true" }) : null, statusText)),
    el("p", { class: "task-prompt", text: t.prompt }),
    meta);
  if (t.status === "running" || (t.plan && t.status === "waiting")) {
    node.append(el("button", {
      type: "button", class: "btn small",
      "aria-label": t.plan ? `Cancel the plan of step ${t.step}` : `Cancel task #${t.id}`,
      onclick: (e) => cancelTask(t, e.currentTarget),
    }, icon(ICONS.stop), t.plan ? "Cancel plan" : "Cancel"));
  }
  if (t.output !== null && t.output !== undefined && !["cancelled", "skipped"].includes(t.status)) {
    const open = openResults.has(t.id) ? openResults.get(t.id) : true;
    const details = el("details", { open: open || undefined },
      el("summary", { text: t.status === "failed" ? "Error" : "Result" }),
      t.status === "failed"
        ? el("pre", { text: t.output, tabindex: "0" })
        : el("div", { class: "result", tabindex: "0" }, renderMarkdown(t.output)),
      el("button", { type: "button", class: "btn small", onclick: () => copy(t.output, "Result") }, icon(ICONS.copy), "Copy"));
    details.addEventListener("toggle", () => openResults.set(t.id, details.open));
    node.append(details);
  }
  return node;
}

function renderTasks() {
  const list = $("#task-list");
  list.replaceChildren();
  $("#task-count").textContent = state.tasks.length;
  if (!state.tasks.length) {
    list.append(el("li", { class: "empty" }, "No tasks yet. Choose ", el("strong", { text: "Run task" }),
      " below to run a prompt on this agent or a peer."));
  }
  for (const t of [...state.tasks].reverse()) list.append(taskNode(t));
  renderSquads();
}

// ---------- squads ----------
function renderSquads() {
  const list = $("#squad-list");
  list.replaceChildren();
  const squads = state.squads ?? [];
  if (!squads.length) {
    list.append(el("li", { class: "empty" }, "Group peers into squads, then give a squad steps in a ",
      el("strong", { text: "Plan" }), "."));
  }
  for (const s of squads) {
    const online = s.members.filter((m) => m.online).length;
    list.append(el("li", { class: "squad" },
      el("div", { class: "who" },
        el("span", { class: "name", text: `#${s.name}` }),
        el("span", { class: "sub", text: `${online}/${s.members.length} online` }),
        el("ul", { class: "squad-members" }, ...s.members.map((m) => {
          const busy = state.tasks.filter((t) => t.status === "running" && t.target === m.name).length;
          return el("li", {},
            el("span", { class: `dot${m.online ? " on" : ""}`, "aria-hidden": "true" }),
            el("span", { text: m.name }),
            el("span", { class: "muted small", text: m.online ? (busy ? ` · ${busy} running` : " · idle") : " · offline" }));
        }))),
      el("button", {
        type: "button", class: "icon-btn", title: `Remove squad ${s.name}`, "aria-label": `Remove squad ${s.name}`,
        onclick: () => removeSquad(s.name),
      }, icon(ICONS.trash))));
  }
}

function openSquadDialog() {
  $("#squad-form").reset();
  $("#squad-error").textContent = "";
  const box = $("#squad-members");
  box.replaceChildren(...(state.trusted.length
    ? state.trusted.map((t) => el("label", { class: "check" },
      el("input", { type: "checkbox", value: t.name }),
      el("span", { text: `${t.name}${t.connected ? "" : " (offline)"}` })))
    : [el("span", { class: "muted small", text: "Add peers to the allowlist first." })]));
  $("#squad-dialog").showModal();
  $("#squad-name").focus();
}

async function submitSquad(e) {
  e.preventDefault();
  const name = $("#squad-name").value.trim();
  const members = [...document.querySelectorAll("#squad-members input:checked")].map((i) => i.value);
  if (!name || !members.length) {
    $("#squad-error").textContent = "Enter a name and pick at least one member.";
    return;
  }
  try {
    await api("/api/squads", { method: "POST", body: JSON.stringify({ name, members }) });
    $("#squad-dialog").close();
    toast(`Squad #${name} saved`);
  } catch (err) {
    $("#squad-error").textContent = err.message;
  }
}

async function removeSquad(name) {
  if (!confirm(`Remove squad #${name}? Its members stay on the allowlist.`)) return;
  try {
    await api(`/api/squads/${encodeURIComponent(name)}`, { method: "DELETE" });
    toast(`Squad #${name} removed`);
  } catch (err) {
    toast(err.message, "error");
  }
}

function upsertTask(task) {
  const i = state.tasks.findIndex((t) => t.id === task.id);
  const isNew = i < 0;
  if (isNew) state.tasks.push(task);
  else state.tasks[i] = task;
  renderTasks();
  if (!isNew && !["running", "waiting"].includes(task.status)) {
    const verb = { done: "finished", failed: "failed", cancelled: "cancelled", skipped: "skipped" }[task.status];
    const label = task.step ? `Step ${task.step}` : `Task #${task.id}`;
    toast(`${label} ${verb}`, task.status === "failed" ? "error" : "");
  }
}

async function cancelTask(t, button) {
  button.disabled = true;
  try {
    await api(`/api/tasks/${t.id}`, { method: "DELETE" });
  } catch (err) {
    button.disabled = false;
    toast(err.message, "error");
  }
}

// ---------- approvals ----------
function renderApprovals() {
  const list = $("#approvals-list");
  const approvals = state.approvals ?? [];
  $("#approvals-panel").hidden = !approvals.length;
  $("#approvals-count").textContent = approvals.length;
  list.replaceChildren(...approvals.map((a) => {
    const note = el("input", { type: "text", class: "approval-note", maxlength: "500", "aria-label": `Note for request ${a.id}`, placeholder: "Note (required to reject)" });
    const error = el("p", { class: "form-error", role: "alert" });
    const answer = async (approved, button) => {
      if (!approved && !note.value.trim()) {
        error.textContent = "Say why you reject it.";
        note.focus();
        return;
      }
      button.disabled = true;
      try {
        await api(`/api/approvals/${a.id}`, { method: "POST", body: JSON.stringify({ approved, note: note.value }) });
        toast(approved ? "Approved" : "Rejected");
      } catch (err) {
        button.disabled = false;
        error.textContent = err.message;
      }
    };
    return el("li", { class: "task approval" },
      el("div", { class: "task-top" },
        el("span", { class: "task-title", text: `${a.title} · from ${a.from}` }),
        el("span", { class: "status waiting", text: "Needs you" })),
      el("details", {}, el("summary", { text: "What you're approving" }),
        el("div", { class: "result", tabindex: "0" }, renderMarkdown(a.text))),
      note,
      error,
      el("div", { class: "approval-actions" },
        el("button", { type: "button", class: "btn primary small", onclick: (e) => answer(true, e.currentTarget) }, "Approve"),
        el("button", { type: "button", class: "btn small danger", onclick: (e) => answer(false, e.currentTarget) }, "Reject")));
  }));
}

// ---------- evidence runs ----------
async function loadRuns() {
  const res = await api("/api/runs");
  const list = $("#runs-list");
  $("#runs-count").textContent = res.runs.length;
  list.replaceChildren();
  if (!res.runs.length) {
    list.append(el("li", { class: "empty" }, "No runs yet. Run a ", el("strong", { text: "Plan" }),
      " with claim: lines and verify= checks to collect evidence."));
  }
  for (const r of res.runs.slice(0, 20)) {
    const verdict = r.claims ? (r.passed ? "Pass" : "Fail") : "No claims";
    list.append(el("li", { class: "run-item" },
      el("button", { type: "button", class: "run-open", onclick: () => openRun(r.id), "aria-label": `Open evidence run ${r.id}` },
        el("span", { class: "task-title", text: r.id }),
        el("span", { class: `status ${r.passed ? "done" : r.claims ? "failed" : "skipped"}`, text: verdict })),
      el("div", { class: "task-meta", text: `${clock(r.started_unix * 1000)} · ${r.proven}/${r.claims} claims proven · ${r.steps_done}/${r.steps} steps done` })));
  }
}

async function openRun(id) {
  try {
    const run = await api(`/api/runs/${encodeURIComponent(id)}`);
    $("#run-h").textContent = `Evidence run ${id}`;
    $("#run-verdict").textContent = run.verdict;
    $("#run-verdict").dataset.pass = String(run.summary.passed);
    $("#run-signatures").replaceChildren(...run.signatures.map((s) => el("li", { class: s.ok ? "ok" : "bad" },
      el("strong", { text: `${s.ok ? "✓" : "✗"} ${s.step}: ` }), s.text)));
    $("#run-report").replaceChildren(renderMarkdown(run.report));
    $("#run-dialog").showModal();
    $("#run-close").focus();
  } catch (err) {
    toast(err.message, "error");
  }
}

// ---------- audit log ----------
const OUTCOME_TEXT = { done: "Done", failed: "Failed", cancelled: "Cancelled", refused: "Refused" };
const OUTCOME_CLASS = { done: "done", failed: "failed", cancelled: "cancelled", refused: "failed" };

async function loadAudit() {
  const res = await api("/api/audit?limit=50");
  const panel = $("#audit-panel");
  panel.hidden = !res.enabled;
  if (!res.enabled) return;
  const list = $("#audit-list");
  list.replaceChildren();
  $("#audit-count").textContent = res.entries.length;
  if (!res.entries.length) {
    list.append(el("li", { class: "empty", text: "No peer has run a task on this agent yet." }));
  }
  for (const e of res.entries) {
    const access = e.access ? ACCESS_SHORT[e.access] ?? e.access : "no access";
    list.append(el("li", { class: "audit-item" },
      el("div", { class: "task-top" },
        el("span", { class: "task-title", text: `${e.peer} · #${e.task_id} · ${providerLabel(e.provider)}` }),
        el("span", { class: `status ${OUTCOME_CLASS[e.outcome] ?? ""}`, text: OUTCOME_TEXT[e.outcome] ?? e.outcome })),
      el("div", { class: "task-meta", text: `${clock(e.ts_ms)} · ${access} · ${duration(e.duration_ms)} · ${e.prompt_chars} chars, sha256 ${e.prompt_sha256.slice(0, 10)}` }),
      e.error ? el("div", { class: e.outcome === "cancelled" ? "audit-note" : "audit-error", text: e.error }) : null));
  }
}

// ---------- composer ----------
function availableProviders() {
  const set = new Set(state.me?.providers || []);
  for (const p of state.peers) for (const c of p.capabilities) set.add(c);
  return [...set];
}

function renderTaskOptions() {
  const providerSel = $("#provider");
  const targetSel = $("#target");
  const prevProvider = providerSel.value;
  const prevTarget = targetSel.value;
  const providers = availableProviders();
  providerSel.replaceChildren(...providers.map((p) => el("option", { value: p, text: providerLabel(p) })));
  if (providers.includes(prevProvider)) providerSel.value = prevProvider;

  const provider = providerSel.value;
  const targets = [];
  if (provider && state.me?.providers.includes(provider)) targets.push(["local", "This agent"]);
  const serving = state.peers.filter((p) => p.capabilities.includes(provider));
  if (serving.length > 1) targets.push(["auto", "Any peer"]);
  for (const p of serving) targets.push([p.id, p.name]);
  targetSel.replaceChildren(...targets.map(([v, label]) => el("option", { value: v, text: label })));
  if (targets.some(([v]) => v === prevTarget)) targetSel.value = prevTarget;
  renderShareOptions();
  updateComposer();
}

/** Folder picker for a peer target: the folders that peer shares with this agent. */
function renderShareOptions() {
  const sel = $("#share");
  const prev = sel.value;
  const peer = state.peers.find((p) => p.id === $("#target").value);
  const shares = peer?.shares ?? [];
  $("#share-field").hidden = !peer || !shares.length;
  sel.replaceChildren(...shares.map((s) =>
    el("option", { value: s.name, text: `${s.name} (${(ACCESS_SHORT[s.access] ?? s.access).toLowerCase()})` })));
  if (shares.some((s) => s.name === prev)) sel.value = prev;
}

const PLAN_EXAMPLE = "claim: C-1 The parser handles empty input\nfix @alice:repo verify=test attempts=3 claims=C-1 Make the parser accept empty input\nreview after fix #qa:repo apply=fix verify=test claims=C-1 Check the fix\nsignoff after review approve @me claims=C-1 Approve the change";

function updateComposer() {
  const task = state.mode === "task";
  const plan = state.mode === "plan";
  const input = $("#composer-input");
  const noProviders = task && !availableProviders().length;
  $("#task-opts").hidden = !task;
  input.rows = plan ? 6 : 2;
  input.placeholder = plan
    ? `claim: ID statement, then one step per line: id [after a,b] [approve] @peer[:folder]|#squad [using claude] [verify=a,b attempts=N claims=ID apply=step] prompt\n${PLAN_EXAMPLE}`
    : task
      ? noProviders
        ? "No provider available: enable one with --providers, or connect a peer that serves one"
        : "Describe the task, e.g. “Add unit tests for the parser module”"
      : "Message all connected peers…";
  $("#composer-label").textContent = plan ? "Plan" : task ? "Task prompt" : "Message";
  $("#send-btn").querySelector("span").textContent = plan ? "Run plan" : task ? "Run" : "Send";
  $("#send-btn").disabled = noProviders || (task && !$("#target").value);
  $("#composer-hint").textContent = plan
    ? "Ctrl+Enter to run · claims are proven only by checks peers run (verify=) or a sign-off (approve) · the run is saved under Evidence runs"
    : "Enter to send · Shift+Enter for a new line";
}

async function submitComposer(e) {
  e?.preventDefault();
  const input = $("#composer-input");
  const text = input.value.trim();
  if (!text || $("#send-btn").disabled) return;
  $("#send-btn").disabled = true;
  try {
    if (state.mode === "chat") {
      const res = await api("/api/chat", { method: "POST", body: JSON.stringify({ text }) });
      if (res.sent === 0) toast("No peers connected; nobody received the message");
    } else if (state.mode === "plan") {
      const res = await api("/api/plans", { method: "POST", body: JSON.stringify({ text }) });
      toast(`Plan started: ${Object.keys(res.steps).length} steps`);
    } else {
      const task = await api("/api/tasks", {
        method: "POST",
        body: JSON.stringify({
          provider: $("#provider").value,
          target: $("#target").value,
          share: $("#share-field").hidden ? null : $("#share").value,
          prompt: text,
        }),
      });
      upsertTask(task);
      toast(`Task #${task.id} started on ${task.target}`);
    }
    input.value = "";
  } catch (err) {
    toast(err.message, "error");
  } finally {
    updateComposer();
    input.focus();
  }
}

// ---------- allowlist ----------
const ACCESS_SHORT = { "read-only": "Read only", "workspace-write": "Edit files", full: "Full" };

function syncDelegateFields() {
  const on = $("#add-delegate").checked;
  $("#rate-field").hidden = !on;
  $("#access-field").hidden = !on;
  $("#shares-field").hidden = !on;
  const chosen = $("#add-access").value;
  const hint = [];
  if (chosen === "full") {
    hint.push("Shell commands run in an OS sandbox: writes only in the folder, no network.");
  }
  hint.push("Their tasks run only in the folders you tick below; files outside them and credential files (.env, keys, .ssh…) are always refused.");
  $("#access-hint").textContent = hint.join(" ");
}

function renderShareChecks() {
  const box = $("#add-shares");
  box.replaceChildren(...(state.me?.shares ?? []).map((s) =>
    el("label", { class: "check" },
      el("input", { type: "checkbox", value: s.name, checked: s.name === "data" || undefined }),
      el("span", {}, el("strong", { text: s.name }), ` · at most ${(ACCESS_SHORT[s.max_access] ?? s.max_access).toLowerCase()} · `,
        el("span", { class: "mono", text: s.path })))));
}

function openAddDialog() {
  $("#add-form").reset();
  renderShareChecks();
  syncDelegateFields();
  $("#add-error").textContent = "";
  $("#add-dialog").showModal();
  $("#add-ticket").focus();
}

async function submitAdd(e) {
  e.preventDefault();
  const ticket = $("#add-ticket").value.trim();
  const errorBox = $("#add-error");
  if (!ticket) {
    errorBox.textContent = "Paste the peer's ticket or endpoint id.";
    $("#add-ticket").focus();
    return;
  }
  const delegate = $("#add-delegate").checked;
  try {
    const res = await api("/api/trust", {
      method: "POST",
      body: JSON.stringify({
        ticket,
        name: $("#add-name").value.trim() || null,
        delegate,
        tasks_per_hour: delegate ? Number($("#add-rate").value) || 20 : null,
        access: delegate ? $("#add-access").value : "read-only",
        shares: delegate ? [...document.querySelectorAll("#add-shares input:checked")].map((i) => i.value) : null,
      }),
    });
    $("#add-dialog").close();
    toast(`${res.name} added to the allowlist`);
  } catch (err) {
    errorBox.textContent = err.message;
  }
}

async function removePeer(t) {
  if (!confirm(`Remove ${t.name} from the allowlist? It will be disconnected and can no longer connect.`)) return;
  try {
    await api(`/api/trust/${encodeURIComponent(t.id)}`, { method: "DELETE" });
    toast(`${t.name} removed`);
  } catch (err) {
    toast(err.message, "error");
  }
}

// ---------- live updates ----------
async function loadState() {
  const s = await api("/api/state");
  Object.assign(state, { me: s.me, peers: s.peers, trusted: s.trusted, squads: s.squads, feed: s.feed, tasks: s.tasks, approvals: s.approvals });
  renderMe();
  renderPeers();
  renderFeed();
  renderTasks();
  renderApprovals();
  await Promise.all([loadAudit(), loadRuns()]);
}

let events = null;
function connectEvents() {
  const source = new EventSource(`/api/events?token=${encodeURIComponent(token)}`);
  events = source;
  source.onopen = () => loadState().catch((err) => toast(err.message, "error"));
  source.onmessage = (e) => {
    const msg = JSON.parse(e.data);
    if (msg.type === "feed") appendFeed(msg.item);
    else if (msg.type === "task") upsertTask(msg.task);
    else if (msg.type === "audit") loadAudit().catch((err) => toast(err.message, "error"));
    else if (msg.type === "runs") loadRuns().catch((err) => toast(err.message, "error"));
    else if (msg.type === "approvals") {
      const before = state.approvals?.length ?? 0;
      state.approvals = msg.approvals;
      renderApprovals();
      if (msg.approvals.length > before) toast("A teammate asks for your approval");
    }
    else if (msg.type === "peers") {
      state.peers = msg.peers;
      state.trusted = msg.trusted;
      state.squads = msg.squads;
      renderPeers();
      renderSquads();
    }
  };
  source.onerror = () => {
    if (authLost) return;
    const pill = $("#net-status");
    pill.dataset.state = "offline";
    pill.querySelector(".label").textContent = "Reconnecting…";
    // EventSource can't see status codes; probe once to tell "agent down" from "token rejected".
    fetch("/api/state", { headers: { Authorization: `Bearer ${token}` } })
      .then((res) => { if (res.status === 401) showAuthError(); })
      .catch(() => {});
  };
}

// ---------- wiring ----------
function init() {
  applyTheme();
  radioGroup($(".theme-switch"), (btn) => {
    localStorage.setItem(THEME_KEY, btn.dataset.themeChoice);
    applyTheme();
  });
  radioGroup($(".segmented"), (btn, viaKeyboard) => {
    state.mode = btn.dataset.mode;
    for (const b of document.querySelectorAll("[data-mode]")) {
      b.setAttribute("aria-checked", String(b === btn));
      b.tabIndex = b === btn ? 0 : -1;
    }
    renderTaskOptions();
    // Keyboard users stay in the switch (arrow keys); mouse users go straight to typing.
    if (!viaKeyboard) $("#composer-input").focus();
  });
  for (const b of document.querySelectorAll("[data-mode]")) b.tabIndex = b.dataset.mode === state.mode ? 0 : -1;

  $("#composer").addEventListener("submit", submitComposer);
  $("#composer-input").addEventListener("keydown", (e) => {
    if (e.key !== "Enter" || e.isComposing) return;
    // In a plan Enter starts a new step; Ctrl/Cmd+Enter runs it.
    if (state.mode === "plan" ? e.ctrlKey || e.metaKey : !e.shiftKey) submitComposer(e);
  });
  $("#add-squad-btn").addEventListener("click", openSquadDialog);
  $("#run-close").addEventListener("click", () => $("#run-dialog").close());
  $("#run-dialog").addEventListener("click", (e) => {
    if (e.target === e.currentTarget) $("#run-dialog").close();
  });
  $("#squad-cancel").addEventListener("click", () => $("#squad-dialog").close());
  $("#squad-dialog").addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      $("#squad-dialog").close();
    }
  });
  $("#squad-dialog").addEventListener("click", (e) => {
    if (e.target === e.currentTarget) $("#squad-dialog").close();
  });
  $("#squad-dialog").addEventListener("close", () => $("#add-squad-btn").focus());
  $("#squad-form").addEventListener("submit", submitSquad);
  $("#provider").addEventListener("change", renderTaskOptions);
  $("#target").addEventListener("change", () => { renderShareOptions(); updateComposer(); });
  $("#me-id").addEventListener("click", () => state.me && copy(state.me.id, "Endpoint id"));
  $("#copy-ticket").addEventListener("click", () => state.me && copy(state.me.ticket, "Ticket"));
  $("#add-peer-btn").addEventListener("click", openAddDialog);
  $("#add-cancel").addEventListener("click", () => $("#add-dialog").close());
  // Not every browser closes a modal <dialog> on Escape; also close on a backdrop click.
  $("#add-dialog").addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      $("#add-dialog").close();
    }
  });
  $("#add-dialog").addEventListener("click", (e) => {
    if (e.target === e.currentTarget) $("#add-dialog").close();
  });
  $("#add-dialog").addEventListener("close", () => $("#add-peer-btn").focus());
  $("#add-form").addEventListener("submit", submitAdd);
  $("#add-delegate").addEventListener("change", syncDelegateFields);
  $("#add-access").addEventListener("change", syncDelegateFields);
  setInterval(() => {
    for (const span of document.querySelectorAll(".elapsed[data-start]")) {
      span.textContent = `running ${duration(Date.now() - Number(span.dataset.start))}`;
    }
  }, 1000);

  if (!token) {
    $("#feed").replaceChildren(el("div", { class: "feed-empty" },
      el("h2", { text: "Access token missing" }),
      el("p", { text: "Open the full link that p2pagents printed when it started (it ends with #t=…)." })));
    return;
  }
  loadState().then(connectEvents).catch((err) => toast(err.message, "error"));
}

init();
