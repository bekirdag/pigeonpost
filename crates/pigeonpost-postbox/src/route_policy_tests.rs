//! Every route reaches `policy::authorize` (Phase 9 hardening).
//!
//! A static check over the source: for each `.route(…)` in every router, follow the handler's
//! calls through functions defined in this crate (a few levels deep) and require that one of them
//! calls `authorize(` or `authorize_service(`. Routes that are deliberately open — public pages,
//! discovery documents, the proof-of-work challenge, the SSH CA's public key — are listed in
//! [`OPEN_ROUTES`] with the reason. The list must match exactly: a new route without a policy
//! check fails, and so does an entry that no longer needs to be there.
//!
//! It is a reachability check, not a proof that every branch authorizes: a handler can still return
//! data before the call it reaches. Reviews cover that; this catches the route that forgot the
//! policy entirely.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

/// `(method, path)` → why it needs no policy decision.
const OPEN_ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/", "static onboarding page"),
    ("GET", "/health", "liveness probe; no data"),
    (
        "GET",
        "/review-sign-in",
        "static App Review sign-in page; the POST is policy-checked",
    ),
    (
        "GET",
        "/.well-known/oauth-protected-resource",
        "public OAuth discovery document",
    ),
    (
        "GET",
        "/.well-known/oauth-protected-resource/chatgpt",
        "public OAuth discovery document",
    ),
    (
        "GET",
        "/v1/pow/challenge",
        "public proof-of-work challenge (signed, rate-adaptive)",
    ),
    (
        "GET",
        "/v1/handles/{name}/availability",
        "public handle availability, rate-limited",
    ),
    ("GET", "/v1/shell/ca", "the SSH CA's public key"),
];

const ROUTERS: &[(&str, &str)] = &[
    ("main.rs", "fn build_router("),
    ("internal_api.rs", "pub(crate) fn router("),
    ("org_routes.rs", "pub(crate) fn router("),
    ("org_agents.rs", "pub(crate) fn router("),
    ("org_api_keys.rs", "pub(crate) fn router("),
    ("webhooks.rs", "pub(crate) fn router("),
    ("meetings.rs", "pub(crate) fn router("),
    ("meetings.rs", "pub(crate) fn internal_router("),
];

const DEPTH: usize = 4;

fn sources() -> BTreeMap<String, String> {
    fn walk(dir: &Path, root: &Path, out: &mut BTreeMap<String, String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|e| e == "rs")
                && !path.to_string_lossy().ends_with("_tests.rs")
            {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(
                    rel,
                    std::fs::read_to_string(&path)
                        .unwrap()
                        .replace("\r\n", "\n"),
                );
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = BTreeMap::new();
    walk(&root, &root, &mut out);
    out
}

/// The brace-balanced block starting at the first `{` at or after `from`.
fn block(text: &str, from: usize) -> &str {
    let Some(open) = text[from..].find('{').map(|i| from + i) else {
        return "";
    };
    let mut depth = 0usize;
    for (i, ch) in text[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &text[open..open + i + 1];
                }
            }
            _ => {}
        }
    }
    &text[open..]
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Every `name(` call (and `name::<…>(`) in `body`, lowercase identifiers only.
fn calls(body: &str) -> Vec<&str> {
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i] as char;
        if (c.is_ascii_lowercase() || c == '_') && (i == 0 || !is_ident(bytes[i - 1] as char)) {
            let start = i;
            while i < bytes.len() && is_ident(bytes[i] as char) {
                i += 1;
            }
            let name = &body[start..i];
            let mut j = i;
            if body[j..].starts_with("::<") {
                if let Some(close) = body[j..].find('>') {
                    j += close + 1;
                }
            }
            while j < bytes.len() && bytes[j] == b' ' {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'(' {
                out.push(name);
            }
        } else {
            i += 1;
        }
    }
    out
}

/// Function name → `(file, body)` for every definition with that name.
type Fns = HashMap<String, Vec<(String, String)>>;

fn functions(src: &BTreeMap<String, String>) -> Fns {
    let mut fns: Fns = HashMap::new();
    for (file, text) in src {
        let mut rest = 0;
        while let Some(at) = text[rest..].find("fn ") {
            let at = rest + at;
            rest = at + 3;
            if at > 0 && is_ident(text.as_bytes()[at - 1] as char) {
                continue;
            }
            let name: String = text[at + 3..]
                .chars()
                .take_while(|c| is_ident(*c))
                .collect();
            if name.is_empty() {
                continue;
            }
            fns.entry(name)
                .or_default()
                .push((file.clone(), block(text, at).to_string()));
        }
    }
    fns
}

