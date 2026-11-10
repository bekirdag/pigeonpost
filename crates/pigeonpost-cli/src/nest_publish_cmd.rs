//! `pigeonpost nest submit|status|logs` — a publisher's side of the marketplace review pipeline
//! (M2 routes), signed in as the account (`pigeonpost login`).
//!
//! ```text
//! pigeonpost nest submit --publisher pub_… --template tpl_… --version 1.2.0 --commit <sha>
//!                        (--repo https://git.example/owner/app.git | --source src_…)
//! pigeonpost nest status --publisher pub_… [--build bld_… | --review rvw_…]
//! pigeonpost nest logs   --publisher pub_… --test-nest tnt_…
//! ```
//!
//! `submit` asks Pigeonpost to build a pinned commit (`POST /v1/publishers/{p}/builds`; the build
//! plane fetches it, builds it twice and scans it). `status` lists the publisher's builds, or shows
//! one build and its review's stages (`GET …/builds/{b}`, `GET …/reviews/{r}`). `logs` prints a
//! hosted test Nest's log on a staging postbox (`GET …/test-nests/{t}/logs`). When the postbox
//! answers `401 step_up_required`, the request is repeated once with a fresh two-step sign-in
//! from the browser, as `pigeonpost ssh` does; that token is used for that request only.

use serde_json::{json, Value};
use std::path::Path;

type Error = Box<dyn std::error::Error>;

fn id_ok(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix).is_some_and(|rest| {
        !rest.is_empty()
            && rest.len() <= 60
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

fn need_id(id: &str, prefix: &str, what: &str) -> Result<(), Error> {
    if id_ok(id, prefix) {
        Ok(())
    } else {
        Err(format!("{what}: an id starting {prefix}").into())
    }
}

/// The body of a build request; checked here so a typo never reaches the postbox.
pub(crate) fn submit_body(
    template: &str,
    version: &str,
    commit: &str,
    repo: Option<&str>,
    source: Option<&str>,
) -> Result<Value, Error> {
    need_id(template, "tpl_", "--template")?;
    if !pigeonpost_core::nest_package::semver_ok(version) {
        return Err("--version: major.minor.patch".into());
    }
    let full_hex = (commit.len() == 40 || commit.len() == 64)
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !full_hex {
        return Err("--commit: the full lower-case commit id (40 or 64 hex digits)".into());
    }
    let src = match (repo, source) {
        (Some(r), None) => {
            if !r.starts_with("https://") || r.contains('@') || r.contains('?') {
                return Err("--repo: an https URL with no credentials".into());
            }
            json!({ "repository": r, "commit": commit })
        }
        (None, Some(s)) => {
            need_id(s, "src_", "--source")?;
            json!({ "source_id": s, "commit": commit })
        }
        _ => return Err("give exactly one of --repo or --source".into()),
    };
    Ok(json!({ "template_id": template, "version": version, "source": src }))
}

fn error_code(v: &Value) -> &str {
    v["error"]["code"]
        .as_str()
        .or(v["error"].as_str())
        .unwrap_or("error")
}

fn refusal(status: u16, v: &Value) -> String {
    let code = error_code(v);
    let msg = v["error"]["message"].as_str().unwrap_or("");
    let hint = match code {
        "publisher_not_certified" | "publisher_not_approved" => {
            " — the publisher must be certified first (staff review of the application)"
        }
        "quota_exceeded" => " — the publisher has used its builds for today",
        "git_host_not_allowed" => " — the build plane fetches from allowlisted git hosts only",
        "version_exists" => " — bump the version in nest-release.toml and the request",
        "feature_disabled" => " — the marketplace is not enabled for this account",
        _ => "",
    };
    format!("the postbox answered {status} {code}: {msg}{hint}")
}

async fn call(
    method: reqwest::Method,
    url: &str,
    token: &str,
    body: Option<&Value>,
) -> Result<(u16, Value), Error> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut req = client.request(method, url).bearer_auth(token);
    if let Some(b) = body {
        req = req.json(b);
    }
    let resp = req.send().await?;
    let status = resp.status().as_u16();
    Ok((status, resp.json().await.unwrap_or(Value::Null)))
}

/// One request as the signed-in account; on `step_up_required`, once more with a fresh strong
/// sign-in.
async fn request(
    home: &Path,
    method: reqwest::Method,
    url: &str,
    body: Option<&Value>,
) -> Result<(u16, Value), Error> {
    let token = crate::login_cmd::access_token(home).await?;
    let (s, v) = call(method.clone(), url, &token, body).await?;
    if s == 401 && error_code(&v) == "step_up_required" {
        let acr = v["error"]["details"]["acr_values"]
            .as_str()
            .unwrap_or("")
            .to_string();
        eprintln!("This needs a fresh two-step sign-in.");
        let strong = crate::login_cmd::step_up_token(home, &acr).await?;
        return call(method, url, &strong, body).await;
    }
    Ok((s, v))
}

/// The lines `status` prints for a review (`NestReviewStatus`).
pub(crate) fn render_review(r: &Value) -> Vec<String> {
    let mut out = vec![format!(
        "review {}  {}",
        r["review_id"].as_str().or(r["id"].as_str()).unwrap_or("?"),
        r["status"].as_str().unwrap_or("?")
    )];
    for st in r["stages"].as_array().into_iter().flatten() {
        let name = st["stage"].as_str().or(st["name"].as_str()).unwrap_or("?");
        let status = st["status"].as_str().unwrap_or("?");
        let mut line = format!("  {name:<20} {status}");
        if let Some(reason) = st["reason"].as_str().filter(|r| !r.is_empty()) {
            let clean: String = reason
                .chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect();
            line.push_str(&format!("  ({clean})"));
        }
        out.push(line);
    }
    out
}

pub(crate) fn render_build(b: &Value) -> String {
    let mut line = format!(
        "build {}  {}  {} @ {}",
        b["id"].as_str().unwrap_or("?"),
        b["status"].as_str().unwrap_or("?"),
        b["version"].as_str().unwrap_or("?"),
        b["source"]["commit"]
            .as_str()
            .map(|c| &c[..c.len().min(12)])
            .unwrap_or("?")
    );
    if let Some(code) = b["failure"]["code"].as_str() {
        line.push_str(&format!("  failed: {code}"));
    }
    if let Some(r) = b["review_id"].as_str() {
        line.push_str(&format!("  review {r}"));
    }
    line
}

fn print(json_out: bool, v: &Value, lines: Vec<String>) {
    if json_out {
        println!("{v}");
    } else {
        for l in lines {
            println!("{l}");
        }
    }
}

pub(crate) struct Submit<'a> {
    pub postbox: &'a str,
    pub publisher: &'a str,
    pub template: &'a str,
    pub version: &'a str,
    pub commit: &'a str,
    pub repo: Option<&'a str>,
    pub source: Option<&'a str>,
}

