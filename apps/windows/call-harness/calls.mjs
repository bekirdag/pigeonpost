// Starts the repository's call service (calls-service/src/server.js) for local tests. Its own
// entry check compares file:// URLs in a POSIX form, so on Windows it is started from here.
import http from "node:http";
import { pathToFileURL } from "node:url";

const { configFromEnv, createCallsService } = await import(pathToFileURL(process.argv[2]).href);
const cfg = configFromEnv();
const service = createCallsService(cfg);
const server = http.createServer((req, res) => service.handle(req, res));
server.headersTimeout = 15_000;
server.requestTimeout = 0;
server.listen(cfg.port, cfg.host, () => console.log(`calls: listening on ${cfg.host}:${cfg.port}`));
