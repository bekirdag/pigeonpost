// The handle grammar, as the protocol defines it.
//
// This is a faithful port of `canonical_handle` in crates/pigeonpost-core/src/address.rs. It exists
// because the account page used to accept `a-z0-9` only, which refuses three of the four shapes the
// product now mints: `/pp/alex`, `alex@example.com`, and `/github/alex/agent1`.
//
// The rule that matters: a name this accepts may still be refused by the postbox (reserved, taken,
// or not yours to hang an agent under) — but a name this *rejects* must always be refused there too.
// Drift in the other direction shows somebody a green tick and then an error after they pay.
//
// site-inbox/handle.js is a byte-identical copy, kept honest by a test. Edit this one.
window.PIGEONPOST_HANDLE = (function () {
  "use strict";

  // Byte ceilings, from the Rust constants of the same names.
  const MAX_HANDLE = 320;
  const MAX_NAMESPACE = 32;
  const MAX_NAME = 39;
  const MAX_ADDRESS = 254;

  const isAlnum = (c) => (c >= "a" && c <= "z") || (c >= "A" && c <= "Z") || (c >= "0" && c <= "9");

  // What a namespace or a plain name may contain.
  const plainChar = (c) => isAlnum(c) || c === "-" || c === "_" || c === ".";

  // RFC 5322's dot-atom set, less the four this format cannot carry: `/` separates segments, `?`
  // opens a loft hint, `#` opens a capability token, and `%` can survive one decoding too many and
  // turn one name silently into another.
  const LOCAL_EXTRA = "!$&'*+-=^_`{|}~.";
  const localChar = (c) => isAlnum(c) || LOCAL_EXTRA.indexOf(c) >= 0;

  // A name is a thing people read out; one that begins with a dash or ends with a dot is a mistake
  // being preserved.
  const edgeMarked = (s) => /^[-.@]/.test(s) || /[-.@]$/.test(s);

  const isAscii = (s) => {
    for (let i = 0; i < s.length; i++) if (s.charCodeAt(i) > 0x7f) return false;
    return true;
  };
  const bytes = (s) => s.length; // ASCII-only by the time this is asked

  const every = (s, pred) => {
    for (let i = 0; i < s.length; i++) if (!pred(s[i])) return false;
    return true;
  };

  // Whether this segment is claiming to be an address rather than a word. Deliberately cheap: it
  // decides which set of rules applies, and validateAddress does the judging.
  function looksLikeAddress(segment) {
    const at = segment.indexOf("@");
    if (at < 0) return false;
    const local = segment.slice(0, at);
    const domain = segment.slice(at + 1);
    return local.length > 0 && domain.indexOf(".") >= 0;
  }

  function validateAddress(segment) {
    if (bytes(segment) > MAX_ADDRESS) return "that address is too long";
    if ((segment.match(/@/g) || []).length > 1) return "an address has one @";
    const at = segment.indexOf("@");
    const local = segment.slice(0, at);
    const domain = segment.slice(at + 1);
    if (!local || !domain) return "an address needs a name and a domain";
    if (!every(local, localChar)) return "that address has characters a handle cannot carry";
    if (edgeMarked(local)) return "an address cannot start or end with a dot or dash";
    // At least two labels: `alex@localhost` names nothing anybody else can reach.
    const labels = domain.split(".");
    if (labels.length < 2) return "that domain is not a full domain";
    for (const label of labels) {
      if (!label || label.length > 63) return "that domain is not a full domain";
      if (!every(label, (c) => isAlnum(c) || c === "-")) return "that domain has characters it cannot have";
      if (label.startsWith("-") || label.endsWith("-")) return "a domain label cannot start or end with a dash";
    }
    return null;
  }

  function validatePlain(segment, max, what) {
    if (!segment) return `a ${what} cannot be empty`;
    if (bytes(segment) > max) return `that ${what} is too long (at most ${max} characters)`;
    if (!every(segment, plainChar)) return `a ${what} uses letters, digits, dot, dash and underscore`;
    if (edgeMarked(segment)) return `a ${what} cannot start or end with a dot or dash`;
    return null;
  }

  // The whole grammar. Returns { ok: true, handle } or { ok: false, reason }.
  function canonical(input) {
    const raw = String(input == null ? "" : input).trim();
    if (!raw) return { ok: false, reason: "", silent: true };
    if (!isAscii(raw)) return { ok: false, reason: "a handle is ASCII only" };
    if (bytes(raw) > MAX_HANDLE) return { ok: false, reason: "that handle is too long" };

    const trimmed = raw.startsWith("/") ? raw.slice(1) : raw;
    const segments = trimmed.split("/");
    // A namespace, a person, and optionally that person's agent. Four segments is not a deeper
    // fleet, it is a typo.
    if (segments.length > 3) return { ok: false, reason: "a handle has at most three parts" };

    // A single segment is a handle only when it is an address. `/bekir` on its own is a namespace,
    // and resolving it is somebody else's decision.
    if (segments.length === 1) {
      const only = segments[0];
      if (!looksLikeAddress(only)) return { ok: false, reason: "expected /namespace/name" };
      const bad = validateAddress(only);
      if (bad) return { ok: false, reason: bad };
      return { ok: true, handle: "/" + only.toLowerCase() };
    }

    const head = segments[0];
    if (head.toLowerCase() === "gh") return { ok: false, reason: "use /github, not /gh" };
    if (looksLikeAddress(head)) {
      const bad = validateAddress(head);
      if (bad) return { ok: false, reason: bad };
    } else {
      // A namespace is a word somebody owns or a provider's name, and neither is an address.
      const bad = validatePlain(head, MAX_NAMESPACE, "namespace");
      if (bad) return { ok: false, reason: bad };
    }

    for (const segment of segments.slice(1)) {
      if (looksLikeAddress(segment)) {
        const bad = validateAddress(segment);
        if (bad) return { ok: false, reason: bad };
        continue;
      }
      const bad = validatePlain(segment, MAX_NAME, "name");
      if (bad) return { ok: false, reason: bad };
    }

    return { ok: true, handle: "/" + segments.map((s) => s.toLowerCase()).join("/") };
  }

  // A namespace that belongs to everybody rather than to one person. Mirrors PROVIDER_NAMESPACES
  // in crates/pigeonpost-cli/src/onboard_cmd.rs.
  const PROVIDER_NAMESPACES = ["github", "google", "pp"];

  // Which namespace *your* agents live under — the question onboarding asks.
  //
  // `/bekir/main` → `/bekir`: the first segment is the account's own namespace and its agents are
  // its siblings. `/github/alex` → `/github/alex`: `/github` belongs to everybody, so the person is
  // the whole handle and their agents are its children. Both are one person; the difference is only
  // which segment says so.
  function namespaceOf(handle) {
    const parts = String(handle == null ? "" : handle).replace(/^\/+/, "").split("/");
    if (!parts.length || !parts[0]) return null;
    if (parts[0].indexOf("@") >= 0) return "/" + parts[0];           // an address is a person
    if (parts.length >= 2 && PROVIDER_NAMESPACES.indexOf(parts[0]) >= 0) {
      return "/" + parts[0] + "/" + parts[1];
    }
    return "/" + parts[0];
  }

  // What to trust so a whole fleet of yours is covered: your namespace, plus `/*`.
  function fleetWildcard(handle) {
    const ns = namespaceOf(handle);
    return ns ? ns + "/*" : null;
  }

  // Which contact entry covers a given peer — the postbox's `namespace_wildcard`. Drops the last
  // segment and wildcards the parent. Deliberately *not* the first segment: `/github/*` would trust
  // every GitHub user alive because one of them was trusted.
  function coverageWildcard(peer) {
    const rest = String(peer == null ? "" : peer).replace(/^\//, "");
    const cut = rest.lastIndexOf("/");
    if (cut < 0) return null;
    const parent = rest.slice(0, cut);
    const name = rest.slice(cut + 1);
    if (!parent || !name || parent === "k") return null;
    return "/" + parent + "/*";
  }

  return {
    canonical, namespaceOf, fleetWildcard, coverageWildcard, looksLikeAddress,
    MAX_NAME, MAX_NAMESPACE,
  };
})();
