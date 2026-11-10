//! HR policy Q&A, the marketplace reference release (build plan M1).
//!
//! It answers one operation, `hr.policy.ask`, from the policy text bundled into the binary and
//! from nothing else. It reads no file but its own job inbox, writes nothing outside its job
//! outbox, and its only network peer is the model broker. When the broker is not available it
//! still answers, by quoting the best-matching policy section.
//!
//! This file is pure logic: no I/O. The Nests crate's tests include it directly, so the honeytoken
//! behaviour test runs this exact code.

use serde_json::{json, Value};

pub const OPERATION: &str = "hr.policy.ask";
pub const OPERATION_VERSION: &str = "1.0";
pub const MAX_QUESTION_CHARS: usize = 2000;
/// Sections given to the model per question.
const CONTEXT_SECTIONS: usize = 2;

/// One `## ` section of the handbook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub id: String,
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct Policy {
    pub sections: Vec<Section>,
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() > 2)
        .map(str::to_lowercase)
        .collect()
}

impl Policy {
    /// Split Markdown on `## ` headings. Text before the first heading is not a section.
    pub fn parse(markdown: &str) -> Policy {
        let mut sections: Vec<Section> = Vec::new();
        for line in markdown.lines() {
            if let Some(title) = line.strip_prefix("## ") {
                let title = title.trim().to_string();
                let id = title
                    .to_lowercase()
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                    .collect::<String>();
                sections.push(Section {
                    id,
                    title,
                    body: String::new(),
                });
            } else if let Some(current) = sections.last_mut() {
                if !line.trim().is_empty() {
                    if !current.body.is_empty() {
                        current.body.push(' ');
                    }
                    current.body.push_str(line.trim());
                }
            }
        }
        Policy { sections }
    }

    /// The sections that share the most words with the question, best first. Ties keep the
    /// handbook's order, so the answer is deterministic.
    pub fn best(&self, question: &str, n: usize) -> Vec<&Section> {
        let asked = words(question);
        let mut scored: Vec<(usize, usize, &Section)> = self
            .sections
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let have = words(&format!("{} {}", s.title, s.body));
                let title = words(&s.title);
                let score = asked
                    .iter()
                    .map(|w| {
                        let in_title = title.iter().any(|t| t.starts_with(w.as_str()));
                        let in_body = have.iter().any(|t| t.starts_with(w.as_str()));
                        usize::from(in_body) + 2 * usize::from(in_title)
                    })
                    .sum();
                (score, i, s)
            })
            .filter(|(score, _, _)| *score > 0)
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().take(n).map(|(_, _, s)| s).collect()
    }
}

/// The model broker, as this app sees it: one completion at a time, text in, text out.
pub trait Model {
    fn complete(&mut self, system: &str, user: &str) -> Result<String, String>;
}

fn refuse(job_id: &str, code: &str) -> Value {
    json!({ "job_id": job_id, "status": "failed", "error_code": code })
}

/// Answer one job. `job` is what the dispatcher wrote to the inbox:
/// `{"job_id", "operation", "operation_version", "payload": {"question"}}`.
pub fn handle(policy: &Policy, job: &Value, model: Option<&mut dyn Model>) -> Value {
    let job_id = job["job_id"].as_str().unwrap_or("").to_string();
    if job["operation"] != OPERATION || job["operation_version"] != OPERATION_VERSION {
        return refuse(&job_id, "unknown_operation");
    }
    let Some(question) = job["payload"]["question"].as_str() else {
        return refuse(&job_id, "invalid_operation_payload");
    };
    let question = question.trim();
    if question.is_empty() || question.chars().count() > MAX_QUESTION_CHARS {
        return refuse(&job_id, "invalid_operation_payload");
    }
    let best = policy.best(question, CONTEXT_SECTIONS);
    if best.is_empty() {
        return json!({
            "job_id": job_id,
            "status": "succeeded",
            "answer": "The handbook does not cover this question.",
            "sources": [],
            "model_used": false,
        });
    }
    let sources: Vec<&str> = best.iter().map(|s| s.id.as_str()).collect();
    let quoted = || format!("{}: {}", best[0].title, best[0].body);
    let (answer, model_used) = match model {
        Some(m) => {
            let context: String = best
                .iter()
                .map(|s| format!("## {}\n{}\n", s.title, s.body))
                .collect();
            let system = "You answer questions about an HR policy handbook. Use only the \
                          handbook sections given. If they do not answer the question, say so.";
            let user = format!("Handbook sections:\n{context}\nQuestion: {question}");
            match m.complete(system, &user) {
                Ok(text) if !text.trim().is_empty() => (text.trim().to_string(), true),
                _ => (quoted(), false),
            }
        }
        None => (quoted(), false),
    };
    json!({
        "job_id": job_id,
        "status": "succeeded",
        "answer": answer,
        "sources": sources,
        "model_used": model_used,
    })
}