/// `(METHOD, path, handler path)` for every route in `router_fn`.
fn routes(text: &str, router_fn: &str) -> Vec<(String, String, String)> {
    let start = text.find(router_fn).expect("router function");
    let body = block(text, start);
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find(".route(") {
        rest = &rest[at + ".route(".len()..];
        let open = rest.find('"').unwrap();
        let close = open + 1 + rest[open + 1..].find('"').unwrap();
        let path = rest[open + 1..close].to_string();
        let mut depth = 1usize;
        let mut end = close;
        for (i, ch) in rest[close..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = close + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let expr = &rest[close + 1..end];
        for method in ["get", "post", "put", "patch", "delete"] {
            let mut from = 0;
            while let Some(m) = expr[from..].find(&format!("{method}(")) {
                let m = from + m;
                from = m + method.len() + 1;
                if m > 0 && is_ident(expr.as_bytes()[m - 1] as char) {
                    continue;
                }
                let handler: String = expr[from..]
                    .trim_start()
                    .chars()
                    .take_while(|c| is_ident(*c) || *c == ':')
                    .collect();
                out.push((method.to_uppercase(), path.clone(), handler));
            }
        }
    }
    out
}

/// The file a handler path names: `crate::calendar::guest::preview` → `calendar/guest.rs` (or
/// `calendar/guest/mod.rs`); an unqualified handler lives in the router's own file.
fn handler_file(router_file: &str, handler: &str) -> Vec<String> {
    let mut parts: Vec<&str> = handler.split("::").collect();
    parts.pop();
    parts.retain(|p| *p != "crate" && *p != "super" && *p != "self");
    if parts.is_empty() {
        return vec![router_file.to_string()];
    }
    let base = parts.join("/");
    vec![format!("{base}.rs"), format!("{base}/mod.rs")]
}

/// Whether the handler, resolved in its own file, reaches `authorize` within [`DEPTH`] calls.
/// Callees are resolved by name only (an over-approximation, so the check can miss but never
/// wrongly fail a route).
fn reaches_policy(fns: &Fns, router_file: &str, handler: &str) -> bool {
    let name = handler.rsplit("::").next().unwrap_or("");
    let files = handler_file(router_file, handler);
    let own: Vec<&String> = fns
        .get(name)
        .into_iter()
        .flatten()
        .filter(|(f, _)| files.contains(f))
        .map(|(_, b)| b)
        .collect();
    let mut seen = BTreeSet::new();
    let mut frontier: Vec<String> = Vec::new();
    for body in own {
        let called = calls(body);
        if called
            .iter()
            .any(|c| *c == "authorize" || *c == "authorize_service")
        {
            return true;
        }
        frontier.extend(
            called
                .into_iter()
                .filter(|c| fns.contains_key(*c))
                .map(str::to_string),
        );
    }
    for _ in 0..DEPTH {
        let mut next = Vec::new();
        for name in frontier {
            if !seen.insert(name.clone()) {
                continue;
            }
            for (_, body) in fns.get(&name).into_iter().flatten() {
                let called = calls(body);
                if called
                    .iter()
                    .any(|c| *c == "authorize" || *c == "authorize_service")
                {
                    return true;
                }
                next.extend(
                    called
                        .into_iter()
                        .filter(|c| fns.contains_key(*c))
                        .map(str::to_string),
                );
            }
        }
        frontier = next;
    }
    false
}

#[test]
fn every_route_reaches_the_policy_or_is_listed_as_open() {
    let src = sources();
    let fns = functions(&src);
    let mut all = Vec::new();
    for (file, router_fn) in ROUTERS {
        for (m, p, h) in routes(&src[*file], router_fn) {
            all.push((file.to_string(), m, p, h));
        }
    }
    assert!(all.len() > 200, "parser found only {} routes", all.len());
    let unresolved: Vec<_> = all
        .iter()
        .filter(|(f, _, _, h)| {
            let name = h.rsplit("::").next().unwrap_or("");
            let files = handler_file(f, h);
            !fns.get(name)
                .into_iter()
                .flatten()
                .any(|(file, _)| files.contains(file))
        })
        .map(|(_, m, p, h)| format!("{m} {p} -> {h}"))
        .collect();
    assert!(
        unresolved.is_empty(),
        "handlers not found in their files: {unresolved:?}"
    );
    let unreached: BTreeSet<(String, String)> = all
        .iter()
        .filter(|(f, _, _, h)| !reaches_policy(&fns, f, h))
        .map(|(_, m, p, _)| (m.clone(), p.clone()))
        .collect();
    let listed: BTreeSet<(String, String)> = OPEN_ROUTES
        .iter()
        .map(|(m, p, _)| (m.to_string(), p.to_string()))
        .collect();
    let missing: Vec<_> = unreached.difference(&listed).collect();
    let stale: Vec<_> = listed.difference(&unreached).collect();
    assert!(
        missing.is_empty(),
        "routes that never reach policy::authorize (add the check, or list them in OPEN_ROUTES \
         with a reason): {missing:?}"
    );
    assert!(
        stale.is_empty(),
        "OPEN_ROUTES entries that now reach the policy or no longer exist: {stale:?}"
    );
}

#[test]
fn the_reachability_check_can_fail() {
    let mut fns: Fns = HashMap::new();
    let one = |b: &str| vec![("main.rs".to_string(), b.to_string())];
    fns.insert("open".into(), one("{ helper(1) }"));
    fns.insert("helper".into(), one("{ do_thing() }"));
    fns.insert(
        "closed".into(),
        one("{ let x = helper(2); authorize(&a, b, &c, &d) }"),
    );
    // Same name in another file: must not lend its authorization to main.rs's handler.
    fns.get_mut("open")
        .unwrap()
        .push(("other.rs".into(), "{ authorize(&a, b, &c, &d) }".into()));
    assert!(!reaches_policy(&fns, "main.rs", "open"));
    assert!(reaches_policy(&fns, "main.rs", "closed"));
    assert!(reaches_policy(&fns, "main.rs", "other::open"));
}
