import test from "node:test";
import assert from "node:assert/strict";
import { parseSections, bestSections, loadDocs, makeHandlers } from "./app.mjs";

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

test("parseSections splits on ## headings", () => {
  const result = parseSections("intro\n## One\nfirst line\nsecond\n## Two\nx");
  assert.deepEqual(result, [
    { id: "one", title: "One", body: "first line second" },
    { id: "two", title: "Two", body: "x" }
  ]);
});

test("bestSections finds annual-leave section for query about leave days", () => {
  const sections = loadDocs();
  const best = bestSections(sections, "annual leave days", 1);
  assert.equal(best[0].id, "annual-leave");
});

test("handler docs.ask@1.0 without broker returns quoted answer", async () => {
  const sections = loadDocs();
  const handlers = makeHandlers(sections);
  const job = fakeJob({ payload: { question: "How many days of annual leave?" } });
  const result = await handlers["docs.ask@1.0"](job);
  assert.equal(result.model_used, false);
  assert.equal(result.sources[0], "annual-leave");
  assert.ok(result.answer.includes("25 days"));
});

test("handler docs.ask@1.0 with broker uses model answer", async () => {
  const sections = loadDocs();
  const handlers = makeHandlers(sections);
  let capturedUser;
  const broker = {
    available: true,
    complete: async ({ system, user }) => {
      capturedUser = user;
      return { text: " Twenty-five days. ", usage: { input: 1, output: 1 } };
    }
  };
  const job = fakeJob({ payload: { question: "How many days of annual leave?" }, broker });
  const result = await handlers["docs.ask@1.0"](job);
  assert.equal(result.answer, "Twenty-five days.");
  assert.equal(result.model_used, true);
  assert.ok(capturedUser.includes("Annual leave"));
});

test("handler docs.ask@1.0 with failing broker falls back to quoted answer", async () => {
  const sections = loadDocs();
  const handlers = makeHandlers(sections);
  const broker = {
    available: true,
    complete: async () => { throw new Error("model failed"); }
  };
  const job = fakeJob({ payload: { question: "How many days of annual leave?" }, broker });
  const result = await handlers["docs.ask@1.0"](job);
  assert.equal(result.model_used, false);
  assert.ok(result.answer.includes("25 days"));
});

test("handler docs.ask@1.0 with unmatched question returns not-covered message", async () => {
  const sections = loadDocs();
  const handlers = makeHandlers(sections);
  const job = fakeJob({ payload: { question: "zzzz qqqq" } });
  const result = await handlers["docs.ask@1.0"](job);
  assert.deepEqual(result, { answer: "The documents do not cover this question.", sources: [], model_used: false });
});

test("handler docs.ask@1.0 rejects empty question", async () => {
  const sections = loadDocs();
  const handlers = makeHandlers(sections);
  const job = fakeJob({ payload: { question: "" } });
  await assert.rejects(
    handlers["docs.ask@1.0"](job),
    (e) => e.code === "invalid_operation_payload"
  );
});
