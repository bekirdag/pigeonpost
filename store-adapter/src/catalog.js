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

/// The public price list (for the site and the enterprise portal).
export function publicCatalog() {
  const plans = Object.entries(catalog.plans).map(([code, p]) => {
    const years = Object.keys(p.editions).sort();
    const current = p.editions[years[years.length - 1]];
    return {
      code, family: p.family, currency: p.currency, interval: p.interval,
      ...(p.family === "cloud" ? { names: p.names } : { seats: p.seats, agent_limit: p.agent_limit, storage_gb: p.storage_gb }),
      amount: current.amount, pricing_year: Number(years[years.length - 1]),
      available: Boolean(current.plan_id),
    };
  });
  return { catalog_version: catalog.catalog_version, trial_period_days: catalog.trial_period_days,
    contact_sales_above_seats: 500, open_currencies: config.openCurrencies, plans };
}
