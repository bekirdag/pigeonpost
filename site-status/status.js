// status.pigeonpost.dev: reads /status.json (merged every minute from two locations by
// deploy/status/pp-status.py) and draws it with the design kit's status-help template: the overall
// hero, services by group with 90 hourly uptime bars, and incidents. Server text only ever goes
// through text nodes.
(function () {
  "use strict";
  const SVG = "http://www.w3.org/2000/svg";
  function el(tag, attrs, ...kids) {
    const n = document.createElement(tag);
    for (const [k, v] of Object.entries(attrs || {})) if (v !== null && v !== undefined) n.setAttribute(k, v);
    for (const k of kids.flat()) if (k !== null && k !== undefined && k !== false) n.append(k instanceof Node ? k : String(k));
    return n;
  }
  function icon(name) {
    const s = document.createElementNS(SVG, "svg");
    s.setAttribute("class", "pp-icon");
    s.setAttribute("aria-hidden", "true");
    const u = document.createElementNS(SVG, "use");
    u.setAttribute("href", `/kit/icons.svg#${name}`);
    s.append(u);
    return s;
  }
  // Component states -> kit data-status and label. "unknown" (no recent data) is muted.
  const STATE = {
    operational: ["operational", "Operational"],
    degraded: ["degraded", "Degraded"],
    down: ["down", "Outage"],
    unknown: ["queued", "No data"],
  };
  const HERO = {
    operational: ["operational", "i-circle-check", "All systems operational"],
    degraded: ["degraded", "i-alert-triangle", "Some systems are degraded"],
    down: ["down", "i-circle-x", "Some systems are down"],
  };
  const ago = (t) => {
    const s = Math.max(0, Math.round(Date.now() / 1000 - t));
    return s < 90 ? `${s} seconds ago` : `${Math.round(s / 60)} minutes ago`;
  };
  const pct = (v) => (v === null || v === undefined ? "" : `${v}%`);

  function bars(values) {
    return el("div", { class: "pp-uptime", "aria-hidden": "true" }, (values || []).map((v) => {
      const s = v === null || v === undefined ? "none" : v >= 99.5 ? null : v >= 90 ? "degraded" : "down";
      return el("span", { "data-status": s, title: s === "none" ? "No data" : `${v}% of checks passed` });
    }));
  }

  function hero(s) {
    const stale = Date.now() / 1000 - s.generated_at > 300;
    const [status, ic, title] = stale ? ["degraded", "i-clock", "Status data is out of date"] : (HERO[s.overall] || HERO.degraded);
    const down = (s.components || []).filter((c) => c.state === "down").map((c) => c.name);
    const from = s.checked_from ? `Checked from ${s.checked_from} location${s.checked_from === 1 ? "" : "s"}` : "Checked";
    const text = `${down.length ? `Affected: ${down.join(", ")}. ` : ""}${from}, ${ago(s.generated_at)}. Updated every minute.`;
    return el("div", { class: "pp-status-card pp-status-card--hero", "data-status": status, role: "status" },
      el("span", { class: "pp-status-card__icon" }, icon(ic)),
      el("div", { class: "pp-status-card__body" },
        el("p", { class: "pp-status-card__title" }, title),
        el("p", { class: "pp-status-card__text" }, text)));
  }

  function row(c) {
    const [status, label] = STATE[c.state] || STATE.degraded;
    return el("div", { class: "pp-status-row", "data-status": status },
      el("span", { class: "pp-status-row__name", title: c.description || null }, c.name, " ", el("small", {}, pct(c.uptime_24h))),
      el("span", { class: "pp-status", "data-status": status }, label),
      bars(c.bars),
      el("div", { class: "pp-status-row__foot" }, el("span", {}, "90 hours ago"), el("span", {}, "Now")));
  }

  function services(s) {
    const groups = new Map();
    for (const c of s.components || []) {
      if (!groups.has(c.group)) groups.set(c.group, []);
      groups.get(c.group).push(c);
    }
    return el("section", { class: "pp-stack" },
      el("div", { class: "st-head" }, el("h2", { class: "pp-h3" }, "Services"), el("span", { class: "pp-caption" }, "Uptime over the last 90 hours, last 24 hours shown by each name")),
      [...groups].map(([g, cs]) => el("div", { class: "st-group" }, el("h3", {}, g), el("div", { class: "pp-status-list" }, cs.map(row)))));
  }

  function incidents(s) {
    const list = Array.isArray(s.incidents) ? s.incidents : [];
    return el("section", { class: "pp-stack pp-stack--s" },
      el("h2", { class: "pp-h3" }, "Incidents"),
      list.length ? list.map((i) => {
        const resolved = i.state === "resolved";
        return el("div", { class: "pp-status-card", "data-status": resolved ? "operational" : "degraded" },
          el("span", { class: "pp-status-card__icon" }, icon(resolved ? "i-circle-check" : "i-alert-triangle")),
          el("div", { class: "pp-status-card__body" },
            el("p", { class: "pp-status-card__title" }, String(i.title || "Incident"), i.state ? ` · ${i.state}` : ""),
            i.text ? el("p", { class: "pp-status-card__text" }, String(i.text)) : null,
            i.updated ? el("p", { class: "pp-status-card__meta" }, String(i.updated)) : null));
      }) : el("div", { class: "pp-status-card", "data-status": "operational" },
        el("span", { class: "pp-status-card__icon" }, icon("i-circle-check")),
        el("div", { class: "pp-status-card__body" }, el("p", { class: "pp-status-card__text" }, "No incidents reported in the last 7 days."))));
  }

  function draw(s) {
    document.getElementById("st-root").replaceChildren(hero(s), services(s), incidents(s));
  }

  async function load() {
    try {
      const r = await fetch("/status.json", { cache: "no-store" });
      if (!r.ok) throw new Error(String(r.status));
      draw(await r.json());
    } catch {
      document.getElementById("st-root").replaceChildren(el("div", { class: "pp-status-card pp-status-card--hero", "data-status": "degraded", role: "status" },
        el("span", { class: "pp-status-card__icon" }, icon("i-alert-triangle")),
        el("div", { class: "pp-status-card__body" },
          el("p", { class: "pp-status-card__title" }, "Status is unavailable right now"),
          el("p", { class: "pp-status-card__text" }, "Try again in a minute."))));
    }
  }
  load();
  window.setInterval(load, 60000);
})();
