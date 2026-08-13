//! The scoped request envelope: what an `auto` contact is actually allowed to ask for.
//!
//! Phase 2 gave a mailbox a way to say "I know this sender and my agent may act on their mail".
//! On its own that means *act on any prose they send*, which is far too much: knowing who sent a
//! message is not knowing the message is safe to obey, and one captured agent in a mesh could
//! then drive every peer that trusts it. This module is what makes `auto` finite.
//!
//! A message qualifies for `auto` only when its body is a request envelope
//!
//! ```json
//! { "v": 1, "verb": "run_tests", "args": { "target": "crates/…" }, "note": "…" }
//! ```
//!
//! whose verb is in a **closed, server-known vocabulary** *and* in the allowlist the recipient's
//! human granted that specific contact. Everything else — prose, malformed JSON, a future envelope
//! version, an unknown verb, a verb the contact wasn't granted — falls back to `review`.
//!
//! The vocabulary is closed on purpose. An open verb space with a blocklist of dangerous names
//! buys nothing: a sender who wants `run_shell` and finds it blocked simply asks for
//! `execute_command` instead. The server can only vouch that a request is bounded if the server
//! knows what the verb means, so an unrecognised verb must fail closed. New verbs arrive with a
//! postbox release, which is the point at which someone can think about their blast radius.

use serde_json::Value;

/// Whether a verb can ever be acted on without a human.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    /// Bounded and read-mostly. May be auto-accepted — but only for a contact whose allowlist
    /// names it.
    Grantable,
    /// Never auto, at any policy, for any contact: the categories where being wrong once is
    /// unrecoverable. Named rather than merely unknown so that a human is refused at grant time
    /// with a reason, and so an `auto` peer reaching for one shows up in the log as an attempt
    /// rather than as an unremarkable typo.
    Denied,
}

/// The whole vocabulary. Keep it small; every addition widens what a trusted-but-captured peer
/// can do to the agents that trust it.
const VOCABULARY: &[(&str, Class)] = &[
    // -- grantable ------------------------------------------------------------------------
    // "How is your work going?" — the verb the mesh exists for.
    ("report_status", Class::Grantable),
    // A question answerable from what the agent already knows or can read.
    ("answer_question", Class::Grantable),
    // Read one file and return it. Bounded by the agent's own filesystem permissions.
    ("read_file", Class::Grantable),
    // Run the project's test suite and report the result. Costs CPU, changes nothing durable.
    ("run_tests", Class::Grantable),
    // -- denied ---------------------------------------------------------------------------
    // The categories the plan puts permanently out of reach: publishing, deploying, secrets,
    // money, destruction, and arbitrary execution (which is every other row at once).
    ("git_push", Class::Denied),
    ("deploy", Class::Denied),
    ("read_credentials", Class::Denied),
    ("spend", Class::Denied),
    ("delete_files", Class::Denied),
    ("run_shell", Class::Denied),
];

/// Longest verb this will even consider, so a pathological body can't be used to bloat a log line.
const MAX_VERB_LEN: usize = 64;

/// Ceiling on serialized `args`. An envelope is a request, not a payload channel; anything with
/// real data in it should be prose a human reads, or a file the recipient fetches itself.
const MAX_ARGS_BYTES: usize = 4096;

/// The only envelope version this build understands. A newer one is not "probably fine" — it is by
/// definition a shape whose bounds this server cannot check, so it falls back to `review`.
const ENVELOPE_VERSION: u64 = 1;

pub fn class_of(verb: &str) -> Option<Class> {
    VOCABULARY
        .iter()
        .find(|(name, _)| *name == verb)
        .map(|(_, class)| *class)
}

/// The grantable verbs, for the CLI and the `list_contacts` response — an agent cannot be expected
/// to guess a closed vocabulary.
pub fn grantable() -> Vec<&'static str> {
    VOCABULARY
        .iter()
        .filter(|(_, c)| *c == Class::Grantable)
        .map(|(name, _)| *name)
        .collect()
}

/// The permanently-denied verbs, published for the same reason: better that a peer learns the
/// answer is "never" from a list than by being held for review forever with no explanation.
pub fn denied() -> Vec<&'static str> {
    VOCABULARY
        .iter()
        .filter(|(_, c)| *c == Class::Denied)
        .map(|(name, _)| *name)
        .collect()
}

/// A parsed request envelope. Its presence says the body is *shaped* like a request; it says
/// nothing yet about whether this sender may have it acted on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub verb: String,
    pub args: Value,
    pub note: Option<String>,
}

