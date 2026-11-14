// Assemble the account site as deployed (site-shared at /shared/) plus a preview page with the
// stub, into the directory given: node test/smoke/assemble.mjs <dir>
import { cpSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { fixtures } from "../helpers.mjs";
const out = process.argv[2];
const site = new URL("../../", import.meta.url);
mkdirSync(out, { recursive: true });
cpSync(site.pathname, out, { recursive: true, filter: (p) => !/\/(node_modules|test)(\/|$)/.test(p) });
cpSync(new URL("../site-shared/", site).pathname, `${out}/shared`, { recursive: true, filter: (p) => !/\/(node_modules|test)(\/|$)/.test(p) });
mkdirSync(`${out}/__preview`, { recursive: true });
writeFileSync(`${out}/__preview/fixtures.js`, `window.__FIX = ${JSON.stringify(fixtures())};`);
cpSync(new URL("stub.js", import.meta.url).pathname, `${out}/__preview/stub.js`);
// A 390 px frame: headless Chrome will not make a window narrower than 500 px.
cpSync(new URL("m.html", import.meta.url).pathname, `${out}/__preview/m.html`);
const html = readFileSync(new URL("index.html", site), "utf8").replace('<script src="/config.js"></script>', '<script src="/config.js"></script>\n<script src="/__preview/fixtures.js"></script>\n<script src="/__preview/stub.js"></script>');
writeFileSync(`${out}/__preview/index.html`, html);
