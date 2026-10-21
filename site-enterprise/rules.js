// Pigeonpost for teams — org rules as sentences (SDS §9.4). Pure model: rule <-> editor form <->
// plain sentence. The page in app.js draws the pickers; this file decides what they mean.
(function () {
  "use strict";
  const PPE = (window.PPE = window.PPE || {});
  const R = (PPE.rules = {});

  R.NEVER_AUTO = ["git_push", "deploy", "read_credentials", "spend", "delete_files", "run_shell"];
  R.KINDS = ["everyone", "members", "agents", "anyone", "address"];

  /// The pattern a picker choice stands for.
  R.patternOf = function (kind, custom, ns) {
    switch (kind) {
      case "everyone": return `/${ns}/*`;
      case "members": return "members";
      case "agents": return "agents";
      case "anyone": return "*";
      default: return String(custom || "").trim();
    }
  };

  /// The picker choice a pattern reads as.
  R.kindOf = function (pattern, ns) {
    if (pattern === `/${ns}/*`) return { kind: "everyone", custom: "" };
    if (pattern === "members" || pattern === "agents") return { kind: pattern, custom: "" };
    if (pattern === "*") return { kind: "anyone", custom: "" };
    return { kind: "address", custom: pattern || "" };
  };

  R.label = function (pattern, orgName, ns, { subject = false } = {}) {
    const k = R.kindOf(pattern, ns);
    const cap = (s) => (subject ? s.charAt(0).toUpperCase() + s.slice(1) : s);
    switch (k.kind) {
      case "everyone": return cap(`everyone at ${orgName}`);
      case "members": return cap(`people at ${orgName}`);
      case "agents": return cap(`agents at ${orgName}`);
      case "anyone": return cap("anyone");
      default: return k.custom;
    }
  };

  /// Is a pattern acceptable for this side? Owners stay inside the namespace (SDS §9.5).
  R.validPattern = function (pattern, ns, side) {
    if (["members", "agents"].includes(pattern)) return true;
    if (pattern === "*") return side === "peers";
    const m = /^\/([a-z0-9][a-z0-9._-]{0,31})(\/[a-z0-9][a-z0-9._-]*\*?|\/\*)?$/.exec(pattern);
    if (!m || !m[2]) return false;
    if (side === "owners" && m[1] !== ns) return false;
    return true;
  };

  R.toForm = function (rule, ns) {
    const o = R.kindOf(rule.owners, ns);
    const p = R.kindOf(rule.peers, ns);
    return {
      id: rule.id || null,
      ownersKind: o.kind, ownersCustom: o.custom,
      peersKind: p.kind, peersCustom: p.custom,
      admission: rule.admission || "",
      known: Boolean(rule.known),
      autonomy: rule.autonomy || "",
      verbs: Array.isArray(rule.verbs) ? rule.verbs.join(", ") : "",
      locked: Boolean(rule.locked),
      priority: Number.isInteger(rule.priority) ? rule.priority : 100,
      description: rule.description || "",
    };
  };

  /// Form -> rule body, or `{ error }`.
  R.fromForm = function (form, ns) {
    const owners = R.patternOf(form.ownersKind, form.ownersCustom, ns);
    const peers = R.patternOf(form.peersKind, form.peersCustom, ns);
    if (!R.validPattern(owners, ns, "owners")) return { error: `Who the rule is for must be inside /${ns}.` };
    if (!R.validPattern(peers, ns, "peers")) return { error: "Who they hear from is not a valid address or pattern." };
    const verbs = String(form.verbs || "").split(/[\s,]+/).map((v) => v.trim()).filter(Boolean);
    if (verbs.some((v) => !/^[a-z][a-z0-9_]{0,63}$/.test(v))) return { error: "Actions are lowercase words like run_tests." };
    if (form.autonomy === "auto" && verbs.some((v) => R.NEVER_AUTO.includes(v))) {
      return { error: `These always need a person: ${verbs.filter((v) => R.NEVER_AUTO.includes(v)).join(", ")}.` };
    }
    if (form.autonomy === "auto" && !verbs.length) return { error: "List the actions agents may take on their own." };
    if (form.autonomy === "auto" && !["agents", "address"].includes(form.ownersKind)) {
      return { error: "Only agents can be set to act on their own; people always decide for themselves." };
    }
    const priority = Number(form.priority);
    if (!Number.isInteger(priority) || priority < 1 || priority > 1000) return { error: "Priority is a whole number from 1 to 1000." };
    if (!form.admission && !form.known && !form.autonomy) return { error: "The rule doesn't change anything yet." };
    return {
      owners, peers,
      admission: form.admission || null,
      known: Boolean(form.known),
      autonomy: form.autonomy || null,
      verbs: form.autonomy === "auto" || verbs.length ? verbs : null,
      locked: Boolean(form.locked),
      priority,
      ...(form.description ? { description: String(form.description).slice(0, 200) } : {}),
    };
  };

  /// One plain sentence, as the card and the audit view show it.
  R.sentence = function (rule, orgName, ns) {
    const who = R.label(rule.owners, orgName, ns, { subject: true });
    const from = R.label(rule.peers, orgName, ns);
    const parts = [];
    if (rule.admission === "block") parts.push(`${who} block messages from ${from}`);
    else if (rule.admission === "allow") parts.push(`${who} accept messages from ${from}`);
    else parts.push(`For ${who.charAt(0).toLowerCase() + who.slice(1)}, ${from}`);
    let s = parts[0];
    if (rule.known && rule.admission !== "block") s += rule.admission ? " as a known contact" : " is a known contact";
    if (rule.autonomy === "auto" && Array.isArray(rule.verbs) && rule.verbs.length) s += `, and may ask them to ${rule.verbs.join(", ")} without review`;
    else if (rule.autonomy === "review") s += ", and every request waits for a person";
    s += ".";
    if (rule.locked) s += " Locked for agents.";
    return s;
  };

  /// The portal's templates (docs/orgs.md §6).
  R.templates = function (ns) {
    return [
      { key: "known", title: "Everyone at the company is known", rules: [
        { owners: `/${ns}/*`, peers: `/${ns}/*`, admission: "allow", known: true, autonomy: null, verbs: null, locked: false, priority: 100, description: "Everyone at the company is known" }] },
      { key: "direct", title: "Let a person direct an agent", rules: [
        { owners: "", peers: "", admission: "allow", known: true, autonomy: "auto", verbs: ["report_status"], locked: false, priority: 80, description: "A person directs an agent" }], needsEdit: true },
      { key: "inside", title: "Agents only talk to the company", rules: [
        { owners: "agents", peers: `/${ns}/*`, admission: "allow", known: false, autonomy: null, verbs: null, locked: true, priority: 50, description: "Agents hear from the company" },
        { owners: "agents", peers: "*", admission: "block", known: false, autonomy: null, verbs: null, locked: true, priority: 100, description: "Agents ignore everyone else" }] },
    ];
  };
})();