/// Parse a message body as a request envelope, or `None` if it is just text.
///
/// Failing to parse is the ordinary case, not an error: most messages between agents are prose,
/// and prose is exactly what should be held for a human.
pub fn parse(body: &str) -> Option<Request> {
    let value: Value = serde_json::from_str(body).ok()?;
    let obj = value.as_object()?;

    if obj.get("v").and_then(Value::as_u64) != Some(ENVELOPE_VERSION) {
        return None;
    }
    let verb = obj.get("verb").and_then(Value::as_str)?;
    if verb.is_empty() || verb.len() > MAX_VERB_LEN {
        return None;
    }

    // `args` may be absent, but when present it must be an object of bounded size. A bare scalar
    // or an array means the sender and this server disagree about the shape, and disagreement
    // about shape is precisely when not to act.
    let args = match obj.get("args") {
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(v @ Value::Object(_)) => {
            if v.to_string().len() > MAX_ARGS_BYTES {
                return None;
            }
            v.clone()
        }
        Some(_) => return None,
    };

    Some(Request {
        verb: verb.to_string(),
        args,
        note: obj.get("note").and_then(Value::as_str).map(str::to_string),
    })
}

/// Why a message that *could* have been auto-accepted was held instead. Carried to the recipient
/// so an agent can tell "nobody trusts you" from "you asked for the wrong thing", and so a human
/// reviewing the queue can see at a glance which it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// The sender has no `auto` grant. Nothing about the body would have changed this.
    SenderNotAuto,
    /// Free prose, malformed JSON, or an envelope version this build doesn't understand.
    NotARequest,
    /// Shaped like a request, but the verb is not in the vocabulary.
    UnknownVerb,
    /// A verb no policy can grant.
    VerbDenied,
    /// A real, grantable verb that this contact was not granted.
    VerbNotGranted,
}

impl Held {
    pub fn as_str(self) -> &'static str {
        match self {
            Held::SenderNotAuto => "sender_not_auto",
            Held::NotARequest => "not_a_request",
            Held::UnknownVerb => "unknown_verb",
            Held::VerbDenied => "verb_denied",
            Held::VerbNotGranted => "verb_not_granted",
        }
    }
}

/// What the recipient's agent may do with one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Bounded request from a sender granted this verb: the agent may act without asking.
    Auto { verb: String },
    /// Everything else. `review` is the resting state of this system.
    Review { held: Held, verb: Option<String> },
}

/// Decide one message.
///
/// `sender_is_auto` is the Phase 2 answer ("does a human trust this sender to drive work"),
/// `granted` the Phase 3 one ("to do which of these specific things"). Both must hold.
pub fn decide(body: &str, sender_is_auto: bool, granted: &[String]) -> Decision {
    let request = parse(body);

    if !sender_is_auto {
        return Decision::Review {
            held: Held::SenderNotAuto,
            verb: request.map(|r| r.verb),
        };
    }

    let Some(request) = request else {
        return Decision::Review {
            held: Held::NotARequest,
            verb: None,
        };
    };

    let held = match class_of(&request.verb) {
        None => Held::UnknownVerb,
        Some(Class::Denied) => Held::VerbDenied,
        Some(Class::Grantable) => {
            if granted.contains(&request.verb) {
                return Decision::Auto { verb: request.verb };
            }
            Held::VerbNotGranted
        }
    };
    Decision::Review {
        held,
        verb: Some(request.verb),
    }
}

