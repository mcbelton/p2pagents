// Proofkin web UI: connect other agents. Agents on this computer (VS Code, Claude
// Code, any MCP or A2A client) hand work to this agent's peers with a token of their
// own, which works only for /mcp and /a2a. Uses app.js helpers: $, el, api, toast,
// confirmDialog and copy.
"use strict";

const connectState = { info: null, token: null, open: false };

async function loadConnect() {
  connectState.info = await api("/api/connect");
  renderConnect();
}

/// How to set up clients, with the token if it is shown (VS Code asks for it instead).
function connectSnippets(info, token) {
  const t = token ?? "<token>";
  return [
    ["VS Code (.vscode/mcp.json; it asks for the token)", JSON.stringify({
      inputs: [{ type: "promptString", id: "proofkin-token", description: "Proofkin integration token", password: true }],
      servers: { proofkin: { type: "http", url: info.mcp, headers: { Authorization: "Bearer ${input:proofkin-token}" } } },
    }, null, 2)],
    ["Claude Code", `claude mcp add --transport http proofkin ${info.mcp} --header "Authorization: Bearer ${t}"`],
    ["A2A clients", `Agent card: ${info.card}\nHeader: Authorization: Bearer ${t}\nHeader: A2A-Version: 1.0`],
  ];
}

function connectRow(label, value, what) {
  return el("div", { class: "connect-row" },
    el("span", { class: "connect-label", text: label }),
    el("code", { class: "mono connect-value", text: value }),
    el("button", { type: "button", class: "btn small", "aria-label": `Copy ${what}`, onclick: () => copy(value, what) }, "Copy"));
}

function renderConnect() {
  const info = connectState.info;
  if (!info) return;
  const box = $("#connect");
  const toggle = $("#connect-toggle");
  toggle.hidden = info.worker;
  toggle.textContent = info.on ? "Turn off" : "Turn on";
  if (info.worker) {
    box.replaceChildren(el("p", { class: "muted small", text: "A digital worker takes work from its owner only." }));
    return;
  }
  if (!info.on) {
    box.replaceChildren(el("p", { class: "muted small", text: "Let other agents on this computer (VS Code, Claude Code, any MCP or A2A client) hand work to your peers, with a token of their own. Off until you turn it on." }));
    return;
  }
  const token = connectState.token;
  const setup = el("details", { class: "connect-setup", open: connectState.open },
    el("summary", { text: "Set up a client" }),
    el("div", { class: "connect-actions" },
      token ? connectRow("Token", token, "token")
        : el("button", { type: "button", class: "btn small", onclick: () => revealConnectToken().catch((err) => toast(err.message, "error")) }, "Show token"),
      el("button", { type: "button", class: "btn small", onclick: () => newConnectToken().catch((err) => toast(err.message, "error")) }, "New token")),
    ...connectSnippets(info, token).map(([label, text]) => el("div", { class: "connect-snippet" },
      el("div", { class: "connect-snippet-head" },
        el("span", { class: "small", text: label }),
        el("button", { type: "button", class: "btn small", "aria-label": `Copy the ${label} setup`, onclick: () => copy(text, "Setup") }, "Copy")),
      el("pre", { class: "mono small", text }))));
  setup.addEventListener("toggle", () => { connectState.open = setup.open; });
  box.replaceChildren(...[
    el("p", { class: "muted small", text: "On: agents with the token run tasks and plans on your peers (never on this computer); their work shows up under Tasks." }),
    connectRow("MCP", info.mcp, "MCP address"),
    connectRow("A2A", info.card, "agent card address"),
    info.fixed_port ? null : el("p", { class: "small warn-text", text: `These addresses change when the agent restarts: start it with --ui-port ${info.port} to keep them.` }),
    setup,
  ].filter(Boolean));
}

async function revealConnectToken() {
  const info = await api("/api/connect?reveal=true");
  connectState.info = info;
  connectState.token = info.token;
  connectState.open = true;
  renderConnect();
}

async function newConnectToken() {
  const ok = await confirmDialog({
    title: "Make a new token?",
    body: "The current token stops working: clients that use it need the new one.",
    confirm: "Make a new token",
    danger: true,
  });
  if (!ok) return;
  connectState.info = await api("/api/connect", { method: "POST" });
  connectState.token = connectState.info.token;
  connectState.open = true;
  renderConnect();
  toast("New token made: give it to your clients");
}

async function toggleConnect() {
  if (connectState.info?.on) {
    const ok = await confirmDialog({
      title: "Stop other agents connecting?",
      body: "Agents that use the token can no longer hand work to your peers. Work they already started keeps running.",
      confirm: "Turn off",
      danger: true,
    });
    if (!ok) return;
    connectState.info = await api("/api/connect", { method: "DELETE" });
    connectState.token = null;
    toast("Other agents can no longer connect");
  } else {
    connectState.info = await api("/api/connect", { method: "POST" });
    connectState.token = connectState.info.token;
    connectState.open = true;
    toast("Other agents can connect: set up a client below");
  }
  renderConnect();
}

$("#connect-toggle").addEventListener("click", () => toggleConnect().catch((err) => toast(err.message, "error")));
