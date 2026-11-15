// status.pigeonpost.dev: reads /status.json (written every minute by deploy/status/pp-status.py)
// and draws the overall state, each component with its last 90 hours, and incidents.
// Server text only ever goes through text nodes (PP.el).
(function () {
  "use strict";
  const el = (...a) => window.PP.el(...a);
  const STATE = {
    operational: ["Operational", "success"],
    degraded: ["Degraded", "warning"],
    down: ["Outage", "danger"],
  };
  const HEADLINE = {
    operational: "All systems operational",
    degraded: "Some systems are degraded",
    down: "Some systems are down",
  };
  const ago = (t) => {
    const s = Math.max(0, Math.round(Date.now() / 1000 - t));
    return s < 90 ? `${s} seconds ago` : `${Math.round(s / 60)} minutes ago`;
  };
  function bar(v) {
    const tone = v === null || v === undefined ? "none" : v >= 99.5 ? "ok" : v >= 90 ? "warn" : "bad";
    return el("span", { class: `st-bar st-${tone}`, title: v === null || v === undefined ? "No data" : `${v}% of checks passed` });
  }
  function draw(s) {
    const root = document.getElementById("st-root");
    const [, tone] = STATE[s.overall] || STATE.degraded;
    const stale = Date.now() / 1000 - s.generated_at > 300;
    const groups = new Map();
    for (const c of s.components || []) {
      if (!groups.has(c.group)) groups.set(c.group, []);
      groups.get(c.group).push(c);
    }
    root.replaceChildren(...[
      el("section", { class: `st-banner pp-tone-${stale ? "warning" : tone}` },
        el("h1", {}, stale ? "Status data is out of date" : HEADLINE[s.overall] || HEADLINE.degraded),
        el("p", {}, `Checked ${ago(s.generated_at)}. Updated every minute.`)),
      (s.incidents || []).length ? el("section", { class: "st-incidents" }, el("h2", {}, "Incidents"),
        s.incidents.map((i) => el("article", { class: "pp-card" },
          el("div", { class: "pp-card-head" }, el("h3", {}, String(i.title || "Incident")), i.state ? window.PP.el("span", { class: `pp-badge pp-tone-${i.state === "resolved" ? "success" : "warning"}` }, String(i.state)) : null),
          i.text ? el("p", {}, String(i.text)) : null,
          i.updated ? el("p", { class: "pp-faint" }, String(i.updated)) : null))) : null,
      [...groups].map(([g, cs]) => el("section", { class: "st-group" },
        el("h2", {}, g),
        el("ul", { class: "st-list" }, cs.map((c) => {
          const [label, t] = STATE[c.state] || STATE.degraded;
          return el("li", { class: "st-item" },
            el("div", { class: "st-row" },
              el("strong", {}, c.name),
              el("span", { class: `pp-badge pp-tone-${t}` }, label)),
            el("div", { class: "st-bars", "aria-label": `${c.name}: last 90 hours` }, (c.bars || []).map(bar)),
            el("div", { class: "st-meta pp-faint" },
              el("span", {}, "90 hours ago"),
              el("span", {}, c.uptime_24h === null || c.uptime_24h === undefined ? "" : `${c.uptime_24h}% in the last 24 hours`),
              el("span", {}, "Now")));
        }))))].flat().filter(Boolean));
  }
  async function load() {
    try {
      const r = await fetch("/status.json", { cache: "no-store" });
      if (!r.ok) throw new Error(String(r.status));
      draw(await r.json());
    } catch {
      document.getElementById("st-root").replaceChildren(el("section", { class: "st-banner pp-tone-warning" }, el("h1", {}, "Status is unavailable right now"), el("p", {}, "Try again in a minute.")));
    }
  }
  load();
  window.setInterval(load, 60000);
})();
