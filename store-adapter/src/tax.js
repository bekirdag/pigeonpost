// Where a web buyer is, and which VAT treatment their purchase gets (docs/billing-tax.md).
//
// The seller, Piyote Bilişim, is a Turkish company with no EU or UK establishment. For services
// supplied electronically to consumers, EU and UK VAT is due in the buyer's country from the first
// sale (Directive 2006/112/EC Art. 58; UK: no threshold for non-established sellers). Until Piyote
// is registered for the EU non-Union OSS scheme and for UK VAT, the web does not sell to EU/UK
// consumers; they buy in the iOS or Android app, where Apple or Google is the seller. EU/UK
// businesses buy with a VAT number verified here (VIES for the EU, HMRC for the UK) and pay no VAT
// (reverse charge). Everyone else is unchanged.
//
// Evidence of the buyer's location (Implementing Regulation 282/2011 Art. 24b(d), 24f): the billing
// address country and the country of the IP address the checkout came from. A non-EU seller has no
// single-item relief, so this is deliberately conservative: either item in the EU/UK makes the
// buyer an EU/UK buyer, except a Turkish billing profile carrying a Turkish identity or tax number.
// The card's issuing country is not known before iyzico's hosted form, so it is not used here.

import { readFileSync } from "node:fs";
import { config } from "./config.js";

const fail = (status, message, code) => { throw Object.assign(new Error(message), { status, code }); };

export const RATES = JSON.parse(readFileSync(new URL("../catalog/vat-rates.json", import.meta.url), "utf8"));
export const EU = new Set(Object.keys(RATES.EU));
const UK = new Set(["GB"]);

/// "EU", "UK" or null for an ISO 3166-1 alpha-2 code.
export const regionOf = (cc) => (EU.has(cc) ? "EU" : UK.has(cc) ? "UK" : null);

// ---- country names ------------------------------------------------------------------------------

const fold = (s) => String(s || "").replace(/[ıİ]/g, "i").normalize("NFKD").replace(/[̀-ͯ]/g, "")
  .toLowerCase().replace(/[^a-z]+/g, " ").trim();

const ALIASES = {
  uk: "GB", "united kingdom": "GB", "great britain": "GB", britain: "GB", england: "GB", scotland: "GB",
  wales: "GB", "northern ireland": "GB", "united kingdom of great britain and northern ireland": "GB",
  el: "GR", turkey: "TR", turkiye: "TR", "republic of turkey": "TR", "republic of turkiye": "TR",
  usa: "US", "united states of america": "US", america: "US", "czech republic": "CZ", holland: "NL",
  "the netherlands": "NL", "republic of ireland": "IE", eire: "IE", tur: "TR", deu: "DE", fra: "FR",
  gbr: "GB", nld: "NL", esp: "ES", ita: "IT",
};

// Retired, reserved and grouping codes that Intl still names (DD is "Germany" in some ICU builds).
const NOT_COUNTRIES = new Set(["AA", "AN", "BU", "CS", "DD", "EU", "EZ", "FX", "NT", "QO", "QU", "SU", "TP", "UK",
  "UN", "XA", "XB", "YD", "YU", "ZR", "ZZ"]);
const CODES = new Set();
const NAMES = (() => {
  const map = new Map();
  const letters = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
  for (const lang of ["en", "tr", "de", "fr", "es", "it", "nl"]) {
    let names;
    try { names = new Intl.DisplayNames([lang], { type: "region" }); } catch { continue; }
    for (const a of letters) for (const b of letters) {
      const code = a + b;
      if (NOT_COUNTRIES.has(code) || /^(Q[M-Z]|X[A-Z])$/.test(code) && code !== "XK") continue;
      let name;
      try { name = names.of(code); } catch { continue; }
      if (!name || name === code) continue;
      CODES.add(code);
      if (!map.has(fold(name))) map.set(fold(name), code);
    }
  }
  return map;
})();

/// An ISO 3166-1 alpha-2 code from what a billing profile holds ("DE", "Germany", "Almanya",
/// "Türkiye"…), or null when it cannot be recognised.
export function countryCode(value) {
  const raw = String(value || "").trim();
  if (!raw) return null;
  if (/^[A-Za-z]{2}$/.test(raw)) {
    const up = raw.toUpperCase();
    if (up === "UK") return "GB";
    if (up === "EL") return "GR";
    return CODES.has(up) ? up : null;
  }
  const key = fold(raw);
  return ALIASES[key] || NAMES.get(key) || null;
}

/// The country the request came from, as the edge (Cloudflare) geolocated it; null if unknown.
export function ipCountry(headers) {
  const value = String(headers?.[config.tax.ipCountryHeader] || "").trim().toUpperCase();
  return /^[A-Z]{2}$/.test(value) && !["XX", "T1"].includes(value) ? value : null;
}

