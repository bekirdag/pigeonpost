// Contacts, phone numbers and names — the parts that are rules rather than screens.
//
// docs/contacts-and-phone.md is the contract. Everything here is pure (no DOM, no network) so the
// rules can be tested on their own, and so the native clients can port them with the same tables:
//
//   normalizeE164   a typed or imported number → "+<8–15 digits>" or null (§2)
//   hashPhone       E.164 → 32 hex characters, the only thing discovery ever sends (§3.1)
//   parseVcard      a .vcf export → [{ name, phones }] (names stay in this page; §1.2)
//   parseCsv        a Google / Outlook / generic .csv export → the same shape
//   pickName        which name to show for a peer, and how (§6.3)
//   aliases         what *you* call someone, in this browser only (§6.2)
//
// Nothing read from a contacts file is ever stored here: the page parses it, hashes the numbers and
// lets the list go when the sheet closes.
window.PigeonpostContacts = (function () {
  "use strict";

  // ---- phone numbers (§2) -------------------------------------------------------------------

  // Deliberately simpler than libphonenumber. A number this gets wrong simply fails to match.
  function normalizeE164(raw, defaultCallingCode) {
    if (raw == null) return null;
    let s = String(raw).trim();
    s = s.replace(/^tel:/i, "");
    // URI parameters (`;ext=12`, `;phone-context=…`) and written extensions (`x12`, `ext. 12`).
    s = s.split(";")[0];
    // "+44 (0)20 …": the trunk 0 some countries write in brackets after the country code.
    s = s.replace(/^(\+\d{1,3})[\s.-]*\(0\)/, "$1");
    s = s.replace(/\s*(?:ext(?:ension)?\.?|x|#)\s*\d*\s*$/i, "");
    s = s.replace(/[\s.\-()\u00A0]/g, "");
    if (!/^\+?\d+$/.test(s)) return null; // letters, short codes with symbols, empty
    const cc = String(defaultCallingCode == null ? "" : defaultCallingCode).replace(/\D/g, "");
    let digits;
    if (s.startsWith("+")) digits = s.slice(1);
    else if (s.startsWith("00")) digits = s.slice(2);
    else if (cc === "1" && s.startsWith("011")) digits = s.slice(3);
    else if (!cc) return null;
    else if (cc === "1") {
      // NANP has no trunk prefix: an 11-digit number starting with 1 already carries the country code.
      digits = s.length === 11 && s.startsWith("1") ? s : "1" + s;
    } else {
      digits = cc + (s.startsWith("0") ? s.slice(1) : s);
    }
    return /^[1-9]\d{7,14}$/.test(digits) ? "+" + digits : null;
  }

  const HASH_PREFIX = "pigeonpost:phone:v1:";

  function subtle() {
    const c = (typeof window !== "undefined" && window.crypto) || globalThis.crypto;
    if (!c || !c.subtle) throw new Error("This browser cannot hash numbers (WebCrypto is unavailable).");
    return c.subtle;
  }

  // First 16 bytes of SHA-256("pigeonpost:phone:v1:" + E.164), lowercase hex.
  async function hashPhone(e164) {
    const digest = await subtle().digest("SHA-256", new TextEncoder().encode(HASH_PREFIX + e164));
    return Array.from(new Uint8Array(digest).slice(0, 16), (b) => (b < 16 ? "0" : "") + b.toString(16)).join("");
  }

  // ---- vCard ----------------------------------------------------------------------------------

  function decodeQuotedPrintable(value) {
    const bytes = [];
    for (let i = 0; i < value.length; i++) {
      const c = value[i];
      if (c === "=" && /^[0-9a-f]{2}$/i.test(value.slice(i + 1, i + 3))) {
        bytes.push(parseInt(value.slice(i + 1, i + 3), 16));
        i += 2;
      } else {
        const code = value.charCodeAt(i);
        if (code < 128) bytes.push(code);
        else bytes.push(...new TextEncoder().encode(c));
      }
    }
    try { return new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array(bytes)); }
    catch (_) { return String.fromCharCode(...bytes); }
  }

  function vcardUnescape(value) {
    return value.replace(/\\([\\;,nN])/g, (_, c) => (c === "n" || c === "N" ? " " : c));
  }

  // Split on a separator that is not escaped with a backslash.
  function splitUnescaped(value, sep) {
    const out = [];
    let cur = "";
    for (let i = 0; i < value.length; i++) {
      if (value[i] === "\\" && i + 1 < value.length) { cur += value[i] + value[i + 1]; i++; continue; }
      if (value[i] === sep) { out.push(cur); cur = ""; continue; }
      cur += value[i];
    }
    out.push(cur);
    return out;
  }

  function parseVcard(text) {
    const physical = String(text || "").replace(/^\uFEFF/, "").replace(/\r\n?/g, "\n").split("\n");
    // Unfold: a line starting with a space or tab continues the one before it (3.0/4.0, and 2.1 in
    // practice). A 2.1 quoted-printable value ending in "=" continues on the next line as is.
    const lines = [];
    for (const line of physical) {
      const prev = lines.length ? lines[lines.length - 1] : null;
      if (prev !== null && /^[ \t]/.test(line)) lines[lines.length - 1] = prev + line.slice(1);
      else if (prev !== null && /QUOTED-PRINTABLE/i.test(prev.split(":")[0]) && /=$/.test(prev)) lines[lines.length - 1] = prev.slice(0, -1) + line;
      else lines.push(line);
    }
    const cards = [];
    let card = null;
    for (const line of lines) {
      const colon = line.indexOf(":");
      if (colon < 0) continue;
      const head = line.slice(0, colon), rawValue = line.slice(colon + 1);
      const parts = head.split(";");
      const name = parts[0].replace(/^[^.]*\./, "").trim().toUpperCase(); // drop an `item1.` group
      const params = parts.slice(1).map((p) => p.trim().toUpperCase());
      if (name === "BEGIN" && /^VCARD$/i.test(rawValue.trim())) { card = { fn: "", n: "", phones: [] }; continue; }
      if (!card) continue;
      if (name === "END" && /^VCARD$/i.test(rawValue.trim())) {
        cards.push({ name: (card.fn || card.n).trim(), phones: card.phones });
        card = null;
        continue;
      }
      const qp = params.some((p) => p === "QUOTED-PRINTABLE" || p === "ENCODING=QUOTED-PRINTABLE");
      const value = qp ? decodeQuotedPrintable(rawValue) : rawValue;
      if (name === "FN") card.fn = vcardUnescape(value).replace(/\s+/g, " ");
      else if (name === "N") {
        const [family = "", given = "", additional = "", prefix = "", suffix = ""] = splitUnescaped(value, ";").map(vcardUnescape);
        card.n = [prefix, given, additional, family, suffix].map((s) => s.trim()).filter(Boolean).join(" ");
      } else if (name === "TEL") {
        const number = value.trim();
        if (number) card.phones.push(number);
      }
    }
    return cards;
  }

  // ---- CSV ------------------------------------------------------------------------------------

  // RFC 4180: quoted fields may hold commas, doubled quotes and line breaks.
  function csvRows(text) {
    const s = String(text || "").replace(/^\uFEFF/, "");
    const rows = [];
    let row = [], field = "", quoted = false;
    for (let i = 0; i < s.length; i++) {
      const c = s[i];
      if (quoted) {
        if (c === '"' && s[i + 1] === '"') { field += '"'; i++; }
        else if (c === '"') quoted = false;
        else field += c;
      } else if (c === '"' && field === "") quoted = true;
      else if (c === ",") { row.push(field); field = ""; }
      else if (c === "\n" || c === "\r") {
        if (c === "\r" && s[i + 1] === "\n") i++;
        row.push(field); rows.push(row); row = []; field = "";
      } else field += c;
    }
    if (field !== "" || row.length) { row.push(field); rows.push(row); }
    return rows.filter((r) => r.some((f) => f.trim() !== ""));
  }

  // Google Contacts: "Name" or "First Name"/"Middle Name"/"Last Name" (older exports "Given Name"/
  // "Family Name"), and "Phone 1 - Value", "Phone 2 - Value"… whose value may hold several numbers
  // joined by " ::: ". Outlook: "First Name", "Last Name", "Mobile Phone", "Business Phone",
  // "Home Phone", "Primary Phone" and friends. Anything else: a header that says phone, mobile,
  // cell or tel, minus the label/type columns that sit beside them.
  function parseCsv(text) {
    const rows = csvRows(text);
    if (rows.length < 2) return [];
    const headers = rows[0].map((h) => h.trim());
    const lower = headers.map((h) => h.toLowerCase());
    const col = (...names) => names.map((n) => lower.indexOf(n)).find((i) => i >= 0) ?? -1;
    const nameCol = col("name", "display name", "full name", "file as");
    const firstCol = col("first name", "given name");
    const middleCol = col("middle name", "additional name");
    const lastCol = col("last name", "family name", "surname");
    const fallbackCol = col("nickname", "organization name", "organization 1 - name", "company");
    const phoneCols = lower
      .map((h, i) => ((/\b(phone|mobile|cell|tel|telephone)\b/.test(h) && !/(type|label)\b/.test(h)) ? i : -1))
      .filter((i) => i >= 0);
    const out = [];
    for (const r of rows.slice(1)) {
      const get = (i) => (i >= 0 && i < r.length ? r[i].trim() : "");
      const name = get(nameCol) || [get(firstCol), get(middleCol), get(lastCol)].filter(Boolean).join(" ") || get(fallbackCol);
      const phones = [];
      for (const i of phoneCols) {
        for (const p of get(i).split(":::")) if (p.trim()) phones.push(p.trim());
      }
      out.push({ name: name.replace(/\s+/g, " "), phones });
    }
    return out;
  }

  // ---- names (§6) -----------------------------------------------------------------------------

  // The same character set as the server's `profile_names.rs::forbidden`: control characters, `/`,
  // `@`, and the format, bidi, zero-width and filler characters that make one name look like another.
  const FORBIDDEN = /[\p{Cc}/@\u00AD\u034F\u0600-\u0605\u061C\u06DD\u070F\u115F\u1160\u17B4\u17B5\u180B-\u180F\u200B-\u200F\u2028-\u202E\u2060-\u206F\u2800\u3164\uFE00-\uFE0F\uFEFF\uFFA0\uFFF0-\uFFFB\u{E0000}-\u{E007F}]/u;
  const RESERVED = ["admin", "administrator", "support", "official", "verified", "security", "system", "moderator"];

  // `{ ok: true, value }` with the tidied name, or `{ ok: false, error }` in words a person can act on.
  function validateName(raw, max, reserved) {
    const text = String(raw == null ? "" : raw).normalize("NFC");
    if (FORBIDDEN.test(text)) return { ok: false, error: "Names can\u2019t contain /, @, or invisible characters." };
    const name = text.split(/\s+/).filter(Boolean).join(" ");
    const length = Array.from(name).length;
    if (!length) return { ok: false, error: "Enter a name." };
    if (length > max) return { ok: false, error: `Use at most ${max} characters.` };
    if (!/[\p{L}\p{N}]/u.test(name)) return { ok: false, error: "Use at least one letter or digit." };
    if (reserved) {
      const squashed = Array.from(name.toLowerCase()).filter((c) => /[\p{L}\p{N}]/u.test(c)).join("");
      if (squashed.includes("pigeonpost") || RESERVED.includes(squashed)) return { ok: false, error: "That name is reserved. Choose another." };
    }
    return { ok: true, value: name };
  }
  const validateDisplayName = (raw) => validateName(raw, 40, true);
  const validateAlias = (raw) => validateName(raw, 64, false);

  // §6.3: local alias, then the server contact alias, then a self-set display name (shown as
  // "~Name"), then the name derived from the handle.
  function pickName({ alias, contactAlias, displayName, derived } = {}) {
    const clean = (v) => (typeof v === "string" ? v.trim() : "");
    if (clean(alias)) return { text: clean(alias), kind: "alias" };
    if (clean(contactAlias)) return { text: clean(contactAlias), kind: "contact" };
    if (clean(displayName)) return { text: "~" + clean(displayName), kind: "display" };
    return { text: clean(derived) || "unknown", kind: "derived" };
  }

  // The sender's name as the invitation SMS carries it (§5): letters, digits, spaces and .'- only,
  // at most 24 characters, otherwise "A friend".
  function inviteName(displayName) {
    const kept = String(displayName || "").replace(/[^\p{L}\p{N} .'-]/gu, "").replace(/\s+/g, " ").trim();
    return Array.from(kept).slice(0, 24).join("").trim() || "A friend";
  }

  function inviteText(displayName) {
    return "Pigeonpost: " + inviteName(displayName)
      + " invited you to chat on Pigeonpost. Join: https://pigeonpost.dev/invite?c=\u2026 Reply STOP to opt out.";
  }

  // ---- local aliases (§6.2) ----------------------------------------------------------------------

  const ALIAS_PREFIX = "ppi_aliases_v1:";

  function storage() {
    try { return window.localStorage || null; } catch (_) { return null; }
  }

  // What *you* call someone, in this browser only. Never sent anywhere. Every access is guarded:
  // storage can be disabled, full or throwing, and the inbox must work the same without it (an
  // alias then lasts until the page is reloaded).
  function aliases(accountId) {
    const key = ALIAS_PREFIX + String(accountId || "local");
    let memory = null;
    const load = () => {
      if (memory) return memory;
      let data = {};
      try {
        const raw = storage()?.getItem(key);
        const parsed = raw ? JSON.parse(raw) : {};
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) data = parsed;
      } catch (_) { data = {}; }
      memory = {};
      for (const [peer, alias] of Object.entries(data)) if (typeof alias === "string" && alias) memory[peer] = alias;
      return memory;
    };
    const save = () => {
      try {
        const s = storage();
        if (!s) return;
        if (Object.keys(memory).length) s.setItem(key, JSON.stringify(memory));
        else s.removeItem(key);
      } catch (_) { /* kept in memory for this page */ }
    };
    return {
      key,
      get(peer) { return (peer && Object.hasOwn(load(), peer)) ? load()[peer] : null; },
      set(peer, alias) {
        const checked = validateAlias(alias);
        if (!checked.ok) throw new Error(checked.error);
        load()[peer] = checked.value;
        save();
        return checked.value;
      },
      remove(peer) { delete load()[peer]; save(); },
      all() { return { ...load() }; },
      clear() {
        memory = {};
        try { storage()?.removeItem(key); } catch (_) { /* nothing to clear */ }
      },
    };
  }

  // Signing out removes every alias this browser holds, whichever account set them.
  function clearAllAliases() {
    try {
      const s = storage();
      if (!s) return;
      const keys = [];
      for (let i = 0; i < s.length; i++) { const k = s.key(i); if (k && k.startsWith(ALIAS_PREFIX)) keys.push(k); }
      keys.forEach((k) => s.removeItem(k));
    } catch (_) { /* storage unavailable */ }
  }

  // Common countries first; anything else is typed as "+code". Calling codes, not regions: NANP
  // shares +1 between the US and Canada.
  const COUNTRIES = [
    ["TR", "T\u00FCrkiye", "90"], ["US", "United States / Canada", "1"], ["GB", "United Kingdom", "44"],
    ["DE", "Germany", "49"], ["FR", "France", "33"], ["NL", "Netherlands", "31"], ["ES", "Spain", "34"],
    ["IT", "Italy", "39"], ["AU", "Australia", "61"], ["IN", "India", "91"],
  ];

  // A starting guess from the browser's language (en-GB → 44). Only a default; the person picks.
  function guessCallingCode(languages) {
    for (const lang of languages || []) {
      const region = String(lang).split(/[-_]/)[1];
      if (!region) continue;
      const hit = COUNTRIES.find(([r]) => r === region.toUpperCase() || (r === "US" && region.toUpperCase() === "CA"));
      if (hit) return hit[2];
    }
    return "1";
  }

  return {
    normalizeE164, hashPhone, parseVcard, parseCsv, pickName, validateName, validateDisplayName,
    validateAlias, inviteName, inviteText, aliases, clearAllAliases, COUNTRIES, guessCallingCode,
  };
})();
