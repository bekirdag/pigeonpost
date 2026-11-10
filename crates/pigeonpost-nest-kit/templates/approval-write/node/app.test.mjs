import test from "node:test";
import assert from "node:assert/strict";
import { effectKey, plainReply, handlers } from "./app.mjs";

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

test("effectKey generates correct key", () => {
  assert.equal(effectKey("job_x", "T 1/2"), "reply:T_1_2:job_x");
});

test("plainReply formats reply with signature", () => {
  assert.equal(plainReply(" Refund approved. ", "Team"), "Hello,\n\nRefund approved.\n\nTeam");
});

test("handler helpdesk.reply.send@1.0 without broker posts plain reply", async () => {
  let effectArg;
  const effect = async (arg) => {
    effectArg = arg;
    return { reference: "HD-1", body: { ticket_id: "T-1", reply: "APPROVED TEXT" } };
  };
  const job = fakeJob({
    payload: { ticket_id: "T-1", notes: "Refund approved." },
    config: { signature: "Team" },
    effect
  });
  const result = await handlers["helpdesk.reply.send@1.0"](job);
  assert.deepEqual(result.answer, {
    ticket_id: "T-1",
    reply: "APPROVED TEXT",
    posted: true,
    reference: "HD-1"
  });
  assert.deepEqual(result.sources, []);
  assert.equal(result.model_used, false);
  assert.equal(effectArg.kind, "helpdesk.reply.post");
  assert.equal(effectArg.requiresApproval, true);
  assert.equal(effectArg.key, "reply:T-1:job_aaaaaaaaaaaaaaaaaaaaaaaaaa");
  assert.equal(effectArg.body.reply, "Hello,\n\nRefund approved.\n\nTeam");
});

test("handler helpdesk.reply.send@1.0 rejects with approval_required when effect throws", async () => {
  const effect = async () => {
    throw Object.assign(new Error("approval_required"), { code: "approval_required" });
  };
  const job = fakeJob({
    payload: { ticket_id: "T-1", notes: "Refund approved." },
    config: {},
    effect
  });
  await assert.rejects(
    handlers["helpdesk.reply.send@1.0"](job),
    (e) => e.code === "approval_required"
  );
});

test("handler helpdesk.reply.send@1.0 with broker uses model draft", async () => {
  let effectArg;
  const effect = async (arg) => {
    effectArg = arg;
    return { reference: "HD-2" };
  };
  const broker = {
    available: true,
    complete: async () => ({ text: "Your refund is on its way.", usage: { input: 1, output: 1 } })
  };
  const job = fakeJob({
    payload: { ticket_id: "T-1", notes: "Refund approved." },
    config: {},
    broker,
    effect
  });
  const result = await handlers["helpdesk.reply.send@1.0"](job);
  assert.equal(result.answer.reply, "Your refund is on its way.\n\nThe support team");
  assert.equal(result.model_used, true);
});

test("handler helpdesk.reply.send@1.0 rejects missing notes", async () => {
  const job = fakeJob({ payload: { ticket_id: "T-1" }, config: {} });
  await assert.rejects(
    handlers["helpdesk.reply.send@1.0"](job),
    (e) => e.code === "invalid_operation_payload"
  );
});