// ---- VAT numbers ----------------------------------------------------------------------------------

/// Split a VAT number into its country and national part. The prefix is optional when the billing
/// country gives it. Greece uses EL; Northern Ireland's XI numbers are UK numbers for services.
export function parseVatId(raw, billingCountry) {
  const clean = String(raw || "").toUpperCase().replace(/[\s.\-/]/g, "");
  if (!clean) return null;
  let prefix = /^[A-Z]{2}/.test(clean) ? clean.slice(0, 2) : null;
  let number = prefix ? clean.slice(2) : clean;
  if (!prefix) prefix = billingCountry === "GR" ? "EL" : billingCountry;
  const country = prefix === "EL" ? "GR" : prefix === "XI" ? "GB" : prefix;
  if (!regionOf(country) || !/^[0-9A-Z+*]{2,14}$/.test(number)) return { country, prefix, number, malformed: true };
  return { country, prefix, number, id: `${prefix}${number}` };
}

const cache = new Map(); // id -> { result, at }
const CACHE_MS = 24 * 3600_000;

async function getJson(url, init) {
  const res = await fetch(url, { ...init, redirect: "error", signal: AbortSignal.timeout(config.tax.checkTimeoutMs) });
  const text = await res.text();
  let body = null;
  try { body = text ? JSON.parse(text) : null; } catch { body = null; }
  return { status: res.status, body };
}

const unavailable = () => fail(503, "The VAT number check is not answering right now. Try again in a few minutes.", "vat_check_unavailable");

