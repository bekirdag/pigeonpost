import test from "node:test";
import assert from "node:assert/strict";
import { splitLine, summarize, handlers } from "./app.mjs";

function fakeJob({ payload = {}, config = {}, broker = null, effect = null } = {}) {
  const progress = [];
  return {
    id: "job_aaaaaaaaaaaaaaaaaaaaaaaaaa",
    payload, config, progress: async (f, m) => { progress.push([f, m]); },
    progressCalls: progress,
    broker: broker ?? { available: false, complete: async () => { throw new Error("no broker"); } },
    effect: effect ?? (async () => { throw new Error("no effect expected"); }),
  };
}

test("splitLine handles quoted cells with commas", () => {
  assert.deepEqual(splitLine('a,"b, c",d', ","), ["a", "b, c", "d"]);
});

test("splitLine handles escaped double quotes", () => {
  assert.deepEqual(splitLine('x,"say ""hi"""', ","), ["x", 'say "hi"']);
});

test("summarize with default delimiter and decimal places", () => {
  const result = summarize("item,qty,price\nbolts,10,0.25\nnuts,4,0.10\nwashers,12,0.05");
  assert.deepEqual(result.rows, 3);
  assert.deepEqual(result.columns, ["item", "qty", "price"]);
  assert.deepEqual(result.totals.qty, 26);
  assert.deepEqual(result.totals.price, 0.4);
});

test("summarize with semicolon delimiter", () => {
  const result = summarize("a;b\n1;2", ";");
  assert.deepEqual(result.totals.a, 1);
  assert.deepEqual(result.totals.b, 2);
});

test("handler csv.summarize@1.0 returns correct answer", async () => {
  const job = fakeJob({ payload: { csv: "n\n1.234\n2" }, config: { decimal_places: 1 } });
  const result = await handlers["csv.summarize@1.0"](job);
  assert.deepEqual(result.answer, { rows: 2, columns: ["n"], totals: { n: 3.2 } });
  assert.deepEqual(result.sources, []);
  assert.equal(result.model_used, false);
});

test("handler csv.summarize@1.0 rejects empty payload", async () => {
  const job = fakeJob({ payload: { csv: "   " } });
  await assert.rejects(
    handlers["csv.summarize@1.0"](job),
    (e) => e.code === "invalid_operation_payload"
  );
});

