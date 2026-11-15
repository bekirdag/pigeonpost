// status.pigeonpost.dev: reads /status.json (written every minute by deploy/status/pp-status.py)
// and draws it on the design kit's status template (templates/status-help.html): the overall
// state as a hero card, each group's components as a status list with uptime bars, and incidents.
//
// Components are drawn generically by their `group`: no component key is named here, so new
// components and new optional fields (description, checked_from, latency_ms, uptime_7d, …) show
// without a change. Server text only ever goes through text nodes (PP.el).
(function () {
  "use strict";
  const el = (...a) => window.PP.el(...a);
  const STATE = { operational: "Operational", degraded: "Degraded", down: "Outage", maintenance: "Maintenance" };
  const HEADLINE = {
    operational: "All systems operational",
    degraded: "Some systems are degraded",
    down: "Some systems are down",
    maintenance: "Scheduled maintenance in progress",
  };
  const ICON = { operational: "M20 6 9 17l-5-5", degraded: "M12 4 2.5 20h19ZM12 10v4M12 17h.01", down: "M12 4 2.5 20h19ZM12 10v4M12 17h.01", maintenance: "M4 4h16v6H4zM4 14h16v6H4zM8 7h.01M8 17h.01" };
  const known = (s) => (STATE[s] ? s : "degraded");
  const ago = (t) => {
    const s = Math.max(0, Math.round(Date.now() / 1000 - t));
    return s < 90 ? `${s} seconds ago` : s < 5400 ? `${Math.round(s / 60)} minutes ago` : `${Math.round(s / 3600)} hours ago`;
  };
  const pct = (v) => (typeof v === "number" && Number.isFinite(v) ? `${Math.round(v * 100) / 100}%` : null);

  function icon(state) {
    const NS = "http://www.w3.org/2000/svg";
    const svg = document.createElementNS(NS, "svg");
    svg.setAttribute("class", "pp-icon");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("aria-hidden", "true");
    for (const d of ICON[state].split(/(?=M)/)) {
      const p = document.createElementNS(NS, "path");
      p.setAttribute("d", d);
      svg.append(p);
    }
    return svg;
  }

  /// One bar per hour: its share of passed checks (null when there is no data).
  function bar(v) {
    const st = v === null || v === undefined ? "none" : v >= 99.5 ? "operational" : v >= 90 ? "degraded" : "down";
    return el("span", { "data-status": st, title: st === "none" ? "No data" : `${v}% of checks passed` });
  }

  function row(c) {
    const st = known(c.state);
    const bars = Array.isArray(c.bars) ? c.bars : [];
    const facts = [
      c.description ? String(c.description) : null,
      typeof c.checked_from === "number" ? `checked from ${c.checked_from} ${c.checked_from === 1 ? "location" : "locations"}` : null,
      typeof c.latency_ms === "number" && c.latency_ms >= 5 ? `${Math.round(c.latency_ms)} ms` : null,
      pct(c.uptime_7d) ? `${pct(c.uptime_7d)} in 7 days` : null,
    ].filter(Boolean);
    return el("div", { class: "pp-status-row", "data-status": st },
      el("span", { class: "pp-status-row__name" }, String(c.name || c.key || "Service"), pct(c.uptime_24h) ? el("small", {}, `${pct(c.uptime_24h)} in 24 hours`) : null),
      el("span", { class: "pp-status", "data-status": st }, STATE[st]),
      bars.length ? el("div", { class: "pp-uptime", role: "img", "aria-label": `${c.name || "Service"}: last ${bars.length} hours` }, bars.map(bar)) : null,
      el("div", { class: "pp-status-row__foot" },
        el("span", {}, bars.length ? `${bars.length} hours ago` : ""),
        facts.length ? el("span", { class: "st-facts" }, facts.join(" · ")) : null,
        el("span", {}, "Now")));
  }

  function incident(i) {
    const resolved = String(i.state || "").toLowerCase() === "resolved";
    const updates = Array.isArray(i.updates) ? i.updates : [];
    return el("article", { class: "pp-card st-incident" },
      el("div", { class: "pp-card-head pp-card__header" },
        el("div", { class: "pp-card__heading" }, el("h3", { class: "pp-card__title" }, String(i.title || "Incident")), i.updated ? el("p", { class: "pp-card__subtitle pp-muted" }, String(i.updated)) : null),
        i.state ? el("span", { class: `pp-badge pp-badge--dot pp-badge--${resolved ? "success" : "warning"} pp-tone-${resolved ? "success" : "warning"}` }, String(i.state)) : null),
      i.text ? el("p", {}, String(i.text)) : null,
      updates.length ? el("ol", { class: "pp-timeline" }, updates.map((u) => el("li", { class: "pp-timeline__item" },
        el("span", { class: "pp-timeline__marker" }),
        el("div", { class: "pp-timeline__content" },
          el("div", { class: "pp-timeline__head" }, el("span", { class: "pp-timeline__title" }, String(u.state || u.title || "Update")), u.at || u.time ? el("time", { class: "pp-timeline__time" }, String(u.at || u.time)) : null),
          u.text ? el("p", { class: "pp-timeline__text" }, String(u.text)) : null)))) : null);
  }

  function draw(s) {
    const root = document.getElementById("st-root");
    const stale = !s.generated_at || Date.now() / 1000 - s.generated_at > 300;
    const overall = stale ? "degraded" : known(s.overall);
    const groups = new Map();
    for (const c of s.components || []) {
      const g = String(c.group || "Services");
      if (!groups.has(g)) groups.set(g, []);
      groups.get(g).push(c);
    }
    const incidents = Array.isArray(s.incidents) ? s.incidents : [];
    const open = incidents.filter((i) => String(i.state || "").toLowerCase() !== "resolved");
    const past = incidents.filter((i) => String(i.state || "").toLowerCase() === "resolved");
    const hours = Math.max(0, ...(s.components || []).map((c) => (Array.isArray(c.bars) ? c.bars.length : 0)));
    root.replaceChildren(...[
      el("section", { class: "pp-status-card pp-status-card--hero", "data-status": overall, role: "status" },
        el("span", { class: "pp-status-card__icon" }, icon(overall)),
        el("div", { class: "pp-status-card__body" },
          el("h1", { class: "pp-status-card__title" }, stale ? "Status data is out of date" : HEADLINE[overall]),
          el("p", { class: "pp-status-card__text" }, s.generated_at ? `Checked ${ago(s.generated_at)}. Updated every minute.` : "Updated every minute."))),
      open.length ? el("section", { class: "pp-stack st-section" }, el("h2", { class: "pp-h3" }, "Incidents"), open.map(incident)) : null,
      [...groups].map(([g, cs]) => el("section", { class: "pp-stack st-section" },
        el("div", { class: "st-section-head" }, el("h2", { class: "pp-h3" }, g), hours ? el("span", { class: "pp-caption" }, `Uptime over the last ${hours} hours`) : null),
        el("div", { class: "pp-status-list" }, cs.map(row)))),
      past.length ? el("section", { class: "pp-stack st-section" }, el("h2", { class: "pp-h3" }, "Past incidents"), past.map(incident)) : null,
    ].flat().filter(Boolean));
  }

  async function load() {
    try {
      const r = await fetch("/status.json", { cache: "no-store" });
      if (!r.ok) throw new Error(String(r.status));
      draw(await r.json());
    } catch {
      document.getElementById("st-root").replaceChildren(el("section", { class: "pp-status-card pp-status-card--hero", "data-status": "degraded", role: "status" },
        el("span", { class: "pp-status-card__icon" }, icon("degraded")),
        el("div", { class: "pp-status-card__body" }, el("h1", { class: "pp-status-card__title" }, "Status is unavailable right now"), el("p", { class: "pp-status-card__text" }, "Try again in a minute."))));
    }
  }
  load();
  window.setInterval(load, 60000);
})();
