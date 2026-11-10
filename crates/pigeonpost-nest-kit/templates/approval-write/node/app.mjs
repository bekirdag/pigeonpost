// Helpdesk reply: drafts a reply with the model, then asks the platform to post it to the buyer's
// helpdesk. The post is an external effect that needs the buyer's approval: until it is approved
// the job ends with `approval_required`, and the platform delivers the job again afterwards.
//
// The idempotency key comes from the job (its id and ticket), not from the model's draft: a
// redelivered job names the same effect and can never post twice. The approval binds the first
// request made under a key, so what is posted is exactly what the buyer approved, even if a
// redelivered job drafts different words; the outcome carries that approved body back.

import { pathToFileURL } from "node:url";
import { serve, NestError } from "./pigeonpost-nest.mjs";

export function effectKey(jobId, ticketId) {
  return `reply:${ticketId.replace(/[^A-Za-z0-9._-]/g, "_")}:${jobId}`;
}

/** The reply without a model: the notes, framed and signed. Deterministic. */
export function plainReply(notes, signature) {
  return `Hello,\n\n${notes.trim()}\n\n${signature}`;
}

export const handlers = {
  "helpdesk.reply.send@1.0": async (job) => {
    const ticketId = typeof job.payload.ticket_id === "string" ? job.payload.ticket_id.trim() : "";
    const notes = typeof job.payload.notes === "string" ? job.payload.notes.trim() : "";
    if (!ticketId || !notes) throw new NestError("invalid_operation_payload");
    const signature = typeof job.config.signature === "string" ? job.config.signature : "The support team";

    let reply = plainReply(notes, signature);
    let modelUsed = false;
    if (job.broker.available) {
      await job.progress(0.2, "drafting the reply");
      try {
        const { text } = await job.broker.complete({
          system:
            "You write short, polite replies to customer support tickets from the agent's notes. " +
            "Do not promise anything the notes do not say. Do not add a signature.",
          user: `Notes:\n${notes}`,
          maxTokens: 300,
        });
        if (text.trim()) {
          reply = `${text.trim()}\n\n${signature}`;
          modelUsed = true;
        }
      } catch {
        // Keep the plain reply.
      }
    }

    await job.progress(0.6, "waiting for approval to post");
    // Throws NestError("approval_required") until the buyer approves this exact effect.
    const outcome = await job.effect({
      key: effectKey(job.id, ticketId),
      kind: "helpdesk.reply.post",
      summary: `Post a reply to ticket ${ticketId}`,
      body: { ticket_id: ticketId, reply },
      requiresApproval: true,
    });
    const posted = typeof outcome.body?.reply === "string" ? outcome.body.reply : reply;
    return {
      answer: { ticket_id: ticketId, reply: posted, posted: true, reference: String(outcome.reference ?? "") },
      sources: [],
      model_used: modelUsed,
    };
  },
};

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  serve(handlers);
}
