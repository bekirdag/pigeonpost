// Client-side reserved-name check, for instant feedback only.
//
// The registry is authoritative — it enforces the full docs/reserved-names.md at claim time. This is
// the fast-feedback subset so the buyer sees "reserved" before submitting, not after paying. Keep it
// a strict subset of the server list: a name this allows may still be refused server-side, but a name
// this rejects must always be refused there too.
window.PIGEONPOST_RESERVED = (function () {
  // Every 1- and 2-character string is reserved.
  const shortIsReserved = (name) => name.length <= 2;

  // Live and candidate provider namespaces (spelled out; abbreviations are the tempting flat names).
  const namespaces = new Set([
    "k", "github", "google", "email",
    "x", "twitter", "gitlab", "bitbucket", "facebook", "instagram", "threads",
    "linkedin", "tiktok", "reddit", "mastodon", "bluesky", "bsky", "discord",
    "slack", "telegram", "signal", "whatsapp", "youtube", "twitch", "apple",
    "microsoft", "amazon", "meta", "npm", "pypi", "docker", "stackoverflow",
    "okta", "auth0", "orcid", "keybase",
  ]);

  const operational = new Set([
    "admin", "administrator", "root", "system", "api", "www", "web", "app",
    "mail", "email", "smtp", "postmaster", "webmaster", "noreply", "support",
    "help", "contact", "info", "sales", "billing", "security", "abuse", "legal",
    "privacy", "compliance", "dmca", "docs", "status", "login", "signin",
    "signup", "register", "auth", "sso", "token", "test", "staging", "demo",
    "sandbox", "example", "null", "none", "blog", "news", "about", "terms",
    "pricing", "checkout", "payment", "search", "home", "index", "default",
    "public", "private", "internal",
  ]);

  const brand = new Set([
    "pigeonpost", "pigeon", "loft", "registry", "directory", "witness",
    "official", "verified", "staff", "team", "bot",
    "paypal", "stripe", "visa", "mastercard", "netflix", "tesla", "nvidia",
    "spotify", "uber", "airbnb", "zoom", "figma", "notion", "openai",
    "anthropic", "claude", "gemini", "chatgpt", "copilot",
  ]);

  const abuse = new Set([
    "verify", "verification", "verified", "confirm", "validate", "secure",
    "update", "renew", "refund", "invoice", "wallet", "claim", "reward",
    "prize", "giveaway", "airdrop", "bonus", "free", "winner", "recovery",
    "unlock", "activate",
  ]);

  // Fold digit look-alikes so g00gle and paypa1 collide with the real thing.
  function fold(name) {
    return name.replace(/0/g, "o").replace(/1/g, "l").replace(/5/g, "s").replace(/3/g, "e");
  }

  function reason(nameRaw) {
    const name = String(nameRaw || "").toLowerCase();
    if (!name) return null;
    if (shortIsReserved(name)) return "one- and two-character names are reserved";
    const f = fold(name);
    if (namespaces.has(name) || namespaces.has(f)) return "reserved for a provider namespace";
    if (operational.has(name) || operational.has(f)) return "reserved (operational name)";
    if (brand.has(name) || brand.has(f)) return "reserved (brand or trademark)";
    if (abuse.has(name) || abuse.has(f)) return "reserved (too useful for fraud)";
    return null;
  }

  return { reason, isReserved: (n) => reason(n) !== null };
})();