pub(crate) async fn submit(home: &Path, a: Submit<'_>, json_out: bool) -> Result<(), Error> {
    need_id(a.publisher, "pub_", "--publisher")?;
    let body = submit_body(a.template, a.version, a.commit, a.repo, a.source)?;
    let base = a.postbox.trim_end_matches('/');
    let url = format!("{base}/v1/publishers/{}/builds", a.publisher);
    let (s, v) = request(home, reqwest::Method::POST, &url, Some(&body)).await?;
    if s != 201 {
        return Err(refusal(s, &v).into());
    }
    let b = &v["data"];
    print(
        json_out,
        b,
        vec![
            render_build(b),
            format!(
                "follow it with: pigeonpost nest status --publisher {} --build {}",
                a.publisher,
                b["id"].as_str().unwrap_or("?")
            ),
        ],
    );
    Ok(())
}

pub(crate) async fn status(
    home: &Path,
    postbox: &str,
    publisher: &str,
    build: Option<&str>,
    review: Option<&str>,
    json_out: bool,
) -> Result<(), Error> {
    need_id(publisher, "pub_", "--publisher")?;
    let base = format!(
        "{}/v1/publishers/{publisher}",
        postbox.trim_end_matches('/')
    );
    let get = |path: String| {
        let url = format!("{base}{path}");
        async move { request(home, reqwest::Method::GET, &url, None).await }
    };
    let review = match (build, review) {
        (Some(_), Some(_)) => return Err("give --build or --review, not both".into()),
        (None, None) => {
            let (s, v) = get("/builds".into()).await?;
            if s != 200 {
                return Err(refusal(s, &v).into());
            }
            let lines = v["data"]
                .as_array()
                .into_iter()
                .flatten()
                .map(render_build)
                .collect();
            print(json_out, &v["data"], lines);
            return Ok(());
        }
        (Some(b), None) => {
            need_id(b, "bld_", "--build")?;
            let (s, v) = get(format!("/builds/{b}")).await?;
            if s != 200 {
                return Err(refusal(s, &v).into());
            }
            let Some(r) = v["data"]["review_id"].as_str().map(str::to_string) else {
                print(json_out, &v["data"], vec![render_build(&v["data"])]);
                return Ok(());
            };
            if !json_out {
                println!("{}", render_build(&v["data"]));
            }
            r
        }
        (None, Some(r)) => {
            need_id(r, "rvw_", "--review")?;
            r.to_string()
        }
    };
    let (s, v) = get(format!("/reviews/{review}")).await?;
    if s != 200 {
        return Err(refusal(s, &v).into());
    }
    print(json_out, &v["data"], render_review(&v["data"]));
    Ok(())
}

