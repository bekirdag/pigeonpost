"""Docs Q&A: answers `docs.ask` from the Markdown bundled in docs/ and from nothing else.

The documents are read once at start from the image. The model sees only the best-matching
sections; without a broker (or when it fails) the app still answers by quoting the best section.
"""

import os
import re

from pigeonpost_nest import NestError, serve

HERE = os.path.dirname(os.path.abspath(__file__))
MAX_QUESTION_CHARS = 2000


def words(text):
    return [w.lower() for w in re.split(r"[\W_]+", text) if len(w) > 2]


def parse_sections(markdown):
    """Split Markdown on `## ` headings. Text before the first heading is not a section."""
    sections = []
    for line in markdown.split("\n"):
        if line.startswith("## "):
            title = line[3:].strip()
            sections.append({"id": re.sub(r"[^a-z0-9]", "-", title.lower()), "title": title, "body": ""})
        elif sections and line.strip():
            s = sections[-1]
            s["body"] += (" " if s["body"] else "") + line.strip()
    return sections


def best_sections(sections, question, n):
    """The sections sharing the most words with the question, best first; ties keep document order."""
    asked = words(question)
    scored = []
    for i, s in enumerate(sections):
        have = words(s["title"] + " " + s["body"])
        title = words(s["title"])
        score = sum(
            (1 if any(t.startswith(w) for t in have) else 0) + (2 if any(t.startswith(w) for t in title) else 0)
            for w in asked
        )
        if score > 0:
            scored.append((-score, i, s))
    scored.sort(key=lambda x: (x[0], x[1]))
    return [s for _, _, s in scored[:n]]


def load_docs(directory=os.path.join(HERE, "docs")):
    sections = []
    for name in sorted(os.listdir(directory)):
        if name.endswith(".md"):
            with open(os.path.join(directory, name), encoding="utf-8") as f:
                sections.extend(parse_sections(f.read()))
    return sections


def make_handlers(sections):
    def ask(job):
        question = job.payload.get("question")
        question = question.strip() if isinstance(question, str) else ""
        if not question or len(question) > MAX_QUESTION_CHARS:
            raise NestError("invalid_operation_payload")
        n = job.config.get("sections_per_answer", 2)
        n = n if isinstance(n, int) and not isinstance(n, bool) else 2
        best = best_sections(sections, question, n)
        if not best:
            return {"answer": "The documents do not cover this question.", "sources": [], "model_used": False}
        sources = [s["id"] for s in best]
        quoted = "%s: %s" % (best[0]["title"], best[0]["body"])
        if not job.broker.available:
            return {"answer": quoted, "sources": sources, "model_used": False}
        job.progress(0.3, "asking the model")
        style = "Answer in a short paragraph." if job.config.get("answer_style") == "detailed" else "Answer in one or two sentences."
        context = "".join("## %s\n%s\n" % (s["title"], s["body"]) for s in best)
        try:
            reply = job.broker.complete(
                system="You answer questions about the documents given. Use only those sections. If they do "
                "not answer the question, say so. " + style,
                user="Sections:\n%s\nQuestion: %s" % (context, question),
                max_tokens=400,
            )
            if reply["text"].strip():
                return {"answer": reply["text"].strip(), "sources": sources, "model_used": True}
        except NestError:
            pass  # Fall back to quoting: the buyer still gets an answer from the documents.
        return {"answer": quoted, "sources": sources, "model_used": False}

    return {"docs.ask@1.0": ask}


if __name__ == "__main__":
    serve(make_handlers(load_docs()))
