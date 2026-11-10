// CSV summary: a no-AI tool. It never calls the broker, so its manifest declares no egress at all.

import { pathToFileURL } from "node:url";
import { serve, NestError } from "./pigeonpost-nest.mjs";

/** Split one CSV line, honouring double-quoted cells ("a, b" and "" escapes). */
export function splitLine(line, delimiter) {
  const cells = [];
  let cell = "";
  let quoted = false;
  for (let i = 0; i < line.length; i++) {
    const c = line[i];
    if (quoted) {
      if (c === '"' && line[i + 1] === '"') {
        cell += '"';
        i++;
      } else if (c === '"') quoted = false;
      else cell += c;
    } else if (c === '"') quoted = true;
    else if (c === delimiter) {
      cells.push(cell.trim());
      cell = "";
    } else cell += c;
  }
  cells.push(cell.trim());
  return cells;
}

export function summarize(csv, delimiter = ",", places = 2) {
  const lines = csv.split(/\r?\n/).filter((l) => l.trim());
  if (lines.length === 0) throw new NestError("invalid_operation_payload", "the table is empty");
  const columns = splitLine(lines[0], delimiter);
  const rows = lines.slice(1).map((l) => splitLine(l, delimiter));
  const totals = {};
  columns.forEach((name, c) => {
    const values = rows.map((r) => r[c]).filter((v) => v !== undefined && v !== "");
    if (values.length && values.every((v) => /^-?\d+(\.\d+)?$/.test(v))) {
      const sum = values.reduce((s, v) => s + Number(v), 0);
      totals[name] = Number(sum.toFixed(places));
    }
  });
  return { rows: rows.length, columns, totals };
}

export const handlers = {
  "csv.summarize@1.0": async (job) => {
    const csv = job.payload.csv;
    if (typeof csv !== "string" || !csv.trim()) throw new NestError("invalid_operation_payload");
    const delimiter = job.payload.delimiter || ",";
    const places = Number.isInteger(job.config.decimal_places) ? job.config.decimal_places : 2;
    return { answer: summarize(csv, delimiter, places), sources: [], model_used: false };
  },
};

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  serve(handlers);
}
