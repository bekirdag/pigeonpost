// Publisher/team support. Persistent HTTP writes, authenticated WebSocket invalidations.
(function () {
  "use strict";
  const MP = (window.PPMarketplace = window.PPMarketplace || {});
  const el = (tag, props, ...children) => {
    const n = document.createElement(tag);
    for (const [k, v] of Object.entries(props || {})) {
      if (v == null || v === false) continue;
      if (k === "text") n.textContent = v;
      else if (k === "class") n.className = v;
      else if (k.startsWith("on")) n.addEventListener(k.slice(2), v);
      else n.setAttribute(k, v === true ? "" : String(v));
    }
    children
      .flat()
      .filter((v) => v != null)
      .forEach((v) => n.append(v));
    return n;
  };
  const labels = {
    open: "Awaiting support",
    waiting_partner: "Your reply needed",
    resolved: "Resolved",
  };
  const date = (v) =>
    new Date(v).toLocaleString(undefined, {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  const key = () => crypto.randomUUID();
  MP.partnerSupport = function (root, deps, options = {}) {
    const staff = !!options.staff;
    let publisher = "",
      publishers = [],
      rows = [],
      next = null,
      selected = null,
      detail = null,
      older = null,
      seq = 0,
      active = true,
      sending = false,
      ws = null,
      reconnect = null,
      delay = 1000,
      authBlocked = false,
      listGeneration = 0;
    let filter = "all",
      query = "",
      assignment = "all",
      newDraft = {
        subject: "",
        body: "",
        category: "application",
        client_id: key(),
      };
    const drafts = new Map(),
      messageMap = new Map();
    root.classList.add("support-root");
    const notice = el("div", {
      class: "ws-notice",
      role: "status",
      "aria-live": "polite",
      hidden: true,
    });
    const connection = el("span", {
      class: "ws-connection",
      "aria-live": "polite",
      text: "Connecting…",
    });
    const publisherSelect = el("select", {
      "aria-label": "Business",
      onchange: () => {
        publisher = publisherSelect.value;
        selected = null;
        detail = null;
        messageMap.clear();
        drawConversation();
        restartSocket();
        loadList(true);
      },
    });
    const search = el("input", {
      type: "search",
      placeholder: staff
        ? "Search conversations or businesses"
        : "Search your conversations",
      "aria-label": "Search conversations",
      value: "",
    });
    const status = el(
      "select",
      {
        "aria-label": "Filter by status",
        onchange: () => {
          filter = status.value;
          loadList(true);
        },
      },
      ["all", "open", "waiting_partner", "resolved"].map((v) =>
        el("option", {
          value: v,
          text:
            v === "all"
              ? "All conversations"
              : v === "waiting_partner" && staff
                ? "Awaiting partner"
                : labels[v],
        }),
      ),
    );
    const assigned = el(
      "select",
      {
        "aria-label": "Filter by assignment",
        onchange: () => {
          assignment = assigned.value;
          loadList(true);
        },
      },
      [
        ["all", "Everyone"],
        ["mine", "Assigned to me"],
        ["unassigned", "Unassigned"],
      ].map(([value, text]) => el("option", { value, text })),
    );
    const queue = el("div", {
      class: "ws-queue",
      role: "list",
      "aria-label": "Support conversations",
    });
    const more = el("button", {
      class: "btn btn-secondary",
      type: "button",
      hidden: true,
      text: "Load more",
      onclick: () => loadList(false),
    });
    const conversation = el("section", {
      class: "ws-conversation",
      "aria-label": "Conversation",
    });
    const back = el("button", {
      type: "button",
      class: "btn btn-small ws-back",
      text: "← Conversations",
      onclick: () => {
        root.classList.remove("ws-open");
        selected = null;
        detail = null;
        drawConversation();
      },
    });
    const toolbar = el(
      "form",
      {
        class: "ws-search",
        onsubmit: (e) => {
          e.preventDefault();
          query = search.value.trim();
          loadList(true);
        },
      },
      search,
      el("button", { type: "submit", class: "btn btn-small", text: "Search" }),
    );
    const sidebar = el(
      "aside",
      { class: "ws-sidebar" },
      el(
        "div",
        { class: "ws-sidebar-head" },
        el("strong", { text: staff ? "Partner inbox" : "Your conversations" }),
        staff
          ? null
          : el("button", {
              type: "button",
              class: "btn btn-primary",
              text: "+ New conversation",
              onclick: () => {
                selected = null;
                detail = null;
                root.classList.add("ws-open");
                drawNew();
              },
            }),
      ),
      toolbar,
      el("div", { class: "ws-filters" }, status, staff ? assigned : null),
      queue,
      more,
    );
    root.replaceChildren(
      ...[
        el(
          "header",
          { class: "workspace-heading" },
          el(
            "div",
            {},
            el("p", {
              class: "workspace-eyebrow",
              text: staff ? "Partner success" : "Help when you need it",
            }),
            el("h2", { text: staff ? "Support inbox" : "Support" }),
            el("p", {
              text: staff
                ? "One place to help partners move forward."
                : "Talk to the Pigeonpost team about your business, apps, or payments.",
            }),
          ),
          connection,
        ),
        notice,
        staff
          ? null
          : el(
              "div",
              { class: "ws-business" },
              el("label", {}, "Business", publisherSelect),
            ),
        el("div", { class: "ws-layout" }, sidebar, conversation),
      ].filter((v) => v != null),
    );
    function scope(extra = {}) {
      return new URLSearchParams({
        ...(!staff ? { publisher } : { staff: "true" }),
        ...extra,
      }).toString();
    }
    function path(suffix = "", extra = {}) {
      return "/v1/support/tickets" + suffix + "?" + scope(extra);
    }
    const write = (p, body, method = "POST") =>
      deps.pb(p, { method, body: JSON.stringify(body) });
    function error(e) {
      const signin =
        e.code === "step_up_required" || e.status === 401 || authBlocked;
      notice.hidden = false;
      notice.replaceChildren(
        el("span", {
          text: signin
            ? "Verify your sign-in to access partner support."
            : e.message ||
              "Could not connect. Your draft is safe; please try again.",
        }),
        signin
          ? el("button", {
              type: "button",
              class: "btn btn-small",
              text: "Verify sign-in",
              onclick: () =>
                deps.stepUp?.(e.body?.step_up || e.body || e, "support"),
            })
          : el("button", {
              type: "button",
              class: "btn btn-small",
              text: "Retry",
              onclick: () => loadList(true),
            }),
      );
    }
    async function loadList(reset = true) {
      if (!active || (!staff && !publisher)) return;
      const generation = ++listGeneration;
      try {
        const r = await deps.pb(
          path("", {
            status: filter,
            q: query,
            assigned: assignment,
            offset: reset ? 0 : next || 0,
          }),
        );
        if (!active || generation !== listGeneration) return;
        rows = reset ? r.data : rows.concat(r.data);
        next = r.next_offset;
        drawQueue();
        notice.hidden = true;
      } catch (e) {
        if (active && generation === listGeneration) error(e);
      }
    }
    function drawQueue() {
      queue.replaceChildren();
      more.hidden = next == null;
      if (!rows.length) {
        queue.append(
          el(
            "div",
            { class: "ws-queue-empty" },
            el("span", { class: "workspace-symbol", text: "↗" }),
            el("strong", {
              text:
                query || filter !== "all"
                  ? "No matching conversations"
                  : "A clear inbox",
            }),
            el("p", {
              text: staff
                ? "New partner requests will appear here."
                : "Start a conversation whenever you need a hand.",
            }),
          ),
        );
        return;
      }
      rows.forEach((t) =>
        queue.append(
          el(
            "button",
            {
              type: "button",
              class:
                "ws-ticket" +
                (t.id === selected ? " is-selected" : "") +
                (t.unread ? " is-unread" : ""),
              "aria-current": t.id === selected ? "true" : null,
              onclick: () => open(t.id),
            },
            el(
              "div",
              { class: "ws-ticket-top" },
              el("span", { text: staff ? t.publisher_name : t.category }),
              el("time", { datetime: t.updated_at, text: date(t.updated_at) }),
            ),
            el("strong", { text: t.subject }),
            el(
              "div",
              { class: "ws-ticket-bottom" },
              el("span", {
                class:
                  "workspace-badge tone-" +
                  (t.status === "resolved"
                    ? "success"
                    : t.status === "open"
                      ? "info"
                      : "warning"),
                text:
                  t.status === "waiting_partner" && staff
                    ? "Awaiting partner"
                    : labels[t.status],
              }),
              t.priority !== "normal"
                ? el("span", { class: "ws-priority", text: t.priority })
                : null,
              t.unread ? el("span", { class: "ws-unread", text: "New" }) : null,
            ),
          ),
        ),
      );
    }
    async function open(id, refresh = false) {
      const generation = ++seq;
      selected = id;
      root.classList.add("ws-open");
      if (!refresh) {
        detail = null;
        messageMap.clear();
        older = null;
        drawConversation();
      }
      drawQueue();
      try {
        const r = await deps.pb(path("/" + encodeURIComponent(id)));
        if (!active || generation !== seq || id !== selected) return;
        detail = r.data;
        for (const m of detail.messages) messageMap.set(m.seq, m);
        if (!refresh || older === null) older = detail.older_before;
        drawConversation();
        if (!document.hidden)
          await write(path("/" + encodeURIComponent(id) + "/read"), {
            last_seq: detail.last_seq,
          });
        const row = rows.find((t) => t.id === id);
        if (row) row.unread = false;
        drawQueue();
      } catch (e) {
        if (active && generation === seq) error(e);
      }
    }
    async function history() {
      if (!selected || older == null) return;
      const id = selected;
      try {
        const r = await deps.pb(
          path("/" + encodeURIComponent(id), { before: older }),
        );
        if (id !== selected || !active) return;
        for (const m of r.data.messages) messageMap.set(m.seq, m);
        older = r.data.older_before;
        drawMessages(false);
      } catch (e) {
        error(e);
      }
    }
    function drawMessages(bottom = true) {
      const log = conversation.querySelector(".ws-messages");
      if (!log) return;
      const nearBottom =
        log.scrollHeight - log.scrollTop - log.clientHeight < 120;
      log.replaceChildren();
      if (older != null)
        log.append(
          el("button", {
            type: "button",
            class: "btn btn-small",
            text: "Load earlier messages",
            onclick: history,
          }),
        );
      for (const m of [...messageMap.values()].sort((a, b) => a.seq - b.seq)) {
        const ours = m.author_kind === (staff ? "staff" : "partner");
        log.append(
          el(
            "article",
            { class: "ws-message" + (ours ? " ws-message-own" : "") },
            el(
              "div",
              { class: "ws-message-meta" },
              el("strong", {
                text:
                  m.author_kind === "staff"
                    ? "Pigeonpost support"
                    : detail.publisher_name,
              }),
              el("time", { datetime: m.created_at, text: date(m.created_at) }),
            ),
            el("p", { text: m.body }),
          ),
        );
      }
      if (bottom && nearBottom) log.scrollTop = log.scrollHeight;
    }
    function drawConversation() {
      setTimeout(size,0);
      const previousLog = conversation.querySelector(".ws-messages");
      const previousScroll = previousLog?.scrollTop;
      const wasBottom =
        !previousLog ||
        previousLog.scrollHeight -
          previousLog.scrollTop -
          previousLog.clientHeight <
          120;
      const prior = conversation.querySelector("textarea");
      const focused = prior === document.activeElement;
      const position = prior?.selectionStart;
      conversation.replaceChildren();
      if (!selected) {
        conversation.append(
          el(
            "div",
            { class: "ws-welcome" },
            el("div", { class: "workspace-symbol", text: "↗" }),
            el("p", {
              class: "workspace-eyebrow",
              text: "Real people. Shared context.",
            }),
            el("h3", {
              text: staff
                ? "Good support starts here."
                : "Let’s move things forward.",
            }),
            el("p", {
              text: staff
                ? "Choose a conversation to see the business, understand the issue, and reply."
                : "Your conversations stay with your business, so your team can pick up where you left off.",
            }),
            !staff
              ? el("button", {
                  type: "button",
                  class: "btn btn-primary",
                  text: "Start a conversation",
                  onclick: drawNew,
                })
              : null,
          ),
        );
        return;
      }
      if (!detail) {
        conversation.append(
          back,
          el("p", { class: "ws-loading", text: "Loading conversation…" }),
        );
        return;
      }
      const t = detail,
        id = t.id,
        draft = drafts.get(id) || { body: "", client_id: key() };
      drafts.set(id, draft);
      const actions = el(
        "div",
        { class: "ws-ticket-actions" },
        el("button", {
          type: "button",
          class: "btn btn-small",
          text:
            t.status === "resolved" ? "Reopen conversation" : "Mark resolved",
          onclick: () =>
            change({ status: t.status === "resolved" ? "open" : "resolved" }),
        }),
      );
      if (staff) {
        const priority = el(
          "select",
          {
            "aria-label": "Priority",
            onchange: () => change({ priority: priority.value }),
          },
          ["normal", "high", "urgent"].map((v) =>
            el("option", {
              value: v,
              selected: t.priority === v,
              text: v[0].toUpperCase() + v.slice(1) + " priority",
            }),
          ),
        );
        actions.append(
          ...[
            priority,
            el("button", {
              type: "button",
              class: "btn btn-small",
              text: "Assign to me",
              onclick: () => change({ assignment: "mine" }),
            }),
            t.assigned_to
              ? el("button", {
                  type: "button",
                  class: "btn btn-small",
                  text: "Unassign",
                  onclick: () => change({ assignment: "unassigned" }),
                })
              : null,
          ].filter((v) => v != null),
        );
      }
      conversation.append(
        el(
          "header",
          { class: "ws-conversation-head" },
          back,
          el(
            "div",
            {},
            el("p", {
              class: "workspace-eyebrow",
              text:
                t.category +
                " · " +
                (t.assigned_to ? "Assigned" : "Unassigned"),
            }),
            el("h3", { text: t.subject }),
            el("p", {
              text:
                t.publisher_name +
                " · " +
                (t.status === "waiting_partner" && staff
                  ? "Awaiting partner"
                  : labels[t.status]),
            }),
          ),
          actions,
        ),
        el(
          "aside",
          { class: "ws-context" },
          el("span", { text: "Business status: " + t.publisher_status }),
          el("a", {
            href: staff ? "#publishers" : "#business",
            text: staff ? "View applications →" : "My business →",
          }),
        ),
        el("div", {
          class: "ws-messages",
          role: "log",
          "aria-live": "polite",
          "aria-label": "Messages",
        }),
      );
      const input = el("textarea", {
        rows: "3",
        maxlength: "8000",
        "aria-label": "Your reply",
        placeholder: staff ? "Write a helpful reply…" : "Write your message…",
        oninput: () => {
          if (draft.pendingBody && draft.pendingBody !== input.value) {
            draft.client_id = key();
            draft.pendingBody = null;
          }
          draft.body = input.value;
        },
        onkeydown: (e) => {
          if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
            e.preventDefault();
            send();
          }
        },
      });
      input.value = draft.body;
      const sendButton = el("button", {
        type: "submit",
        class: "btn btn-primary",
        disabled: sending,
        text: sending ? "Sending…" : "Send reply →",
      });
      async function send() {
        if (sending || !input.value.trim()) return;
        const body = input.value.trim();
        draft.body = body;
        draft.pendingBody = body;
        sending = true;
        sendButton.disabled = true;
        sendButton.textContent = "Sending…";
        try {
          await write(path("/" + encodeURIComponent(id) + "/messages"), {
            body,
            client_id: draft.client_id,
          });
          if (draft.body === body) drafts.delete(id);
          if (selected === id) await open(id, true);
          await loadList(true);
        } catch (e) {
          error(e);
        } finally {
          sending = false;
          if (selected === id) drawConversation();
        }
      }
      conversation.append(
        el(
          "form",
          {
            class: "ws-composer",
            onsubmit: (e) => {
              e.preventDefault();
              send();
            },
          },
          input,
          el(
            "div",
            { class: "ws-compose-actions" },
            el("span", {
              text: "Visible to your business team and Pigeonpost support. Ctrl/⌘ + Enter to send.",
            }),
            sendButton,
          ),
        ),
      );
      drawMessages(wasBottom);
      const currentLog = conversation.querySelector(".ws-messages");
      if (currentLog && !wasBottom) currentLog.scrollTop = previousScroll;
      if (focused) {
        input.focus();
        input.setSelectionRange(position, position);
      }
    }
    async function change(body) {
      const id = selected;
      try {
        await write(path("/" + encodeURIComponent(id)), body, "PATCH");
        if (selected === id) await open(id, true);
        loadList(true);
      } catch (e) {
        error(e);
      }
    }
    function drawNew() {
      root.classList.add("ws-open");
      conversation.replaceChildren(
        back,
        el(
          "header",
          { class: "ws-conversation-head" },
          el(
            "div",
            {},
            el("p", { class: "workspace-eyebrow", text: "We’re here to help" }),
            el("h3", { text: "Start a conversation" }),
            el("p", {
              text: "Tell us what you need and what you have already tried.",
            }),
          ),
        ),
      );
      const form = el("form", { class: "ws-new-form" }),
        subject = el("input", {
          required: true,
          maxlength: "140",
          placeholder: "A short summary",
          value: newDraft.subject,
          oninput: (e) => (newDraft.subject = e.target.value),
        }),
        category = el(
          "select",
          { onchange: (e) => (newDraft.category = e.target.value) },
          [
            ["application", "Business application"],
            ["apps", "My apps"],
            ["billing", "Earnings & payouts"],
            ["technical", "Technical issue"],
            ["other", "Something else"],
          ].map(([v, t]) =>
            el("option", {
              value: v,
              selected: v === newDraft.category,
              text: t,
            }),
          ),
        ),
        body = el("textarea", {
          required: true,
          rows: "8",
          maxlength: "8000",
          placeholder: "Describe your question…",
          oninput: (e) => (newDraft.body = e.target.value),
        });
      body.value = newDraft.body;
      const button = el("button", {
        class: "btn btn-primary",
        type: "submit",
        text: "Send to Pigeonpost →",
      });
      form.append(
        el("label", {}, "Subject", subject),
        el("label", {}, "Topic", category),
        el("label", {}, "Message", body),
        el("p", {
          class: "muted",
          text: "Please don’t include passwords, payment card numbers, or API keys.",
        }),
        button,
      );
      form.onsubmit = async (e) => {
        e.preventDefault();
        if (sending || !publisher) return;
        sending = true;
        button.disabled = true;
        try {
          const r = await write(path(), newDraft);
          newDraft = {
            subject: "",
            body: "",
            category: "application",
            client_id: key(),
          };
          await loadList(true);
          await open(r.data.id);
        } catch (e) {
          error(e);
        } finally {
          sending = false;
          button.disabled = false;
        }
      };
      conversation.append(form);
      subject.focus();
    }
    function restartSocket() {
      if (ws) {
        ws.onclose = null;
        ws.close();
        ws = null;
      }
      clearTimeout(reconnect);
      authBlocked = false;
      delay = 1000;
      connect();
    }
    function connect() {
      if (
        (deps.isActive && !deps.isActive()) ||
        !active ||
        authBlocked ||
        (!staff && !publisher) ||
        !deps.getToken ||
        !window.WebSocket
      )
        return;
      connection.textContent = "Connecting…";
      connection.dataset.state = "connecting";
      let socket;
      try {
        const u = new URL(
          "/v1/support/live",
          deps.postbox || "https://postbox.pigeonpost.dev",
        );
        u.protocol = u.protocol === "https:" ? "wss:" : "ws:";
        socket = ws = new WebSocket(u);
      } catch {
        connection.textContent = "Connection unavailable";
        return;
      }
      socket.onopen = () => {
        if (socket !== ws) return;
        socket.send(
          JSON.stringify({
            token: deps.getToken(),
            publisher: staff ? null : publisher,
            staff,
          }),
        );
      };
      socket.onmessage = (e) => {
        if (socket !== ws || !active) return;
        let v;
        try {
          v = JSON.parse(e.data);
        } catch {
          return;
        }
        if (v.type === "auth_required") {
          authBlocked = true;
          connection.textContent = "Sign-in verification needed";
          connection.dataset.state = "offline";
          loadList(true);
          socket.close();
          return;
        }
        if (v.type === "ready") {
          delay = 1000;
          connection.textContent = "Live updates on";
          connection.dataset.state = "live";
          loadList(true);
          if (selected) open(selected, true);
        }
        if (v.type === "support.changed" || v.type === "resync") {
          loadList(true);
          if (selected && (!v.ticket_id || v.ticket_id === selected))
            open(selected, true);
        }
      };
      socket.onclose = () => {
        if (socket !== ws || !active || authBlocked) return;
        connection.textContent = "Reconnecting… Your draft is safe";
        connection.dataset.state = "offline";
        reconnect = setTimeout(connect, delay);
        delay = Math.min(delay * 2, 15000);
      };
      socket.onerror = () => socket.close();
    }
    const visibility = () => {
      if (!document.hidden) {
        loadList(true);
        if (selected) open(selected, true);
      }
    };
    document.addEventListener("visibilitychange", visibility);
    const routeChanged = () => {
      if (deps.isActive && !deps.isActive()) {
        clearTimeout(reconnect);
        if (ws) {
          ws.onclose = null;
          ws.close();
          ws = null;
        }
      } else if (!ws) {
        restartSocket();
        loadList(true);
      }
    };
    window.addEventListener("pp-workspace-route", routeChanged);
    const observer = new MutationObserver(() => {
      if (!root.isConnected) destroy();
    });
    observer.observe(document.body, { childList: true, subtree: true });
    function destroy() {
      active = false;
      window.removeEventListener("resize", size);
      root.classList.remove("support-root", "ws-open");
      window.removeEventListener("pp-workspace-route", routeChanged);
      seq++;
      listGeneration++;
      clearTimeout(reconnect);
      if (ws) {
        ws.onclose = null;
        ws.close();
      }
      observer.disconnect();
      document.removeEventListener("visibilitychange", visibility);
    }
    async function init() {
      try {
        if (!staff) {
          publishers = (await deps.pb("/v1/publishers")).data || [];
          if (!active) return;
          publisherSelect.replaceChildren(
            ...publishers.map((p) =>
              el("option", { value: p.id, text: p.display_name }),
            ),
          );
          publisher = publishers[0]?.id || "";
          if (!publisher) {
            connection.textContent = "Create your business profile first";
            conversation.replaceChildren(
              el(
                "div",
                { class: "ws-welcome" },
                el("h3", { text: "Let’s get your business set up." }),
                el("p", {
                  text: "Create a business profile, then you and your team can contact support here—even before your application is approved.",
                }),
                el("a", {
                  class: "btn btn-primary",
                  href: "#business",
                  text: "Set up my business →",
                }),
              ),
            );
            return;
          }
        }
        await loadList(true);
        connect();
      } catch (e) {
        error(e);
      }
    }
    const size = () => {
      const layout = root.querySelector(".ws-layout");
      if (layout)
        layout.style.height =
          Math.max(
            520,
            window.innerHeight - layout.getBoundingClientRect().top - 24,
          ) + "px";
    };
    window.addEventListener("resize", size);
    setTimeout(size, 0);
    drawConversation();
    init();
    return { destroy, reload: () => loadList(true) };
  };
})();
