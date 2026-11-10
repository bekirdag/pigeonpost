// Docs Q&A: answers `docs.ask` from the Markdown bundled in docs/ and from nothing else.
//
// The documents are read once at start from the image. The model sees only the best-matching
// sections; without a broker (or when it fails) the app still answers by quoting the best section.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { serve, NestError } from "./pigeonpost-nest.mjs";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const MAX_QUESTION_CHARS = 2000;

const words = (text) =>
  text
    .split(/[^\p{L}\p{N}]+/u)
    .filter((w) => [...w].length > 2)
    .map((w) => w.toLowerCase());

/** Split Markdown on `## ` headings. Text before the first heading is not a section. */
export function parseSections(markdown) {
  const sections = [];
  for (const line of markdown.split("\n")) {
    if (line.startsWith("## ")) {
      const title = line.slice(3).trim();
      sections.push({ id: title.toLowerCase().replace(/[^a-z0-9]/g, "-"), title, body: "" });
    } else if (sections.length && line.trim()) {
      const s = sections[sections.length - 1];
      s.body += (s.body ? " " : "") + line.trim();
    }
  }
  return sections;
}

/** The sections sharing the most words with the question, best first; ties keep document order. */
export function bestSections(sections, question, n) {
  const asked = words(question);
  return sections
    .map((s, i) => {
      const have = words(`${s.title} ${s.body}`);
      const title = words(s.title);
      const score = asked.reduce(
        (sum, w) =>
          sum + (have.some((t) => t.startsWith(w)) ? 1 : 0) + (title.some((t) => t.startsWith(w)) ? 2 : 0),
        0,
      );
      return { score, i, s };
    })
    .filter((x) => x.score > 0)
    .sort((a, b) => b.score - a.score || a.i - b.i)
    .slice(0, n)
    .map((x) => x.s);
}

export function loadDocs(dir = path.join(HERE, "docs")) {
  return fs
    .readdirSync(dir)
    .filter((n) => n.endsWith(".md"))
    .sort()
    .flatMap((n) => parseSections(fs.readFileSync(path.join(dir, n), "utf8")));
}

export function makeHandlers(sections) {
  return {
    "docs.ask@1.0": async (job) => {
      const question = typeof job.payload.question === "string" ? job.payload.question.trim() : "";
      if (!question || [...question].length > MAX_QUESTION_CHARS) throw new NestError("invalid_operation_payload");
      const n = Number.isInteger(job.config.sections_per_answer) ? job.config.sections_per_answer : 2;
      const best = bestSections(sections, question, n);
      if (best.length === 0) {
        return { answer: "The documents do not cover this question.", sources: [], model_used: false };
      }
      const sources = best.map((s) => s.id);
      const quoted = `${best[0].title}: ${best[0].body}`;
      if (!job.broker.available) return { answer: quoted, sources, model_used: false };
      await job.progress(0.3, "asking the model");
      const style = job.config.answer_style === "detailed" ? "Answer in a short paragraph." : "Answer in one or two sentences.";
      const context = best.map((s) => `## ${s.title}\n${s.body}\n`).join("");
      try {
        const { text } = await job.broker.complete({
          system:
            "You answer questions about the documents given. Use only those sections. If they do " +
            `not answer the question, say so. ${style}`,
          user: `Sections:\n${context}\nQuestion: ${question}`,
          maxTokens: 400,
        });
        if (text.trim()) return { answer: text.trim(), sources, model_used: true };
      } catch {
        // Fall back to quoting: the buyer still gets an answer from the documents.
      }
      return { answer: quoted, sources, model_used: false };
    },
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  serve(makeHandlers(loadDocs()));
}