pub(crate) async fn logs(
    home: &Path,
    postbox: &str,
    publisher: &str,
    test_nest: &str,
    json_out: bool,
) -> Result<(), Error> {
    need_id(publisher, "pub_", "--publisher")?;
    need_id(test_nest, "tnt_", "--test-nest")?;
    let url = format!(
        "{}/v1/publishers/{publisher}/test-nests/{test_nest}/logs",
        postbox.trim_end_matches('/')
    );
    let (s, v) = request(home, reqwest::Method::GET, &url, None).await?;
    if s != 200 {
        return Err(refusal(s, &v).into());
    }
    let lines = v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| {
            let text = e.to_string();
            text.chars().filter(|c| !c.is_control()).collect::<String>()
        })
        .collect();
    print(json_out, &v["data"], lines);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn submit_bodies_are_checked_before_they_are_sent() {
        let c = "a".repeat(40);
        let b = submit_body(
            "tpl_x1",
            "1.2.0",
            &c,
            Some("https://git.example.org/a/b.git"),
            None,
        )
        .unwrap();
        assert_eq!(b["source"]["repository"], "https://git.example.org/a/b.git");
        assert_eq!(b["source"]["commit"], c.as_str());
        let s = submit_body("tpl_x1", "1.2.0", &c, None, Some("src_k1")).unwrap();
        assert_eq!(s["source"]["source_id"], "src_k1");
        for (t, v, commit, repo, src) in [
            ("x1", "1.2.0", c.as_str(), Some("https://g/a"), None),
            ("tpl_x1", "1.2", c.as_str(), Some("https://g/a"), None),
            ("tpl_x1", "1.2.0", "abc", Some("https://g/a"), None),
            ("tpl_x1", "1.2.0", c.as_str(), Some("http://g/a"), None),
            ("tpl_x1", "1.2.0", c.as_str(), Some("https://u:p@g/a"), None),
            ("tpl_x1", "1.2.0", c.as_str(), None, None),
            (
                "tpl_x1",
                "1.2.0",
                c.as_str(),
                Some("https://g/a"),
                Some("src_k1"),
            ),
        ] {
            assert!(
                submit_body(t, v, commit, repo, src).is_err(),
                "{t} {v} {repo:?}"
            );
        }
    }

    #[test]
    fn reviews_render_their_stages_without_control_characters() {
        let r = json!({
            "review_id": "rvw_1", "status": "in_review",
            "stages": [
                { "stage": "intake", "status": "passed" },
                { "stage": "deterministic_scan", "status": "failed", "reason": "secret\u{1b}[2Jfound" },
            ],
        });
        let lines = render_review(&r);
        assert_eq!(lines[0], "review rvw_1  in_review");
        assert!(lines[2].contains("deterministic_scan") && lines[2].contains("failed"));
        assert!(!lines[2].contains('\u{1b}'));
        let b = json!({ "id": "bld_1", "status": "failed", "version": "1.0.0",
                        "source": { "commit": "a".repeat(40) }, "failure": { "code": "build_failed" } });
        assert!(render_build(&b).ends_with("failed: build_failed"));
    }

    #[test]
    fn refusals_name_the_code_and_a_hint() {
        let v = json!({ "error": { "code": "quota_exceeded", "message": "used up" } });
        let text = refusal(429, &v);
        assert!(text.contains("429 quota_exceeded") && text.contains("builds for today"));
    }
}
