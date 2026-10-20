// "Talk to sales" requests from the enterprise portal (organizations above 500 seats, SDS §5.3).
//
// The bridge holds no mailbox, so it cannot write to /pigeonpost/sales itself. A request is
// validated, rate-limited per source address, and appended to a bounded JSON-lines file in the
// bridge's private state directory, where the sales owner reads it. Nothing here is echoed back to
// the browser beyond "received", so the route cannot be used to probe anything.

import { appendFileSync, mkdirSync, statSync } from "node:fs";
import { join } from "node:path";

const FIELDS = { name: 120, company: 160, contact: 254, size: 32, message: 2000 };
const REQUIRED = ["name", "company", "contact"];
const CONTACT = /^[^\s@<>"]{1,64}@[^\s@<>"]{1,189}\.[^\s@<>"]{2,63}$/;
const fail = (status, message, code) => { throw Object.assign(new Error(message), { status, code }); };

export function createSales(dir, { perHour = 5, maxBytes = 5 * 1024 * 1024, now = () => Date.now() } = {}) {
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
    appendFileSync(file, JSON.stringify({ at: new Date(now()).toISOString(), ...clean }) + "\n", { mode: 0o600 });
    console.log(`sales: request recorded (${clean.size || "size not given"})`);
    return { received: true };
  }

  return { record, file };
}

/// The caller's address as nginx reports it. Only trusted when the socket peer is local (the
/// reverse proxy); a direct caller cannot choose its own rate-limit bucket.
export function sourceOf(req) {
  const peer = req.socket?.remoteAddress || "";
  const local = peer === "127.0.0.1" || peer === "::1" || peer === "::ffff:127.0.0.1";
  const forwarded = req.headers["x-real-ip"] || String(req.headers["x-forwarded-for"] || "").split(",")[0].trim();
  return (local && forwarded) ? String(forwarded).slice(0, 64) : peer;
}
