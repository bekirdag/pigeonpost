"""Helpdesk reply: drafts a reply with the model, then asks the platform to post it to the buyer's
helpdesk. The post is an external effect that needs the buyer's approval: until it is approved the
job ends with `approval_required`, and the platform delivers the job again afterwards.

The idempotency key comes from the job (its id and ticket), not from the model's draft: a
redelivered job names the same effect and can never post twice. The approval binds the first
request made under a key, so what is posted is exactly what the buyer approved, even if a
redelivered job drafts different words; the outcome carries that approved body back.
"""

import re

from pigeonpost_nest import NestError, serve


def effect_key(job_id, ticket_id):
    return "reply:%s:%s" % (re.sub(r"[^A-Za-z0-9._-]", "_", ticket_id), job_id)


def plain_reply(notes, signature):
    """The reply without a model: the notes, framed and signed. Deterministic."""
    return "Hello,\n\n%s\n\n%s" % (notes.strip(), signature)


def send_reply(job):
    ticket_id = job.payload.get("ticket_id")
    notes = job.payload.get("notes")
    ticket_id = ticket_id.strip() if isinstance(ticket_id, str) else ""
    notes = notes.strip() if isinstance(notes, str) else ""
    if not ticket_id or not notes:
        raise NestError("invalid_operation_payload")
    signature = job.config.get("signature")
    signature = signature if isinstance(signature, str) else "The support team"

    reply = plain_reply(notes, signature)
    model_used = False
    if job.broker.available:
        job.progress(0.2, "drafting the reply")
        try:
            draft = job.broker.complete(
                system="You write short, polite replies to customer support tickets from the agent's notes. "
                "Do not promise anything the notes do not say. Do not add a signature.",
                user="Notes:\n" + notes,
                max_tokens=300,
            )
            if draft["text"].strip():
                reply = draft["text"].strip() + "\n\n" + signature
                model_used = True
        except NestError:
            pass  # Keep the plain reply.

    job.progress(0.6, "waiting for approval to post")
    # Raises NestError("approval_required") until the buyer approves this exact effect.
    outcome = job.effect(
        key=effect_key(job.id, ticket_id),
        kind="helpdesk.reply.post",
        summary="Post a reply to ticket %s" % ticket_id,
        body={"ticket_id": ticket_id, "reply": reply},
        requires_approval=True,
    )
    body = outcome.get("body") if isinstance(outcome, dict) else None
    posted = body["reply"] if isinstance(body, dict) and isinstance(body.get("reply"), str) else reply
    return {
        "answer": {"ticket_id": ticket_id, "reply": posted, "posted": True, "reference": str(outcome.get("reference", ""))},
        "sources": [],
        "model_used": model_used,
    }


HANDLERS = {"helpdesk.reply.send@1.0": send_reply}

if __name__ == "__main__":
    serve(HANDLERS)
