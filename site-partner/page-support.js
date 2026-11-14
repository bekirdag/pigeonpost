// Support: live conversations between a publisher's team and Pigeonpost. Writes go over HTTP
// (persisted first); a WebSocket only says "something changed", and the page re-reads.
//   GET|POST /v1/support/tickets?publisher={p}[&status&q&offset]     list / {subject, body, category, client_id}
//   GET      /v1/support/tickets/{t}?publisher={p}[&before]           one conversation, paged back
//   POST     /v1/support/tickets/{t}/messages?publisher={p}           {body, client_id}
//   POST     /v1/support/tickets/{t}/read?publisher={p}               {last_seq}
//   PATCH    /v1/support/tickets/{t}?publisher={p}                    {status}
//   WS       wss://postbox…/v1/support/live   first frame {token, publisher, staff:false};
//            server frames: ready, support.changed {ticket_id?}, resync, auth_required
(function () {
  "use strict";
  const PP = window.PP, PPP = window.PPP, ui = PP.ui;
  const el = (...a) => PP.el(...a);

  const LABELS = { open: ["Awaiting Pigeonpost", "info"], waiting_partner: ["Your reply needed", "warning"], resolved: ["Resolved", "success"] };
  const TOPICS = [["application", "Business application"], ["apps", "My apps"], ["billing", "Earnings & payouts"], ["technical", "Technical issue"], ["other", "Something else"]];
  const topic = (c) => (TOPICS.find(([k]) => k === c) || [c, c])[1];
  const key = () => (window.crypto && window.crypto.randomUUID ? window.crypto.randomUUID() : PP.random(16));
  const drafts = new Map();
  let newDraft = { subject: "", body: "", category: "application", client_id: key() };

  PPP.socketUrl = function () {
    const u = new URL("/v1/support/live", window.PP_CONFIG.postbox || "https://postbox.pigeonpost.dev");
    u.protocol = u.protocol === "https:" ? "wss:" : "ws:";
    return u.toString();
  };

  function mount(view, ctx, params) {
    const pid = ctx.pub.id;
    const st = { rows: [], next: null, filter: "all", q: "", selected: params[0] && params[0] !== "new" ? params[0] : null, detail: null, messages: new Map(), older: null, active: true, ws: null, timer: null, delay: 1000, blocked: false, composing: params[0] === "new" };
    const path = (suffix = "", extra = {}) => `/v1/support/tickets${suffix}?${new URLSearchParams({ publisher: pid, ...extra })}`;

    const live = el("span", { class: "ppp-live", "data-state": "connecting", "aria-live": "polite" }, "Connecting…");
    const notice = el("div", { class: "ppp-notice", role: "status", hidden: true });
    const queue = el("div", { class: "ppp-queue", role: "list", "aria-label": "Conversations" });
    const more = ui.button("Load more", { size: "sm", kind: "quiet", onclick: () => loadList(false) });
    more.hidden = true;
    const pane = el("section", { class: "ppp-thread", "aria-label": "Conversation" });
    const search = el("input", { class: "pp-input", type: "search", placeholder: "Search conversations", "aria-label": "Search conversations" });
    const filter = el("select", { class: "pp-input", "aria-label": "Filter by status" }, el("option", { value: "all" }, "All conversations"), Object.entries(LABELS).map(([k, v]) => el("option", { value: k }, v[0])));
    filter.addEventListener("change", () => { st.filter = filter.value; loadList(true); });
    const searchForm = el("form", { class: "ppp-queue-search" }, search);
    searchForm.addEventListener("submit", (e) => { e.preventDefault(); st.q = search.value.trim(); loadList(true); });
    const layout = el("div", { class: "ppp-support" + (st.selected || st.composing ? " is-open" : "") },
      el("aside", { class: "ppp-queue-pane" },
        el("div", { class: "ppp-queue-head" }, el("strong", {}, "Conversations"), ui.button("New", { kind: "primary", size: "sm", icon: "plus", onclick: () => compose() })),
        searchForm, filter, queue, more),
      pane);
    view.append(ui.header({ title: "Support", text: "Talk to the Pigeonpost team about your business, apps or payments. Replies arrive here instantly.", actions: [live, PPP.picker(ctx)] }), notice, layout);

    function fail(e) {
      if (e instanceof PP.Redirecting) return;
      notice.hidden = false;
      notice.replaceChildren(ui.gate({ tone: "danger", text: PP.explain(e) || "Could not reach support. Your draft is safe.", action: ui.button("Try again", { size: "sm", onclick: () => loadList(true) }) }));
    }

    async function loadList(reset = true) {
      if (!st.active) return;
      try {
        const r = await PP.postbox(path("", { status: st.filter, q: st.q, offset: reset ? 0 : st.next || 0 }));
        if (!st.active) return;
        st.rows = reset ? r.data || [] : st.rows.concat(r.data || []);
        st.next = r.next_offset ?? null;
        notice.hidden = true;
        drawQueue();
      } catch (e) { fail(e); }
    }

    function drawQueue() {
      more.hidden = st.next == null;
      if (!st.rows.length) {
        queue.replaceChildren(ui.empty({ icon: "support", title: st.q || st.filter !== "all" ? "No matching conversations" : "No conversations yet", text: "Start one whenever you need a hand." }));
        return;
      }
      queue.replaceChildren(...st.rows.map((t) => el("button", { type: "button", class: `ppp-ticket${t.id === st.selected ? " is-selected" : ""}${t.unread ? " is-unread" : ""}`, role: "listitem", "aria-current": t.id === st.selected ? "true" : null, "data-ticket": t.id, onclick: () => show(t.id) },
        el("div", { class: "ppp-ticket-top" }, el("span", {}, topic(t.category)), el("time", { datetime: t.updated_at }, ui.whenTime(t.updated_at))),
        el("strong", {}, t.subject),
        el("div", { class: "ppp-ticket-foot" }, ui.status(t.status, LABELS), t.unread ? ui.badge("New", "info") : null))));
    }

    /// Open a conversation without leaving the page (the live socket stays connected).
    function show(id) {
      window.history.replaceState(null, "", PP.href("support", id));
      open(id);
      drawQueue();
    }
    function showList() {
      st.selected = null; st.composing = false; st.detail = null;
      layout.classList.remove("is-open");
      window.history.replaceState(null, "", PP.href("support"));
      drawQueue();
      drawThread();
    }

    async function open(id, refresh = false) {
      st.selected = id;
      st.composing = false;
      layout.classList.add("is-open");
      if (!refresh) { st.detail = null; st.messages.clear(); st.older = null; drawThread(); }
      try {
        const r = await PP.postbox(path(`/${PPP.enc(id)}`));
        if (!st.active || st.selected !== id) return;
        st.detail = r.data;
        for (const m of st.detail.messages || []) st.messages.set(m.seq, m);
        if (!refresh || st.older === null) st.older = st.detail.older_before ?? null;
        drawThread();
        if (!document.hidden) {
          await PP.postbox(path(`/${PPP.enc(id)}/read`), { method: "POST", body: { last_seq: st.detail.last_seq } }).catch(() => {});
          const row = st.rows.find((t) => t.id === id);
          if (row && row.unread) { row.unread = false; drawQueue(); }
        }
      } catch (e) { fail(e); }
    }

    async function earlier() {
      if (!st.selected || st.older == null) return;
      const r = await PP.postbox(path(`/${PPP.enc(st.selected)}`, { before: st.older }));
      for (const m of (r.data && r.data.messages) || []) st.messages.set(m.seq, m);
      st.older = r.data.older_before ?? null;
      drawThread();
    }

    function drawThread() {
      const prior = pane.querySelector("textarea");
      const focused = prior && prior === document.activeElement;
      const back = ui.button("Conversations", { size: "sm", kind: "quiet", onclick: showList });
      back.classList.add("ppp-back");
      if (st.composing) return;
      if (!st.selected) {
        pane.replaceChildren(el("div", { class: "ppp-thread-empty" }, ui.empty({ icon: "support", title: "Real people, shared context", text: "Your conversations stay with your business, so your team can pick up where you left off.", action: { label: "Start a conversation", onclick: () => compose() } })));
        return;
      }
      if (!st.detail) { pane.replaceChildren(back, ui.loading(3)); return; }
      const t = st.detail, id = t.id;
      const draft = drafts.get(id) || { body: "", client_id: key() };
      drafts.set(id, draft);
      const log = el("div", { class: "ppp-messages", role: "log", "aria-live": "polite", "aria-label": "Messages" },
        st.older != null ? ui.button("Load earlier messages", { size: "sm", kind: "quiet", onclick: earlier }) : null,
        [...st.messages.values()].sort((a, b) => a.seq - b.seq).map((m) => {
          const ours = m.author_kind === "partner";
          return el("article", { class: `ppp-msg${ours ? " is-own" : ""}` },
            el("div", { class: "ppp-msg-meta" }, el("strong", {}, ours ? (m.author_name || t.publisher_name || "Your team") : "Pigeonpost support"), el("time", { datetime: m.created_at }, ui.whenTime(m.created_at))),
            el("p", {}, m.body));
        }));
      const input = el("textarea", { class: "pp-input", rows: 3, maxlength: 8000, "aria-label": "Your reply", placeholder: "Write your message…" });
      input.value = draft.body;
      input.addEventListener("input", () => { if (draft.pending && draft.pending !== input.value) { draft.client_id = key(); draft.pending = null; } draft.body = input.value; });
      const send = ui.button("Send", { kind: "primary", icon: "chevron", onclick: () => reply() });
      input.addEventListener("keydown", (e) => { if ((e.ctrlKey || e.metaKey) && e.key === "Enter") { e.preventDefault(); reply(); } });
      async function reply() {
        const body = input.value.trim();
        if (!body) return;
        draft.body = body; draft.pending = body;
        send.disabled = true;
        try {
          await PP.postbox(path(`/${PPP.enc(id)}/messages`), { method: "POST", body: { body, client_id: draft.client_id } });
          drafts.delete(id);
          await open(id, true);
          await loadList(true);
        } catch (e) { fail(e); } finally { if (send.isConnected) send.disabled = false; }
      }
      pane.replaceChildren(
        el("header", { class: "ppp-thread-head" }, back,
          el("div", {}, el("p", { class: "pp-eyebrow" }, topic(t.category)), el("h2", {}, t.subject), el("p", { class: "pp-muted" }, ui.status(t.status, LABELS), " ", t.publisher_name || "")),
          ui.button(t.status === "resolved" ? "Reopen" : "Mark resolved", { size: "sm", onclick: async () => {
            await PP.postbox(path(`/${PPP.enc(id)}`), { method: "PATCH", body: { status: t.status === "resolved" ? "open" : "resolved" } });
            await open(id, true); loadList(true);
          } })),
        log,
        el("form", { class: "ppp-composer", onsubmit: (e) => { e.preventDefault(); reply(); } }, input,
          el("div", { class: "ppp-composer-foot" }, el("span", { class: "pp-faint" }, "Seen by your team and Pigeonpost support. Ctrl/⌘ + Enter sends."), send)));
      log.scrollTop = log.scrollHeight;
      if (focused) input.focus();
    }

    function compose() {
      st.composing = true;
      st.selected = null;
      layout.classList.add("is-open");
      if (PP.parseRoute(window.location.hash).params[0] !== "new") window.history.replaceState(null, "", PP.href("support", "new"));
      drawQueue();
      const back = ui.button("Conversations", { size: "sm", kind: "quiet", onclick: showList });
      back.classList.add("ppp-back");
      const form = ui.form([
        { name: "subject", label: "Subject", required: true, maxlength: 140, wide: true, value: newDraft.subject, placeholder: "A short summary" },
        { name: "category", label: "Topic", type: "select", required: true, value: newDraft.category, options: TOPICS },
        { name: "body", label: "Message", type: "textarea", rows: 8, maxlength: 8000, required: true, wide: true, value: newDraft.body, hint: "Please don't include passwords, card numbers or API keys." },
      ], { submit: "Send to Pigeonpost", onSubmit: async (v) => {
        const r = await PP.postbox(path(), { method: "POST", body: { subject: v.subject, body: v.body, category: v.category, client_id: newDraft.client_id } });
        newDraft = { subject: "", body: "", category: "application", client_id: key() };
        await loadList(true);
        show(r.data.id);
      } });
      for (const n of ["subject", "body", "category"]) form.inputs[n].addEventListener("input", () => { newDraft[n] = form.inputs[n].value; });
      form.inputs.category.addEventListener("change", () => { newDraft.category = form.inputs.category.value; });
      pane.replaceChildren(el("header", { class: "ppp-thread-head" }, back, el("div", {}, el("p", { class: "pp-eyebrow" }, "We're here to help"), el("h2", {}, "New conversation"))), el("div", { class: "ppp-compose" }, form));
      form.inputs.subject.focus();
    }

    function setLive(state, text) { live.dataset.state = state; live.textContent = text; }
    function connect() {
      if (!st.active || st.blocked || !window.WebSocket || !PP.token()) { if (!window.WebSocket) setLive("offline", "Live updates unavailable"); return; }
      setLive("connecting", "Connecting…");
      let socket;
      try { socket = st.ws = new window.WebSocket(PPP.socketUrl()); } catch { setLive("offline", "Live updates unavailable"); return; }
      socket.onopen = () => { if (socket === st.ws) socket.send(JSON.stringify({ token: PP.token(), publisher: pid, staff: false })); };
      socket.onmessage = (e) => {
        if (socket !== st.ws || !st.active) return;
        let v; try { v = JSON.parse(e.data); } catch { return; }
        if (v.type === "auth_required") { st.blocked = true; setLive("offline", "Sign in again for live updates"); socket.close(); return; }
        if (v.type === "ready") { st.delay = 1000; setLive("live", "Live"); loadList(true); if (st.selected) open(st.selected, true); }
        if (v.type === "support.changed" || v.type === "resync") { loadList(true); if (st.selected && (!v.ticket_id || v.ticket_id === st.selected)) open(st.selected, true); }
      };
      socket.onclose = () => {
        if (socket !== st.ws || !st.active || st.blocked) return;
        setLive("offline", "Reconnecting… your draft is safe");
        st.timer = window.setTimeout(connect, st.delay);
        st.delay = Math.min(st.delay * 2, 15000);
      };
      socket.onerror = () => socket.close();
    }
    const onVisible = () => { if (!document.hidden && st.active) { loadList(true); if (st.selected) open(st.selected, true); } };
    document.addEventListener("visibilitychange", onVisible);
    PPP.onLeave(() => {
      st.active = false;
      window.clearTimeout(st.timer);
      document.removeEventListener("visibilitychange", onVisible);
      if (st.ws) { st.ws.onclose = null; try { st.ws.close(); } catch { /* already closed */ } st.ws = null; }
    });
    PPP.supportState = st;

    loadList(true).then(() => { if (st.selected) open(st.selected); else if (st.composing) compose(); else drawThread(); });
    drawThread();
    connect();
  }

  PPP.page({
    key: "support", title: "Support", group: "Help", icon: "support", order: 7,
    async render(view, ctx, params) {
      if (!ctx.pub) {
        view.append(ui.header({ title: "Support" }), ui.empty({ icon: "support", title: "Start your application to talk to the partner team", text: "Partner support is for businesses with an application, even before it is approved. For anything else, use help.pigeonpost.dev.", action: { label: "Start your application", href: PP.href("overview") } }));
        return;
      }
      mount(view, ctx, params);
    },
  });
})();
