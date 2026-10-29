// The approved Pigeonpost catalog: plan codes, what each grants, and the immutable editions
// provisioned in saas_be/iyzico (local plan id and provider plan reference per pricing year).
//
// Every saas_be offer and subscription is checked against this file. An edition that is not
// listed here (a new pricing year, a changed price) fails closed until it is reviewed and added.

import { readFileSync } from "node:fs";
import { config } from "./config.js";

const source = process.env.PIGEONPOST_CATALOG_FILE
  || new URL("../catalog/pigeonpost-catalog.json", import.meta.url);
export const catalog = JSON.parse(readFileSync(source, "utf8"));

const CODE = /^(cloud-(10|[1-9])names-yearly|ent-(5|10|15|20|30|50|100|200|500)-(monthly|yearly))-(usd|try)$/;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const AMOUNT = /^\d{1,8}\.\d{2}$/;

for (const [code, plan] of Object.entries(catalog.plans)) {
  if (!CODE.test(code)) throw new Error(`catalog: bad plan code ${code}`);
  if (plan.currency !== code.slice(-3).toUpperCase()) throw new Error(`catalog: ${code} currency`);
  for (const [year, edition] of Object.entries(plan.editions || {})) {
    if (!/^20\d\d$/.test(year) || !AMOUNT.test(edition.amount)) throw new Error(`catalog: ${code} ${year}`);
    if (edition.plan_id !== null && !UUID.test(edition.plan_id)) throw new Error(`catalog: ${code} plan id`);
  }
}

export const isUuid = (value) => typeof value === "string" && UUID.test(value);

export function plan(code) {
  const found = typeof code === "string" && Object.hasOwn(catalog.plans, code) ? catalog.plans[code] : null;
  return found ? { code, ...found } : null;
}

/// The plan an edition (local saas_be plan id) belongs to, with that edition's year.
export function planForLocalId(localPlanId) {
  for (const [code, entry] of Object.entries(catalog.plans)) {
    for (const [year, edition] of Object.entries(entry.editions)) {
      if (edition.plan_id && edition.plan_id === localPlanId) return { ...plan(code), year, edition };
    }
  }
  return null;
}

/// The plan a provider pricing-plan reference belongs to.
export function planForProviderReference(reference) {
  for (const [code, entry] of Object.entries(catalog.plans)) {
    for (const [year, edition] of Object.entries(entry.editions)) {
      if (edition.provider_plan_reference && edition.provider_plan_reference === reference) {
        return { ...plan(code), year, edition };
      }
    }
  }
  return null;
}

/// The plan family and limits a saas_be package slug stands for (`pp-cloud-3names`,
/// `pp-ent-30-yearly`). Used when a renewal moved to an edition this file does not list yet.
export function limitsForPackage(slug) {
  const entry = Object.values(catalog.plans).find((p) => p.package_slug === slug);
  if (!entry) return null;
  return entry.family === "cloud"
    ? { family: "cloud", names: entry.names, interval: entry.interval }
    : { family: "ent", seats: entry.seats, agent_limit: entry.agent_limit, storage_gb: entry.storage_gb, interval: entry.interval };
}

/// The commercial calendar year (Europe/Istanbul), as saas_be's regional pricing uses it.
export function pricingYear(now = new Date()) {
  return Number(new Intl.DateTimeFormat("en-US", { year: "numeric", timeZone: "Europe/Istanbul" }).format(now));
}

/// The edition on sale now: the latest year that has started. A later year (the next TRY
/// edition, provisioned ahead of January 1) is announced as `next`, never sold early.
export function currentEdition(editions, now = new Date()) {
  const years = Object.keys(editions).map(Number).sort((a, b) => a - b);
  const started = years.filter((y) => y <= pricingYear(now));
  const year = started.length ? started[started.length - 1] : years[0];
  const next = years.find((y) => y > year);
  return { year, edition: editions[String(year)], next: next ? { year: next, edition: editions[String(next)] } : null };
}

/// The public price list (for the site and the enterprise portal).
export function publicCatalog(now = new Date()) {
  const plans = Object.entries(catalog.plans).map(([code, p]) => {
    const { year, edition: current, next } = currentEdition(p.editions, now);
    return {
      code, family: p.family, currency: p.currency, interval: p.interval,
      ...(p.family === "cloud" ? { names: p.names } : { seats: p.seats, agent_limit: p.agent_limit, storage_gb: p.storage_gb }),
      amount: current.amount, pricing_year: year,
      available: Boolean(current.plan_id),
      ...(next ? { next_amount: next.edition.amount, next_pricing_year: next.year } : {}),
    };
  });
  return { catalog_version: catalog.catalog_version, trial_period_days: catalog.trial_period_days,
    contact_sales_above_seats: 500, open_currencies: config.openCurrencies, plans };
}
