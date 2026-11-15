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

// The design kit's email template (design.pigeonpost.dev/templates/email.html), the same frame as
// the postbox's mail_layout.rs: kit tokens as literal colours, tables and inline styles, a <style>
// block only for dark mode and phones, no webfont, no tracking pixel. The only images are the
// versioned bird symbols on pigeonpost.dev, which carry no per-recipient data.
const FONT = "-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Helvetica,Arial,sans-serif";
export const LOGO_LIGHT = "https://pigeonpost.dev/email/pigeonpost-symbol.v2.png";
export const LOGO_DARK = "https://pigeonpost.dev/email/pigeonpost-symbol-white.v2.png";
const FOOTER_BRAND = "Pigeonpost · Private messaging for people and AI agents · pigeonpost.dev";
const K = { ground: "#F4F6F8", card: "#FFFFFF", rule: "#E1E5EA", soft: "#F4F6F8", ink: "#12161C", body: "#343C47", muted: "#59626E", brand: "#05368B" };

/// The kit frame around already-escaped card HTML. `title` and `preheader` are plain text; `footer`
/// is escaped HTML, followed by the brand line.
export function kitDocument({ title, preheader = "", card, footer = "" }) {
  const pre = preheader
    ? `<div style="display:none;font-size:1px;line-height:1px;max-height:0;max-width:0;opacity:0;overflow:hidden;mso-hide:all;color:${K.ground};">${esc(preheader)}&#8199;&#65279;&#847;&#8199;&#65279;&#847;</div>\n`
    : "";
  const foot = footer ? `${footer}<br>${esc(FOOTER_BRAND)}` : esc(FOOTER_BRAND);
  return `<!DOCTYPE html>
<html lang="en" xmlns="http://www.w3.org/1999/xhtml">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="x-apple-disable-message-reformatting">
<meta name="format-detection" content="telephone=no, date=no, address=no, email=no, url=no">
<meta name="color-scheme" content="light dark">
<meta name="supported-color-schemes" content="light dark">
<title>${esc(title)}</title>
<style>
  :root { color-scheme: light dark; supported-color-schemes: light dark; }
  body { margin: 0 !important; padding: 0 !important; width: 100% !important; }
  @media (prefers-color-scheme: dark) {
    .pp-bg { background-color: #0B0E12 !important; }
    .pp-card { background-color: #141820 !important; border-color: #232A35 !important; }
    .pp-brand { color: #8FB4FF !important; }
    .pp-ink { color: #F2F4F7 !important; }
    .pp-body { color: #D3D8E0 !important; }
    .pp-muted { color: #A3ACB9 !important; }
    .pp-rule { border-color: #232A35 !important; }
    .pp-panel { background-color: #10141A !important; border-color: #232A35 !important; }
    .pp-logo-light { display: none !important; }
    .pp-logo-dark { display: block !important; max-height: none !important; overflow: visible !important; }
  }
  /* Outlook.com dark mode */
  [data-ogsc] .pp-brand { color: #8FB4FF !important; }
  [data-ogsc] .pp-ink { color: #F2F4F7 !important; }
  [data-ogsc] .pp-body { color: #D3D8E0 !important; }
  [data-ogsc] .pp-muted { color: #A3ACB9 !important; }
  [data-ogsc] .pp-logo-light { display: none !important; }
  [data-ogsc] .pp-logo-dark { display: block !important; max-height: none !important; overflow: visible !important; }
  @media only screen and (max-width: 520px) {
    .pp-outer { padding: 20px 16px !important; }
    .pp-card-pad { padding: 26px 20px 22px !important; }
  }
</style>
</head>
<body class="pp-bg" style="margin:0;padding:0;background-color:${K.ground};">
${pre}<table role="presentation" class="pp-bg" width="100%" cellpadding="0" cellspacing="0" border="0" style="background-color:${K.ground};">
  <tr>
    <td class="pp-outer" align="center" style="padding:32px 16px;">
      <table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="max-width:560px;">
        <tr>
          <td style="padding:0 4px 18px;">
            <table role="presentation" cellpadding="0" cellspacing="0" border="0">
              <tr>
                <td width="29" style="width:29px;">
                  <img class="pp-logo-light" src="${LOGO_LIGHT}" width="29" height="24" alt="" style="display:block;border:0;outline:none;width:29px;height:24px;">
                  <!--[if !mso]><!-->
                  <div class="pp-logo-dark" style="display:none;max-height:0;overflow:hidden;mso-hide:all;">
                    <img src="${LOGO_DARK}" width="29" height="24" alt="" style="display:block;border:0;outline:none;width:29px;height:24px;">
                  </div>
                  <!--<![endif]-->
                </td>
                <td class="pp-brand" style="padding-left:8px;font-family:${FONT};font-size:17px;line-height:24px;font-weight:700;color:${K.brand};">Pigeonpost</td>
              </tr>
            </table>
          </td>
        </tr>
        <tr>
          <td class="pp-card pp-card-pad" style="background-color:${K.card};border:1px solid ${K.rule};border-radius:16px;padding:32px 32px 28px;font-family:${FONT};font-size:15px;line-height:24px;color:${K.body};">
${card}
          </td>
        </tr>
        <tr>
          <td class="pp-muted" style="padding:18px 8px 0;font-family:${FONT};font-size:12px;line-height:18px;color:${K.muted};">
${foot}
          </td>
        </tr>
      </table>
    </td>
  </tr>
</table>
</body>
</html>`;
}