/// Check a proposed allowlist before storing it.
///
/// Refusing a denied or unknown verb here — rather than storing it and letting it silently never
/// match — is the difference between a human learning their grant is a typo now, and wondering in
/// a month why an `auto` contact is still landing in review.
pub fn validate_grant(verbs: &[String]) -> Result<(), String> {
    for v in verbs {
        match class_of(v) {
            Some(Class::Grantable) => {}
            Some(Class::Denied) => {
                return Err(format!(
                    "'{v}' can never be auto-accepted — pushes, deploys, credential access, \
                     spending, destructive file operations and shell execution are held for a \
                     person at every policy level"
                ))
            }
            None => {
                return Err(format!(
                    "'{v}' is not a Pigeonpost verb. Grantable verbs: {}",
                    grantable().join(", ")
                ))
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn granted(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn prose_from_a_trusted_sender_is_still_held_for_a_human() {
        // The whole point of the phase: `auto` must not mean "run any text this peer sends".
        let d = decide("please run the tests", true, &granted(&["run_tests"]));
        assert_eq!(
            d,
            Decision::Review {
                held: Held::NotARequest,
                verb: None
            }
        );
    }

    #[test]
    fn a_granted_verb_from_a_trusted_sender_is_auto() {
        let body = r#"{"v":1,"verb":"run_tests","args":{"target":"crates/x"}}"#;
        assert_eq!(
            decide(body, true, &granted(&["run_tests"])),
            Decision::Auto {
                verb: "run_tests".into()
            }
        );
    }

    #[test]
    fn the_same_verb_from_an_untrusted_sender_is_held() {
        let body = r#"{"v":1,"verb":"run_tests"}"#;
        assert_eq!(
            decide(body, false, &granted(&["run_tests"])),
            Decision::Review {
                held: Held::SenderNotAuto,
                verb: Some("run_tests".into())
            }
        );
    }

    #[test]
    fn a_denied_verb_is_held_even_when_the_contact_somehow_lists_it() {
        // Defence in depth: validate_grant should have refused the grant, but if a row ever
        // acquired one by another route it still must not execute.
        let body = r#"{"v":1,"verb":"deploy"}"#;
        assert_eq!(
            decide(body, true, &granted(&["deploy", "run_tests"])),
            Decision::Review {
                held: Held::VerbDenied,
                verb: Some("deploy".into())
            }
        );
    }

    #[test]
    fn a_grantable_verb_outside_this_contacts_allowlist_is_held() {
        let body = r#"{"v":1,"verb":"read_file","args":{"path":"a.rs"}}"#;
        assert_eq!(
            decide(body, true, &granted(&["run_tests"])),
            Decision::Review {
                held: Held::VerbNotGranted,
                verb: Some("read_file".into())
            }
        );
    }

    #[test]
    fn an_unknown_verb_fails_closed() {
        let body = r#"{"v":1,"verb":"execute_command","args":{"cmd":"rm -rf /"}}"#;
        assert_eq!(
            decide(body, true, &granted(&["execute_command"])),
            Decision::Review {
                held: Held::UnknownVerb,
                verb: Some("execute_command".into())
            }
        );
    }

    #[test]
    fn a_future_envelope_version_is_not_a_request() {
        // We cannot check bounds on a shape we don't know, so v2 is prose until this build learns it.
        let body = r#"{"v":2,"verb":"run_tests"}"#;
        assert_eq!(
            decide(body, true, &granted(&["run_tests"])),
            Decision::Review {
                held: Held::NotARequest,
                verb: None
            }
        );
    }

    #[test]
    fn malformed_and_misshapen_envelopes_are_prose() {
        for body in [
            "{not json",
            "[]",
            r#"{"verb":"run_tests"}"#,                  // no version
            r#"{"v":1}"#,                               // no verb
            r#"{"v":1,"verb":""}"#,                     // empty verb
            r#"{"v":1,"verb":"run_tests","args":[1]}"#, // args must be an object
            r#"{"v":1,"verb":"run_tests","args":"x"}"#,
        ] {
            assert!(parse(body).is_none(), "should not parse: {body}");
        }
    }

    #[test]
    fn oversized_verbs_and_args_are_refused() {
        let long_verb = format!(r#"{{"v":1,"verb":"{}"}}"#, "a".repeat(MAX_VERB_LEN + 1));
        assert!(parse(&long_verb).is_none());

        let big = format!(
            r#"{{"v":1,"verb":"run_tests","args":{{"x":"{}"}}}}"#,
            "a".repeat(MAX_ARGS_BYTES)
        );
        assert!(parse(&big).is_none());
    }

    #[test]
    fn args_default_to_an_empty_object_when_omitted() {
        let r = parse(r#"{"v":1,"verb":"report_status","note":"weekly"}"#).unwrap();
        assert_eq!(r.verb, "report_status");
        assert_eq!(r.args, serde_json::json!({}));
        assert_eq!(r.note.as_deref(), Some("weekly"));
    }

    #[test]
    fn grants_accept_only_grantable_verbs() {
        assert!(validate_grant(&granted(&["run_tests", "read_file"])).is_ok());
        assert!(validate_grant(&granted(&["deploy"]))
            .unwrap_err()
            .contains("never be auto-accepted"));
        assert!(validate_grant(&granted(&["nonsense"]))
            .unwrap_err()
            .contains("not a Pigeonpost verb"));
    }

    #[test]
    fn the_two_classes_do_not_overlap() {
        for d in denied() {
            assert!(!grantable().contains(&d), "{d} is in both classes");
        }
    }
}
