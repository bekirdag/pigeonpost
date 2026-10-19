import http from "node:http";
const ACCOUNTS = { "tok-alice": [{ address: "/k/alice0000000000000000000a", handle: "/alice" }], "tok-bob": [{ address: "/k/bob00000000000000000000000b", handle: "/bob" }] };
http.createServer((req, res) => {
  const owned = ACCOUNTS[(req.headers.authorization || "").replace(/^Bearer /, "")];
  const u = new URL(req.url, "http://x");
  const out = (s, b) => { res.writeHead(s, { "content-type": "application/json" }); res.end(JSON.stringify(b)); };
  if (!owned) return out(401, { error: "unauthorized" });
  if (u.pathname === "/v1/identities") return out(200, { identities: owned.map(m => ({ address: m.address, label: null })) });
  if (u.pathname === "/v1/whoami") { const m = owned.find(x => x.address === u.searchParams.get("identity")); return m ? out(200, m) : out(403, {}); }
  out(404, {});
}).listen(Number(process.argv[2] || 8792), "127.0.0.1");
