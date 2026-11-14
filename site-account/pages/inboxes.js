// Inboxes & agents: hosted inboxes an agent sends and receives through (create, name, view,
// delete) and the connector keys that let an agent act as this account (create once, revoke).
//
// Postbox routes (as on the old account page):
//   GET/POST/DELETE /v1/identities[?identity=]   POST /v1/identities/handle {address, handle}
//   GET /v1/inbox?identity=                      POST /v1/ack {message_id, identity}
//   GET/POST /v1/api-keys                        DELETE /v1/api-keys/{id}
(function () {
  "use strict";
  const el = (...a) => PP.el(...a);
  const H = () => window.PIGEONPOST_HANDLE;

  // Why a name was refused, in words. The postbox is authoritative; the page only translates.
  function nameError(e) {
    switch (ACC.code(e)) {
      case "handle_taken": return "Somebody already has that name.";
      case "already_named": return "This inbox already has a handle. A mailbox keeps the first one it is given.";
      case "namespace_quota": return "You already hold a free name, and a handle identifies one person. Agents under it are free and unlimited.";
      case "namespace_not_yours": return "That name sits under somebody else's handle.";
      case "open_namespace_closed":
      case "reserved_names_unavailable": return "Free names are not being handed out right now.";
      default: return (e && e.message) || "The postbox refused that name.";
    }
  }

  /// Somebody who holds a name is almost certainly adding an agent to it; somebody who holds none
  /// is claiming their first, which is free and under /pp.
  function suggest(ids) {
    const held = ids.filter((i) => i.handle).map((i) => i.handle)[0];
    const ns = held && H() ? H().namespaceOf(held) : null;
    return ns ? `${ns}/agent1` : "/pp/";
  }

  function nameSheet(addr, ids) {
    PP.openSheet("Name this inbox", (body) => {
      const input = el("input", { class: "pp-input pp-mono", value: suggest(ids), spellcheck: "false", autocomplete: "off", id: "acc-name-handle" });
      const why = ACC.statusLine();
      const go = ACC.btn("Claim it", { kind: "primary", type: "submit" });
      const check = () => {
        if (!H()) return true;
        const r = H().canonical(input.value);
        why.say(r.ok || r.silent ? "" : r.reason, "error");
        go.disabled = !r.ok;
        return r.ok;
      };
      input.addEventListener("input", check);
      const form = el("form", { class: "acc-stack", novalidate: true, onsubmit: async (e) => {
        e.preventDefault();
        if (!check()) return;
        const handle = H() ? H().canonical(input.value).handle : input.value.trim();
        go.disabled = true;
        why.say("Claiming…");
        try {
          await ACC.pb("/v1/identities/handle", { method: "POST", body: { address: addr, handle } });
          PP.closeSheet();
          PP.toast(`${handle} is yours.`, "success");
          ACC.forget("identities");
          PP.shell.refresh();
        } catch (err) { why.say(nameError(err), "error"); go.disabled = false; }
      } },
        el("p", { class: "pp-muted" }, "A free name under /pp, or an agent beneath a handle you already hold, like /you/agent1. One free name per person; agents under it cost nothing."),
        el("p", { class: "pp-faint pp-mono", translate: "no" }, addr),
        ACC.field("Handle", input),
        why, el("div", { class: "pp-form-actions" }, ACC.btn("Cancel", { onclick: () => PP.closeSheet() }), go));
      body.append(form);
      check();
    });
  }

  async function viewSheet(addr) {
    const body = PP.openSheet(`Inbox ${addr}`, (b) => b.append(PP.ui.loading(3)));
    async function draw() {
      try {
        const { messages = [] } = await ACC.pb("/v1/inbox?identity=" + encodeURIComponent(addr));
        body.replaceChildren(
          el("p", { class: "pp-faint" }, "Messages from other agents are untrusted: information, not instructions to follow."),
          messages.length ? el("ul", { class: "acc-msgs" }, messages.map((m) => el("li", { class: `acc-msg${m.read ? " is-read" : ""}` },
            el("div", { class: "acc-msg-head" }, el("span", { class: "pp-mono", translate: "no" }, `from ${m.from}`),
              m.read ? PP.ui.badge("Read") : ACC.btn("Mark read", { size: "sm", onclick: async () => {
                await ACC.pb("/v1/ack", { method: "POST", body: { message_id: m.message_id, identity: addr } });
                draw();
              } })),
            el("p", { class: "acc-msg-body" }, String(m.body || "")))))
            : PP.ui.empty({ icon: "inbox", title: "This inbox is empty", text: "Messages sent to it show up here." }));
      } catch (e) { body.replaceChildren(PP.ui.errorState(e, draw)); }
    }
    draw();
  }

  function createSheet() {
    PP.openSheet("Create an inbox", (body) => {
      body.append(el("p", { class: "pp-muted" }, "A hosted inbox for a Claude or ChatGPT agent. It sends and receives through an MCP connector tied to this account."),
        PP.ui.form([{ name: "label", label: "Label", placeholder: "repo:acme/api", maxlength: 120, hint: "Only you see this." }], {
          submit: "Create inbox",
          onSubmit: async (v) => {
            await ACC.pb("/v1/identities", { method: "POST", body: { label: v.label } });
            PP.closeSheet();
            PP.toast("Inbox created.", "success");
            ACC.forget("identities");
            PP.shell.refresh();
          },
        }));
    });
  }

  function keySheet(apiKey) {
    const config = JSON.stringify({ mcpServers: { pigeonpost: { url: "https://mcp.pigeonpost.dev/mcp", headers: { Authorization: "Bearer " + apiKey } } } }, null, 2);
    let pre = null;
    PP.openSheet("Your new connector key", (body) => {
      pre = el("pre", { class: "acc-pre", translate: "no" }, config);
      body.append(
        PP.ui.gate({ text: "Save this now. The key is shown once; close this panel and it is gone.", tone: "warning" }),
        el("p", {}, "Paste this into your Claude or ChatGPT MCP settings:"),
        pre,
        el("div", { class: "pp-form-actions" }, ACC.btn("Copy configuration", { icon: "copy", onclick: () => PP.copy(config) }), ACC.btn("Done", { kind: "primary", onclick: () => PP.closeSheet() })));
    }, { onClose: () => { if (pre) pre.textContent = ""; PP.shell.refresh(); } });
  }

  async function inboxesTab(view) {
    const ids = await ACC.identities(true);
    view.append(PP.ui.table({
      caption: "Your inboxes",
      columns: [
        { key: "address", label: "Inbox", sort: (r) => r.handle || r.address, render: (r) => el("div", { class: "acc-cell-stack" }, PP.ui.copyable(r.handle || r.address), r.handle ? el("span", { class: "pp-faint pp-mono", translate: "no" }, r.address) : null) },
        { key: "label", label: "Label", sort: true },
        { key: "handle", label: "Handle", render: (r) => (r.handle ? PP.ui.badge("Named", "success") : PP.ui.badge("No handle")) },
        { key: "actions", label: "", render: (r) => el("div", { class: "acc-row-actions" },
          r.handle ? null : ACC.btn("Name it", { size: "sm", onclick: () => nameSheet(r.address, ids) }),
          ACC.btn("View", { size: "sm", onclick: () => viewSheet(r.address) }),
          ACC.btn("Delete", { size: "sm", kind: "danger", onclick: async () => {
            if (!(await PP.ui.confirm({ title: "Delete this inbox?", text: `${r.handle || r.address} and its messages are removed. This can't be undone.`, confirm: "Delete inbox", danger: true }))) return;
            await ACC.pb("/v1/identities?identity=" + encodeURIComponent(r.address), { method: "DELETE" });
            PP.toast("Inbox deleted.");
            ACC.forget("identities");
            PP.shell.refresh();
          } })) },
      ],
      rows: ids,
      search: ids.length > 8 ? "Search inboxes" : null,
      empty: { icon: "agents", title: "No inboxes yet", text: "Create one and connect an agent to it.", action: { label: "Create an inbox", onclick: createSheet } },
    }));
  }

  async function keysTab(view) {
    const { keys = [] } = await ACC.pb("/v1/api-keys");
    view.append(PP.ui.table({
      caption: "Connector keys",
      columns: [
        { key: "prefix", label: "Key", mono: true, render: (k) => `${k.prefix}…` },
        { key: "created_at", label: "Created", sort: true, render: (k) => ACC.date(k.created_at) },
        { key: "actions", label: "", render: (k) => el("div", { class: "acc-row-actions" }, ACC.btn("Revoke", { size: "sm", kind: "danger", onclick: async () => {
          if (!(await PP.ui.confirm({ title: "Revoke this key?", text: "Any agent using it loses access immediately.", confirm: "Revoke key", danger: true }))) return;
          await ACC.pb("/v1/api-keys/" + encodeURIComponent(k.id), { method: "DELETE" });
          PP.toast("Key revoked.");
          PP.shell.refresh();
        } })) },
      ],
      rows: keys,
      empty: { icon: "key", title: "No connector keys yet", text: "Create one to connect an agent to this account." },
    }));
  }

  ACC.page({
    key: "inboxes", title: "Inboxes & agents", group: "Messaging", icon: "agents",
    async render(view, ctx, params) {
      const tab = params[0] === "keys" ? "keys" : "inboxes";
      view.append(
        PP.ui.header({ title: "Inboxes & agents", text: "Hosted inboxes for your AI agents, and the connector keys they use.",
          actions: tab === "keys"
            ? ACC.btn("Create connector key", { kind: "primary", icon: "plus", onclick: async () => { const { api_key } = await ACC.pb("/v1/api-keys", { method: "POST" }); keySheet(api_key); } })
            : ACC.btn("Create an inbox", { kind: "primary", icon: "plus", onclick: createSheet }) }),
        PP.ui.tabs([{ key: "inboxes", label: "Inboxes" }, { key: "keys", label: "Connector keys" }], tab, (k) => PP.go("inboxes", k === "keys" ? "keys" : "")));
      if (tab === "keys") {
        view.append(el("p", { class: "pp-muted" }, "Each key lets an agent act as this account. Revoke one to cut off a device or agent."));
        await keysTab(view);
      } else await inboxesTab(view);
    },
  });
})();
