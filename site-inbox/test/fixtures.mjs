// Shared wire-shaped data for DOM regressions and the local browser acceptance server.
export const OWNER = "/k/owner";
export const SECOND = "/k/second";
export const PEER = "/alp/main";
export const SUBJECT = "thread-main";

export function response(body, status = 200) {
  return { ok: status >= 200 && status < 300, status, json: async () => structuredClone(body) };
}

export function fixtureServer() {
  const server = {
    identities: [{ address: OWNER, handle: "/bekir/main" }, { address: SECOND, handle: "/garden/main" }],
    calls: [], mailboxes: {}, intercept: null,
  };
  for (const address of [OWNER, SECOND]) {
    server.mailboxes[address] = {
      messages: Array.from({ length: 35 }, (_, i) => ({
        message_id: `${address}-${i + 1}`, direction: i % 3 === 1 ? "out" : "in",
        from: "/k/peer", peer: PEER, peer_handle: PEER, thread_id: SUBJECT,
        body: `Message ${i + 1} for ${address}.\n\n${"A conversation with enough text to exercise real scrolling. ".repeat(4)}`,
        received_at: 1789500000 + i * 60, read: true, autonomy: "review",
      })),
      threads: [{ thread_id: SUBJECT, peer: PEER, title: "Development ideas", last_at: 1789600000, is_default: true },
        { thread_id: "another-thread", peer: PEER, title: "Release notes", last_at: 1789500000 }],
      contacts: [{ peer: PEER, alias: null, admission: "allow", autonomy: "review", allowed_verbs: [] }],
      archived: [],
    };
  }
  server.fetch = async (url, opts = {}) => {
    const parsed = new URL(url, "https://postbox.pigeonpost.dev");
    const body = typeof opts.body === "string" ? JSON.parse(opts.body) : null;
    const call = { path: parsed.pathname, query: parsed.searchParams, method: opts.method || "GET", body, opts };
    server.calls.push(call);
    const intercepted = await server.intercept?.(call);
    if (intercepted) return intercepted;
    const address = call.query.get("identity") || body?.identity || body?.from || opts.headers?.["x-pigeonpost-identity"];
    const mailbox = server.mailboxes[address] || { messages: [], threads: [], contacts: [], archived: [] };
    if (call.path === "/v1/identities") return response({ identities: server.identities });
    if (call.path === "/v1/whoami") return response(server.identities.find(id => id.address === address) || {});
    if (call.path === "/v1/events") return response({ error: "not_found" }, 404);
    if (call.path === "/v1/inbox") {
      if (call.query.has("wait")) return new Promise(() => {});
      return response({ messages: mailbox.messages });
    }
    if (call.path === "/v1/contacts") return response({ contacts: mailbox.contacts });
    if (call.path === "/v1/archive") return response({ archived: mailbox.archived });
    if (call.path === "/v1/threads") return response({ threads: mailbox.threads });
    if (call.path.startsWith("/v1/threads/") && call.method === "DELETE") {
      const id = decodeURIComponent(call.path.slice("/v1/threads/".length));
      mailbox.threads = mailbox.threads.filter(t => t.thread_id !== id);
      mailbox.messages = mailbox.messages.filter(m => m.thread_id !== id);
      return response({ ok: true });
    }
    if (call.path === "/v1/send") {
      const id = `sent-${server.calls.length}`;
      mailbox.messages.push({ message_id: id, peer: body.to, peer_handle: body.to, body: body.body,
        direction: "out", thread_id: body.thread_id || SUBJECT, received_at: 1789700000, read: true });
      return response({ message_id: id, sent_copy_id: id }, 201);
    }
    if (call.path === "/v1/attachments") return response({ id: "fixture-file" }, 201);
    if (call.path === "/v1/ack") return response({ ok: true });
    if (call.path === "/v1/me/handles") return response({ handles: [{ namespace: "garden", source: "google", active: true }] });
    return response({ error: "not_found" }, 404);
  };
  return server;
}