/// Subject, HTML and plain text for one lead: escaped, in the kit frame, no links (reply to the
/// contact address directly) and no images but the two static logos.
export function renderLead(lead) {
  const subject = `Sales request: ${lead.company}${lead.size ? ` (${lead.size} seats)` : ""}`.replace(/[\r\n]+/g, " ").slice(0, 200);
  const rows = LABELS.filter(([key]) => lead[key]);
  const text = `A "Talk to sales" request came in from the enterprise portal at ${lead.at}.\n\n`
    + rows.map(([key, label]) => `${label}: ${lead[key]}`).join("\n")
    + `\n\nReference: ${lead.id}\nReply to the contact address directly; replies to this message are not read.\n`
    + `\n--\nPigeonpost · pigeonpost.dev\n`;
  const label = (l) => `<div class="pp-muted" style="font-family:${FONT};font-size:12px;line-height:16px;font-weight:600;text-transform:uppercase;letter-spacing:.04em;color:${K.muted};">${esc(l)}</div>`;
  const panel = rows.map(([key, l], i) => `<div style="margin-top:${i ? 12 : 0}px;">${label(l)}`
    + `<div class="pp-body" style="margin-top:2px;white-space:pre-wrap;font-size:14px;line-height:22px;color:${K.body};">${esc(lead[key])}</div></div>`).join("");
  const card = `<h1 class="pp-ink" style="margin:0 0 16px;font-family:${FONT};font-size:22px;line-height:28px;font-weight:700;color:${K.ink};">New "Talk to sales" request</h1>\n`
    + `<p class="pp-body" style="margin:0 0 14px;font-family:${FONT};font-size:15px;line-height:24px;color:${K.body};">From the enterprise portal at ${esc(lead.at)}.</p>\n`
    + `<table role="presentation" class="pp-panel" width="100%" cellpadding="0" cellspacing="0" border="0" style="background-color:${K.soft};border:1px solid ${K.rule};border-radius:12px;margin:0 0 16px;"><tr><td style="padding:16px 18px;font-family:${FONT};">${panel}</td></tr></table>\n`
    + `<p class="pp-muted pp-rule" style="margin:20px 0 0;padding-top:16px;border-top:1px solid ${K.rule};font-family:${FONT};font-size:13px;line-height:20px;color:${K.muted};">Reference ${esc(lead.id)}. Reply to the contact address directly; replies to this message are not read.</p>`;
  const html = kitDocument({
    title: subject,
    preheader: `${lead.company}${lead.size ? ` · ${lead.size} seats` : ""}`,
    card,
    footer: "Sent to the Pigeonpost sales inbox from enterprise.pigeonpost.dev.",
  });
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
