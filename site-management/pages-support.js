// management.pigeonpost.dev — Support: every live partner conversation in one inbox.
//
// Persistent HTTP for reads and writes; a WebSocket (/v1/support/live) only says "something
// changed", and the page re-reads. The same protocol as the partner side (site/partner-support.js):
//   GET   /v1/support/tickets?staff=true&status=&q=&assigned=&offset=
//   GET   /v1/support/tickets/{id}?staff=true[&before=]
//   POST  /v1/support/tickets/{id}/messages?staff=true   {body, client_id}
//   POST  /v1/support/tickets/{id}/read?staff=true       {last_seq}
//   PATCH /v1/support/tickets/{id}?staff=true            {status|priority|assignment}
//   WS    /v1/support/live → send {token, publisher: null, staff: true}; receive ready,
//         support.changed {ticket_id}, resync, auth_required.
(function () {
  "use strict";
  const PP = window.PP, M = window.PPM, ui = PP.ui;
  const el = (...a) => PP.el(...a);
  const enc = encodeURIComponent;
  const STATUS = { open: ["Awaiting support", "info"], waiting_partner: ["Awaiting partner", "warning"], resolved: ["Resolved", "success"] };
  const uuid = () => (window.crypto && window.crypto.randomUUID ? window.crypto.randomUUID() : PP.random(16));

  M.page({
    key: "support", title: "Support", group: "Support", icon: "support", allow: M.allow(["owner", "support"]),
    render: (view, ctx, [ticket]) => M.guard(view, () => supportInbox(view, ticket)),
  });

  M.supportSockets = [];

  function supportInbox(view, initial) {
    const st = { rows: [], next: null, selected: initial || null, detail: null, older: null, messages: new Map(), filter: "open", q: "", assigned: "all", active: true, ws: null, delay: 1000, timer: null, authBlocked: false, sending: false, listSeq: 0, openSeq: 0 };
    const drafts = new Map();
    const conn = el("span", { class: "pm-live", "data-state": "connecting", "aria-live": "polite" }, "Connecting…");
    view.append(ui.header({ eyebrow: "Partner success", title: "Support", text: "Every partner conversation, live. Replies are seen by the partner's whole team.", actions: conn }));
    const notice = el("div", { class: "pm-notice", hidden: true });
    const queue = el("div", { class: "pm-queue", role: "list", "aria-label": "Conversations" });
    const more = ui.button("Load more", { size: "sm", onclick: () => loadList(false) });
    more.hidden = true;
    const search = el("input", { class: "pp-input pp-search", type: "search", placeholder: "Search conversations or businesses", "aria-label": "Search conversations" });
    const assigned = el("select", { class: "pp-input pp-select", "aria-label": "Assignment", onchange: () => { st.assigned = assigned.value; loadList(true); } },
      [["all", "Everyone"], ["mine", "Assigned to me"], ["unassigned", "Unassigned"]].map(([v, t]) => el("option", { value: v }, t)));
    const side = el("aside", { class: "pm-inbox-side" },
      el("form", { class: "pm-inbox-search", role: "search", onsubmit: (e) => { e.preventDefault(); st.q = search.value.trim(); loadList(true); } }, el("label", { class: "pp-search-wrap" }, PP.icon("search"), search)),
      M.chips({ label: "Status", value: st.filter, options: [["open", "Awaiting support"], ["waiting_partner", "Awaiting partner"], ["resolved", "Resolved"], ["all", "All"]], onChange: (v) => { st.filter = v; loadList(true); } }),
      assigned, queue, more);
    const convo = el("section", { class: "pm-convo", "aria-label": "Conversation" });
    const layout = el("div", { class: "pm-inbox" + (st.selected ? " is-open" : "") }, side, convo);
    view.append(notice, layout);

    const path = (suffix = "", extra = {}) => `/v1/support/tickets${suffix}?${M.qs({ staff: "true", ...extra })}`;
    const write = (p, body, method = "POST") => M.api(p, { method, body });

    function problem(e) {
      if (e instanceof PP.Redirecting) return;
      notice.hidden = false;
      notice.replaceChildren(M.isStepUp(e) || st.authBlocked ? M.stepUpCard(e, { compact: true }) : ui.gate({ text: M.explain(e) || "Could not connect. Your draft is safe; try again.", tone: "danger", action: ui.button("Retry", { size: "sm", onclick: () => loadList(true) }) }));
    }

    async function loadList(reset = true) {
      if (!st.active) return;
      const seq = ++st.listSeq;
      if (reset && !st.rows.length) queue.replaceChildren(ui.loading(3));
      try {
        const r = await M.get(path("", { status: st.filter, q: st.q, assigned: st.assigned, offset: reset ? 0 : st.next || 0 }));
        if (!st.active || seq !== st.listSeq) return;
        st.rows = reset ? r.data || [] : st.rows.concat(r.data || []);
        st.next = r.next_offset ?? null;
        notice.hidden = true;
        drawQueue();
      } catch (e) { if (st.active && seq === st.listSeq) { queue.replaceChildren(); problem(e); } }
    }

    function drawQueue() {
      more.hidden = st.next == null;
      if (!st.rows.length) {
        queue.replaceChildren(ui.empty({ icon: "support", title: st.q || st.filter !== "open" ? "No matching conversations" : "Inbox clear", text: "New partner requests appear here as they arrive." }));
        return;
      }
      queue.replaceChildren(...st.rows.map((t) => {
        const [label, tone] = STATUS[t.status] || [t.status, "neutral"];
        return el("button", { type: "button", role: "listitem", class: `pm-ticket${t.id === st.selected ? " is-selected" : ""}${t.unread ? " is-unread" : ""}`, "aria-current": t.id === st.selected ? "true" : null, "data-ticket": t.id, onclick: () => select(t.id) },
          el("span", { class: "pm-ticket-top" }, el("span", { class: "pm-ticket-who" }, t.publisher_name || "Partner"), el("time", { datetime: t.updated_at }, M.time(t.updated_at))),
          el("strong", { class: "pm-ticket-subject" }, t.subject),
          el("span", { class: "pm-ticket-tags" }, ui.badge(label, tone), t.priority && t.priority !== "normal" ? ui.badge(t.priority, t.priority === "urgent" ? "danger" : "warning") : null, t.unread ? ui.badge("New", "info") : null));
      }));
    }

    function select(id) {
      window.history.replaceState(null, "", PP.href("support", id));
      open(id);
    }

    async function open(id, refresh = false) {
      const seq = ++st.openSeq;
      st.selected = id;
      layout.classList.add("is-open");
      if (!refresh) { st.detail = null; st.messages.clear(); st.older = null; drawConvo(); }
      drawQueue();
      try {
        const r = await M.get(path("/" + enc(id)));
        if (!st.active || seq !== st.openSeq || id !== st.selected) return;
        st.detail = r.data;
        for (const m of st.detail.messages || []) st.messages.set(m.seq, m);
        if (!refresh || st.older === null) st.older = st.detail.older_before ?? null;
        drawConvo();
        if (!document.hidden) await write(path("/" + enc(id) + "/read"), { last_seq: st.detail.last_seq });
        const row = st.rows.find((t) => t.id === id);
        if (row && row.unread) { row.unread = false; drawQueue(); }
      } catch (e) { if (st.active && seq === st.openSeq) problem(e); }
    }

    async function earlier() {
      if (!st.selected || st.older == null) return;
      const id = st.selected;
      try {
        const r = await M.get(path("/" + enc(id), { before: st.older }));
        if (id !== st.selected || !st.active) return;
        for (const m of r.data.messages || []) st.messages.set(m.seq, m);
        st.older = r.data.older_before ?? null;
        drawMessages(false);
      } catch (e) { problem(e); }
    }

    function drawMessages(stick = true) {
      const log = convo.querySelector(".pm-messages");
      if (!log) return;
      const near = log.scrollHeight - log.scrollTop - log.clientHeight < 120;
      log.replaceChildren();
      if (st.older != null) log.append(ui.button("Load earlier messages", { size: "sm", onclick: earlier }));
      for (const m of [...st.messages.values()].sort((a, b) => a.seq - b.seq)) {
        const ours = m.author_kind === "staff";
        log.append(el("article", { class: `pm-msg${ours ? " is-own" : ""}` },
          el("div", { class: "pm-msg-meta" }, el("strong", {}, ours ? "Pigeonpost support" : (st.detail && st.detail.publisher_name) || "Partner"), el("time", { datetime: m.created_at }, M.time(m.created_at))),
          el("p", {}, m.body)));
      }
      if (stick && near) log.scrollTop = log.scrollHeight;
    }

    function drawConvo() {
      const prior = convo.querySelector("textarea");
      const focused = prior && prior === document.activeElement;
      const back = ui.button("Conversations", { size: "sm", kind: "quiet", onclick: () => { layout.classList.remove("is-open"); st.selected = null; st.detail = null; window.history.replaceState(null, "", PP.href("support")); drawConvo(); drawQueue(); } });
      back.classList.add("pm-back");
      if (!st.selected) {
        convo.replaceChildren(ui.empty({ icon: "support", title: "Pick a conversation", text: "See the business, understand the issue, and reply. Live updates arrive on their own." }));
        return;
      }
      if (!st.detail) { convo.replaceChildren(back, ui.loading(4)); return; }
      const t = st.detail, id = t.id;
      const draft = drafts.get(id) || { body: "", client_id: uuid() };
      drafts.set(id, draft);
      const [label, tone] = STATUS[t.status] || [t.status, "neutral"];
      const priority = el("select", { class: "pp-input pp-select", "aria-label": "Priority", onchange: () => change({ priority: priority.value }) },
        ["normal", "high", "urgent"].map((v) => el("option", { value: v, selected: t.priority === v ? true : null }, `${v[0].toUpperCase()}${v.slice(1)} priority`)));
      const head = el("header", { class: "pm-convo-head" }, back,
        el("div", { class: "pm-convo-title" },
          el("p", { class: "pp-eyebrow" }, `${t.category || "general"} · ${t.assigned_to ? "assigned" : "unassigned"}`),
          el("h2", {}, t.subject),
          el("p", { class: "pp-muted" }, t.publisher_name || "", " ", ui.badge(label, tone))),
        el("div", { class: "pm-convo-actions" },
          priority,
          ui.button("Assign to me", { size: "sm", onclick: () => change({ assignment: "mine" }) }),
          t.assigned_to ? ui.button("Unassign", { size: "sm", onclick: () => change({ assignment: "unassigned" }) }) : null,
          ui.button(t.status === "resolved" ? "Reopen" : "Mark resolved", { size: "sm", kind: t.status === "resolved" ? "default" : "primary", onclick: () => change({ status: t.status === "resolved" ? "open" : "resolved" }) })));
      const context = el("aside", { class: "pm-convo-context" },
        el("span", {}, "Business status: ", M.status(t.publisher_status, (M.APP_STATES || {})[t.publisher_status])),
        t.publisher_id ? M.link("Open their application", "applications", t.publisher_id) : M.link("Applications", "applications"));
      const log = el("div", { class: "pm-messages", role: "log", "aria-live": "polite", "aria-label": "Messages" });
      const input = el("textarea", { class: "pp-input", rows: "3", maxlength: "8000", "aria-label": "Your reply", placeholder: "Write a helpful reply…" });
      input.value = draft.body;
      input.addEventListener("input", () => {
        if (draft.pending && draft.pending !== input.value) { draft.client_id = uuid(); draft.pending = null; }
        draft.body = input.value;
      });
      input.addEventListener("keydown", (e) => { if ((e.ctrlKey || e.metaKey) && e.key === "Enter") { e.preventDefault(); send(); } });
      const sendBtn = el("button", { class: "pp-btn pp-btn-primary", type: "submit", disabled: st.sending ? true : null }, st.sending ? "Sending…" : "Send reply");
      async function send() {
        if (st.sending || !input.value.trim()) return;
        const body = input.value.trim();
        draft.body = body; draft.pending = body;
        st.sending = true; sendBtn.disabled = true; sendBtn.textContent = "Sending…";
        try {
          await write(path("/" + enc(id) + "/messages"), { body, client_id: draft.client_id });
          if (draft.body === body) drafts.delete(id);
          if (st.selected === id) await open(id, true);
          await loadList(true);
        } catch (e) { problem(e); } finally {
          st.sending = false;
          if (st.selected === id) drawConvo();
        }
      }
      const composer = el("form", { class: "pm-composer", onsubmit: (e) => { e.preventDefault(); send(); } }, input,
        el("div", { class: "pm-composer-foot" }, el("span", { class: "pp-faint" }, "Seen by the partner's team. Ctrl/⌘ + Enter sends."), sendBtn));
      convo.replaceChildren(head, context, log, composer);
      drawMessages(true);
      log.scrollTop = log.scrollHeight;
      if (focused) input.focus();
    }

    async function change(body) {
      const id = st.selected;
      try {
        await write(path("/" + enc(id)), body, "PATCH");
        if (st.selected === id) await open(id, true);
        loadList(true);
      } catch (e) { problem(e); }
    }

    function connect() {
      if (!st.active || st.authBlocked || !window.WebSocket || !PP.token()) { if (!window.WebSocket) setConn("offline", "Live updates unavailable"); return; }
      setConn("connecting", "Connecting…");
      let socket;
      try {
        const u = new URL("/v1/support/live", (window.PP_CONFIG || {}).postbox || "https://postbox.pigeonpost.dev");
        u.protocol = u.protocol === "https:" ? "wss:" : "ws:";
        socket = st.ws = new window.WebSocket(u.toString());
        M.supportSockets.push(socket);
      } catch { setConn("offline", "Live updates unavailable"); return; }
      socket.onopen = () => { if (socket === st.ws) socket.send(JSON.stringify({ token: PP.token(), publisher: null, staff: true })); };
      socket.onmessage = (e) => {
        if (socket !== st.ws || !st.active) return;
        let v;
        try { v = JSON.parse(e.data); } catch { return; }
        if (v.type === "auth_required") { st.authBlocked = true; setConn("offline", "Confirm it's you for live updates"); loadList(true); socket.close(); return; }
        if (v.type === "ready") { st.delay = 1000; setConn("live", "Live"); loadList(true); if (st.selected) open(st.selected, true); }
        if (v.type === "support.changed" || v.type === "resync") {
          loadList(true);
          if (st.selected && (!v.ticket_id || v.ticket_id === st.selected)) open(st.selected, true);
        }
      };
      socket.onclose = () => {
        if (socket !== st.ws || !st.active || st.authBlocked) return;
        setConn("offline", "Reconnecting… your draft is safe");
        st.timer = setTimeout(connect, st.delay);
        st.delay = Math.min(st.delay * 2, 15000);
      };
      socket.onerror = () => socket.close();
    }
    function setConn(state, text) { conn.dataset.state = state; conn.replaceChildren(el("span", { class: "pm-live-dot", "aria-hidden": "true" }), text); }

    const visibility = () => { if (!document.hidden && st.active) { loadList(true); if (st.selected) open(st.selected, true); } };
    function destroy() {
      st.active = false;
      clearTimeout(st.timer);
      if (st.ws) { st.ws.onclose = null; try { st.ws.close(); } catch { /* closed */ } st.ws = null; }
      document.removeEventListener("visibilitychange", visibility);
      window.removeEventListener("hashchange", leave);
    }
    const leave = () => setTimeout(() => { if (!layout.isConnected) destroy(); }, 0);
    document.addEventListener("visibilitychange", visibility);
    window.addEventListener("hashchange", leave);

    drawConvo();
    loadList(true).then(() => { if (st.selected) open(st.selected); });
    connect();
  }
})();
