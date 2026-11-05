// "Talk to sales" requests from the enterprise portal (organizations above 500 seats, SDS §5.3).
//
// The bridge holds no mailbox, so it cannot write to /pigeonpost/sales itself. A request is
// validated, rate-limited per source address, and appended to a bounded JSON-lines file in the
// bridge's private state directory (the durable record). A person is then told through saas_be's
// `pigeonpost-notice` workflow (Novu → the tracking-off Postmark server "Pigeonpost") sends it to
// SALES_NOTIFY_TO, default info@pigeonpost.dev. Sending never delays or changes the answer; it is
// retried a few times in the background, and the file stays the record if it never goes out.
// Nothing here is echoed back to the browser beyond "received", so the route cannot be used to
// probe anything.

import { randomBytes } from "node:crypto";
import { appendFileSync, mkdirSync, statSync } from "node:fs";
import { join } from "node:path";

const FIELDS = { name: 120, company: 160, contact: 254, size: 32, message: 2000 };
const REQUIRED = ["name", "company", "contact"];
const CONTACT = /^[^\s@<>"]{1,64}@[^\s@<>"]{1,189}\.[^\s@<>"]{2,63}$/;
const fail = (status, message, code) => { throw Object.assign(new Error(message), { status, code }); };

const ESCAPES = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
const esc = (text) => String(text).replace(/[&<>"']/g, (c) => ESCAPES[c]);
const LABELS = [["name", "Name"], ["company", "Company"], ["contact", "Contact"], ["size", "Seats"], ["message", "Message"]];

/// Subject, HTML and plain text for one lead: plain, escaped, no images, no links but the reply address.
export function renderLead(lead) {
  const subject = `Sales request: ${lead.company}${lead.size ? ` (${lead.size} seats)` : ""}`.replace(/[\r\n]+/g, " ").slice(0, 200);
  const rows = LABELS.filter(([key]) => lead[key]);
  const text = `A "Talk to sales" request came in from the enterprise portal at ${lead.at}.\n\n`
    + rows.map(([key, label]) => `${label}: ${lead[key]}`).join("\n")
    + `\n\nReference: ${lead.id}\nReply to the contact address directly; replies to this message are not read.\n`;
  const html = `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>${esc(subject)}</title></head>`
    + `<body style="margin:0;padding:24px;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;color:#14181f;">`
    + `<h1 style="font-size:18px;margin:0 0 12px;">New "Talk to sales" request</h1>`
    + `<p style="margin:0 0 16px;color:#6b7480;font-size:14px;">From the enterprise portal at ${esc(lead.at)}.</p>`
    + `<table style="border-collapse:collapse;font-size:14px;">`
    + rows.map(([key, label]) => `<tr><td style="padding:4px 16px 4px 0;color:#6b7480;vertical-align:top;">${esc(label)}</td>`
      + `<td style="padding:4px 0;white-space:pre-wrap;">${esc(lead[key])}</td></tr>`).join("")
    + `</table><p style="margin:16px 0 0;color:#6b7480;font-size:13px;">Reference ${esc(lead.id)}. Reply to the contact address directly; replies to this message are not read.</p>`
    + `</body></html>`;
  return { subject, html, text };
}

/// The saas_be internal notification request for one lead (`pigeonpost-notice`).
export function noticeRequest(lead, to, from) {
  const { subject, html, text } = renderLead(lead);
  const delivery = `pp-sales-${lead.id}`;
  return {
    tenant_id: "pigeonpost", workflow_id: "pigeonpost-notice", transaction_id: delivery, to, from,
    sender_name: "Pigeonpost sales", subject, html, text,
    payload: { tenant_id: "pigeonpost", delivery_id: delivery, notice_kind: "sales_lead", ref_id: lead.id,
      subject, html, text, locale: "en" },
  };
}

/// Sends leads in the background. `fetch`, the delays and the clock are injectable for tests.
export function createNotifier({ url = "", key = "", to = "info@pigeonpost.dev", from = "no-reply@pigeonpost.dev",
  fetch: fetchImpl = globalThis.fetch, delays = [0, 30_000, 120_000, 600_000], timeoutMs = 15_000 } = {}) {
  const base = String(url).trim().replace(/\/+$/, "");
  const configured = Boolean(base && key && (base.startsWith("https://") || /^http:\/\/(127\.0\.0\.1|localhost)(:|\/|$)/.test(base)));
  const counters = { sent: 0, failed: 0, gave_up: 0 };
  async function attempt(body) {
    const ac = new AbortController();
    const timer = setTimeout(() => ac.abort(), timeoutMs);
    timer.unref?.();
    try {
      const res = await fetchImpl(`${base}/v1/internal/notifications/email`, {
        method: "POST", signal: ac.signal,
        headers: { "x-api-key": key, "content-type": "application/json", accept: "application/json", "idempotency-key": body.transaction_id },
        body: JSON.stringify(body),
      });
      return res.ok;
    } catch {
      return false;
    } finally {
      clearTimeout(timer);
    }
  }
  function send(lead) {
    if (!configured) return Promise.resolve(false);
    const body = noticeRequest(lead, to, from);
    return (async () => {
      for (const delay of delays) {
        if (delay) await new Promise((resolve) => { const t = setTimeout(resolve, delay); t.unref?.(); });
        if (await attempt(body)) { counters.sent++; console.log(`sales: lead emailed (sent=${counters.sent})`); return true; }
        counters.failed++;
      }
      counters.gave_up++;
      console.error(`sales: lead not emailed after ${delays.length} attempts (gave_up=${counters.gave_up}); it is in the request file`);
      return false;
    })();
  }
  return { configured, send, health: () => ({ configured, ...counters }) };
}

export function createSales(dir, { perHour = 5, maxBytes = 5 * 1024 * 1024, now = () => Date.now(), notifier = null } = {}) {
  const file = join(dir, "sales-requests.jsonl");
  const recent = new Map(); // source -> timestamps within the last hour

  function allow(source) {
    const since = now() - 3600_000;
    const list = (recent.get(source) || []).filter((t) => t > since);
    if (list.length >= perHour) return false;
    list.push(now());
    recent.set(source, list);
    if (recent.size > 10_000) {
      for (const [key, times] of recent) if (!times.some((t) => t > since)) recent.delete(key);
    }
    return true;
  }

  function record(source, body) {
    if (!body || typeof body !== "object" || Array.isArray(body)) fail(400, "A JSON object is required.", "invalid_request");
    const extra = Object.keys(body).find((key) => !Object.hasOwn(FIELDS, key));
    if (extra) fail(400, `Unexpected field: ${extra}`, "invalid_request");
    const clean = {};
    for (const [key, max] of Object.entries(FIELDS)) {
      const value = body[key];
      if (value === undefined || value === null || value === "") {
        if (REQUIRED.includes(key)) fail(400, `${key} is required.`, "invalid_request");
        continue;
      }
      if (typeof value !== "string" && !(key === "size" && Number.isInteger(value))) fail(400, `${key} is invalid.`, "invalid_request");
      const text = String(value).trim();
      // Control characters would let one request forge further lines in the file.
      if (!text || text.length > max || /[\u0000-\u0008\u000b-\u001f\u007f]/.test(text)) fail(400, `${key} is invalid.`, "invalid_request");
      clean[key] = text;
    }
    if (!CONTACT.test(clean.contact)) fail(400, "Enter a valid contact address.", "invalid_request");
    if (!allow(source)) fail(429, "Too many requests from here. Try again later.", "rate_limited");
    let size = 0;
    try { size = statSync(file).size; } catch (error) { if (error.code !== "ENOENT") throw error; }
    if (size > maxBytes) {
      console.error("sales: request file is full; not recording");
      fail(503, "We could not take your request right now. Try again later.", "unavailable");
    }
    mkdirSync(dir, { recursive: true, mode: 0o700 });
    const lead = { id: `lead_${randomBytes(9).toString("hex")}`, at: new Date(now()).toISOString(), ...clean };
    appendFileSync(file, JSON.stringify(lead) + "\n", { mode: 0o600 });
    console.log(`sales: request recorded (${clean.size || "size not given"})`);
    // Tell a person, without making the browser wait for it.
    if (notifier) notifier.send(lead).catch(() => {});
    return { received: true };
  }

  return { record, file, notifier };
}

/// The caller's address as nginx reports it. Only trusted when the socket peer is local (the
/// reverse proxy); a direct caller cannot choose its own rate-limit bucket.
export function sourceOf(req) {
  const peer = req.socket?.remoteAddress || "";
  const local = peer === "127.0.0.1" || peer === "::1" || peer === "::ffff:127.0.0.1";
  const forwarded = req.headers["x-real-ip"] || String(req.headers["x-forwarded-for"] || "").split(",")[0].trim();
  return (local && forwarded) ? String(forwarded).slice(0, 64) : peer;
}