async function checkVies(vat) {
  let r;
  try {
    r = await getJson(config.tax.viesUrl, {
      method: "POST", headers: { "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify({ countryCode: vat.prefix === "XI" ? "XI" : vat.prefix, vatNumber: vat.number }),
    });
  } catch { unavailable(); }
  const b = r.body;
  if (r.status !== 200 || !b || b.actionSucceed === false || typeof b.valid !== "boolean") {
    const err = b?.errorWrappers?.[0]?.error;
    if (err === "INVALID_INPUT") return { valid: false };
    unavailable();
  }
  const text = (v) => (typeof v === "string" && v.trim() && v.trim() !== "---" ? v.trim().slice(0, 300) : null);
  return { valid: b.valid, name: text(b.name), address: text(b.address), checked_at: b.requestDate || new Date().toISOString(), source: "vies" };
}

let hmrcToken = null; // { value, until }
async function hmrcAccess() {
  if (hmrcToken && hmrcToken.until > Date.now()) return hmrcToken.value;
  let r;
  try {
    r = await getJson(`${config.tax.hmrc.base}/oauth/token`, {
      method: "POST", headers: { "content-type": "application/x-www-form-urlencoded", accept: "application/json" },
      body: new URLSearchParams({ grant_type: "client_credentials", client_id: config.tax.hmrc.clientId,
        client_secret: config.tax.hmrc.clientSecret }).toString(),
    });
  } catch { unavailable(); }
  if (r.status !== 200 || typeof r.body?.access_token !== "string") unavailable();
  hmrcToken = { value: r.body.access_token, until: Date.now() + Math.max(60, Number(r.body.expires_in || 3600) - 60) * 1000 };
  return hmrcToken.value;
}

async function checkHmrc(vat) {
  if (!config.tax.hmrc.clientId || !config.tax.hmrc.clientSecret) {
    fail(403, "UK VAT numbers cannot be verified on the web yet. Buy in the Pigeonpost iOS or Android app, or write to sales@pigeonpost.dev.", "uk_vat_check_unavailable");
  }
  if (!/^(\d{9}|\d{12})$/.test(vat.number)) return { valid: false };
  const token = await hmrcAccess();
  let r;
  try {
    r = await getJson(`${config.tax.hmrc.base}/organisations/vat/check-vat-number/lookup/${vat.number}`, {
      headers: { accept: "application/vnd.hmrc.2.0+json", authorization: `Bearer ${token}` },
    });
  } catch { unavailable(); }
  if (r.status === 404) return { valid: false };
  if (r.status !== 200 || !r.body?.target) unavailable();
  const a = r.body.target.address || {};
  const address = [a.line1, a.line2, a.line3, a.line4, a.postcode].filter((x) => typeof x === "string" && x.trim()).join(", ").slice(0, 300) || null;
  return { valid: true, name: typeof r.body.target.name === "string" ? r.body.target.name.slice(0, 300) : null, address,
    checked_at: r.body.processingDate || new Date().toISOString(), source: "hmrc" };
}

/// Verify a VAT number with the authority for its country. Valid answers are cached for a day.
export async function checkVatId(vat) {
  const hit = cache.get(vat.id);
  if (hit && Date.now() - hit.at < CACHE_MS) return hit.result;
  const result = vat.country === "GB" ? await checkHmrc(vat) : await checkVies(vat);
  if (result.valid) cache.set(vat.id, { result, at: Date.now() });
  return result;
}

// ---- the decision ---------------------------------------------------------------------------------

const APP_HINT = "Buy in the Pigeonpost iOS or Android app, where Apple or Google handles VAT, or buy as a business with a VAT number.";

/// The VAT treatment for a new web purchase, or a refusal (thrown as an HTTP error).
///
/// `profile` is the saas_be billing profile the checkout uses. The result is stored with the
/// checkout and drives the receipt wording.
export async function decide(profile, ip) {
  const billing = countryCode(profile?.address?.country);
  if (!billing) fail(400, "We could not recognise the country in your billing details. Enter it as a two-letter code, for example DE or TR.", "billing_country_unknown");
  const ipc = ip || null;
  const evidence = { billing_country: billing, ip_country: ipc };
  let region = regionOf(billing);
  if (!region && ipc && regionOf(ipc)) {
    // Contradictory evidence. A Turkish profile with a Turkish identity/tax number is Turkish
    // (commercially relevant information, Art. 24f(f)); anything else is treated as EU/UK.
    const turkishId = billing === "TR" && /^\d{10,11}$/.test(String(profile.identity_number || profile.tax_id || "").trim());
    if (!turkishId) region = regionOf(ipc);
  }
  if (!config.tax.gate || !region) return { treatment: "standard", ...evidence };

  const business = profile.account_type === "entity" && String(profile.tax_id || "").trim();
  if (business) {
    const vat = parseVatId(profile.tax_id, billing);
    if (!vat || vat.malformed || regionOf(vat.country) !== region || (regionOf(billing) && vat.country !== billing)) {
      fail(400, `Enter your company's ${region === "UK" ? "UK" : "EU"} VAT number in your billing details, matching your billing country (for example ${region === "UK" ? "GB123456789" : "DE123456789"}).`, "vat_id_invalid");
    }
    const check = await checkVatId(vat);
    if (!check.valid) fail(400, `${vat.id} is not a valid, active VAT number according to ${vat.country === "GB" ? "HMRC" : "VIES"}. Check it in your billing details.`, "vat_id_invalid");
    return { treatment: "reverse_charge", region, ...evidence, vat_id: vat.id, vat_country: vat.country,
      vat_name: check.name, vat_checked_at: check.checked_at, vat_source: check.source };
  }

  if (config.tax.b2cOpenRegions.includes(region)) {
    const country = regionOf(billing) === region ? billing : ipc;
    const rate = RATES[region]?.[country];
    if (typeof rate !== "number") fail(503, "Sales to this country are not open yet.", "vat_rate_missing");
    return { treatment: "b2c_vat", region, ...evidence, vat_country: country, vat_rate: rate };
  }
  fail(403, `We can't sell Pigeonpost Cloud on the web to consumers in the ${region === "UK" ? "UK" : "EU"} yet. ${APP_HINT}`, "eu_uk_consumer_web_unavailable");
}

// ---- receipt wording ------------------------------------------------------------------------------

export const WORDING = {
  reverse_charge_eu: "Reverse charge – VAT to be accounted for by the recipient, Art. 196 Council Directive 2006/112/EC.",
  reverse_charge_uk: "Reverse charge – the customer accounts for UK VAT (VAT Act 1994, section 8).",
  export_tr: "Hizmet ihracı – KDV Kanunu md. 11/1-a uyarınca KDV'den istisnadır (service export, exempt from Turkish VAT).",
};

/// The tax lines a receipt shows for a stored decision, with the buyer details it was made for.
export function receipt(tax, profile) {
  if (!tax) return null;
  const lines = [];
  if (tax.treatment === "reverse_charge") lines.push(tax.region === "UK" ? WORDING.reverse_charge_uk : WORDING.reverse_charge_eu);
  if (tax.treatment === "b2c_vat") lines.push(`Price includes ${tax.vat_rate}% VAT (${tax.vat_country}).`);
  if (tax.billing_country && tax.billing_country !== "TR" && tax.treatment !== "b2c_vat") lines.push(WORDING.export_tr);
  const name = profile ? (profile.account_type === "entity" ? profile.entity_name || profile.legal_name : profile.legal_name) : tax.vat_name;
  return {
    seller: "Piyote Bilişim ve Danışmanlık Hizmetleri Ticaret Ltd. Şti., Türkiye",
    customer: { name: name || null, country: tax.billing_country || null, vat_id: tax.vat_id || null },
    treatment: tax.treatment,
    lines,
  };
}
