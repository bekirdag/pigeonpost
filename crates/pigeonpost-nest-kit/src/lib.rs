//! The Nest developer kit, embedded: the runtime helpers a marketplace Nest app copies into its
//! source tree (`sdk/node`, `sdk/python`) and the starter templates (`templates/`).
//!
//! `pigeonpost nest init` writes a template from here, so the files a developer gets are exactly
//! the ones tested in this repository. Nothing is fetched from a registry.

/// One file of a template or SDK, by its path relative to the app's root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct File {
    pub path: &'static str,
    pub contents: &'static str,
}

/// The language an app is written in. Each template exists in both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Node,
    Python,
}

impl Lang {
    pub fn parse(s: &str) -> Option<Lang> {
        match s {
            "node" | "js" | "javascript" => Some(Lang::Node),
            "python" | "py" => Some(Lang::Python),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Lang::Node => "node",
            Lang::Python => "python",
        }
    }

    /// The interpreter the runner image starts, as the manifest's `entrypoint` names it.
    pub fn entrypoint(self) -> &'static str {
        match self {
            Lang::Node => "/usr/local/bin/node",
            Lang::Python => "/usr/local/bin/python3",
        }
    }

    /// The app's main file in a template.
    pub fn main_file(self) -> &'static str {
        match self {
            Lang::Node => "app.mjs",
            Lang::Python => "app.py",
        }
    }
}

/// A starter template.
#[derive(Debug, Clone, Copy)]
pub struct Template {
    /// What `pigeonpost nest init <name>` takes.
    pub name: &'static str,
    /// The `name` its manifest ships with; `init` replaces it with the app's own.
    pub manifest_name: &'static str,
    pub summary: &'static str,
    pub shared: &'static [File],
    pub node: &'static [File],
    pub python: &'static [File],
}

macro_rules! file {
    ($dest:literal, $src:literal) => {
        File {
            path: $dest,
            contents: include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/", $src)),
        }
    };
}

pub const NODE_SDK: File = file!("pigeonpost-nest.mjs", "sdk/node/pigeonpost-nest.mjs");
pub const PYTHON_SDK: File = file!("pigeonpost_nest.py", "sdk/python/pigeonpost_nest.py");
pub const NODE_CONTAINERFILE: File = file!("Containerfile", "templates/_lang/node/Containerfile");
/// The language's lockfiles: the build plane refuses a release without one (Node:
/// `package.json` and `package-lock.json`; Python: `requirements.txt` with hashes).
pub const NODE_LOCKFILES: &[File] = &[
    file!("package.json", "templates/_lang/node/package.json"),
    file!(
        "package-lock.json",
        "templates/_lang/node/package-lock.json"
    ),
];
pub const PYTHON_LOCKFILES: &[File] = &[file!(
    "requirements.txt",
    "templates/_lang/python/requirements.txt"
)];
pub const PYTHON_CONTAINERFILE: File =
    file!("Containerfile", "templates/_lang/python/Containerfile");
/// The Python compile step (`nest-build.toml`): installs the hash-checked wheels offline, in
/// gVisor with no network, before the image is assembled. Node apps have none: their
/// `node_modules/` is vendored as it is.
pub const PYTHON_BUILD: File = file!("nest-build.toml", "templates/_lang/python/nest-build.toml");

pub const TEMPLATES: &[Template] = &[
    Template {
        name: "tool",
        manifest_name: "csv-summary",
        summary: "A no-AI tool: summarizes a CSV table. No broker, no network.",
        shared: &[
            file!("nest-release.toml", "templates/tool/nest-release.toml"),
            file!(
                "operations/csv.summarize.json",
                "templates/tool/operations/csv.summarize.json"
            ),
            file!("jobs/example.json", "templates/tool/jobs/example.json"),
            file!("README.md", "templates/tool/README.md"),
        ],
        node: &[
            file!("app.mjs", "templates/tool/node/app.mjs"),
            file!("app.test.mjs", "templates/tool/node/app.test.mjs"),
        ],
        python: &[
            file!("app.py", "templates/tool/python/app.py"),
            file!("test_app.py", "templates/tool/python/test_app.py"),
        ],
    },
    Template {
        name: "docs-qa",
        manifest_name: "docs-qa",
        summary: "AI Q&A over documents bundled with the release, through the model broker.",
        shared: &[
            file!("nest-release.toml", "templates/docs-qa/nest-release.toml"),
            file!(
                "operations/docs.ask.json",
                "templates/docs-qa/operations/docs.ask.json"
            ),
            file!("jobs/example.json", "templates/docs-qa/jobs/example.json"),
            file!("docs/handbook.md", "templates/docs-qa/docs/handbook.md"),
            file!("README.md", "templates/docs-qa/README.md"),
        ],
        node: &[
            file!("app.mjs", "templates/docs-qa/node/app.mjs"),
            file!("app.test.mjs", "templates/docs-qa/node/app.test.mjs"),
        ],
        python: &[
            file!("app.py", "templates/docs-qa/python/app.py"),
            file!("test_app.py", "templates/docs-qa/python/test_app.py"),
        ],
    },
    Template {
        name: "approval-write",
        manifest_name: "helpdesk-reply",
        summary: "An operation with an external write the buyer approves first (helpdesk reply).",
        shared: &[
            file!(
                "nest-release.toml",
                "templates/approval-write/nest-release.toml"
            ),
            file!(
                "operations/helpdesk.reply.send.json",
                "templates/approval-write/operations/helpdesk.reply.send.json"
            ),
            file!(
                "jobs/example.json",
                "templates/approval-write/jobs/example.json"
            ),
            file!("README.md", "templates/approval-write/README.md"),
        ],
        node: &[
            file!("app.mjs", "templates/approval-write/node/app.mjs"),
            file!("app.test.mjs", "templates/approval-write/node/app.test.mjs"),
        ],
        python: &[
            file!("app.py", "templates/approval-write/python/app.py"),
            file!("test_app.py", "templates/approval-write/python/test_app.py"),
        ],
    },
];

pub fn template(name: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.name == name)
}

/// A slug the manifest accepts as `name`: 1-64 of a-z 0-9 -.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The publisher part of a template's listing address until `init --publisher` names one.
pub const PLACEHOLDER_PUBLISHER: &str = "your-publisher";

/// Every file of a new app made from `template` in `lang`, named `name`: the template's shared
/// files, its language files, the runner Containerfile and a copy of the SDK. The manifest is
/// rewritten for the app's name, its publisher and the language's entrypoint.
pub fn render(
    template: &Template,
    lang: Lang,
    name: &str,
    publisher: &str,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for f in template.shared {
        let contents = if f.path == "nest-release.toml" {
            manifest_for(f.contents, template.manifest_name, name, publisher, lang)
        } else {
            f.contents.to_string()
        };
        out.push((f.path.to_string(), contents));
    }
    let (lang_files, containerfile, sdk, locks, build): (_, _, _, _, &[File]) = match lang {
        Lang::Node => (
            template.node,
            NODE_CONTAINERFILE,
            NODE_SDK,
            NODE_LOCKFILES,
            &[],
        ),
        Lang::Python => (
            template.python,
            PYTHON_CONTAINERFILE,
            PYTHON_SDK,
            PYTHON_LOCKFILES,
            &[PYTHON_BUILD],
        ),
    };
    for f in lang_files
        .iter()
        .chain([&containerfile, &sdk])
        .chain(locks)
        .chain(build)
    {
        out.push((f.path.to_string(), f.contents.to_string()));
    }
    out
}

/// The template manifest with its name (in `name`, `listing_address` and `[licences]`) and
/// publisher replaced and the entrypoint set for `lang`.
pub fn manifest_for(text: &str, from: &str, to: &str, publisher: &str, lang: Lang) -> String {
    text.lines()
        .map(|line| {
            let t = line.trim_start();
            if t.starts_with("entrypoint = ") {
                format!("entrypoint = \"{}\"", lang.entrypoint())
            } else if t.starts_with("name = ") && t == format!("name = \"{from}\"") {
                format!("name = \"{to}\"")
            } else if t.starts_with("listing_address = ") {
                format!("listing_address = \"/{publisher}/{to}\"")
            } else if t.starts_with(&format!("{from} = ")) {
                line.replacen(from, to, 1)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn on_disk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                on_disk(&path, base, out);
            } else {
                let rel = path.strip_prefix(base).unwrap().to_string_lossy();
                out.push(rel.replace('\\', "/"));
            }
        }
    }

    /// A file added to a template directory must be embedded too, or `init` would silently leave
    /// it out.
    #[test]
    fn every_template_file_on_disk_is_embedded() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let mut files = Vec::new();
        on_disk(&root, &root, &mut files);
        // Every template's files, plus the two runner Containerfiles, the lockfiles and the
        // Python compile step.
        let listed: usize = TEMPLATES
            .iter()
            .map(|t| t.shared.len() + t.node.len() + t.python.len())
            .sum::<usize>()
            + 3
            + NODE_LOCKFILES.len()
            + PYTHON_LOCKFILES.len();
        let files: Vec<String> = files
            .into_iter()
            .filter(|f| !f.contains("__pycache__") && !f.ends_with(".pyc"))
            .collect();
        assert_eq!(files.len(), listed, "on disk: {files:?}");
    }

    #[test]
    fn render_names_the_app_and_picks_the_entrypoint() {
        let t = template("tool").unwrap();
        let files = render(t, Lang::Python, "my-tool", "acme");
        let manifest = &files
            .iter()
            .find(|(p, _)| p == "nest-release.toml")
            .unwrap()
            .1;
        assert!(manifest.contains("name = \"my-tool\""));
        assert!(manifest.contains("listing_address = \"/acme/my-tool\""));
        assert!(manifest.contains("my-tool = \"MIT\""));
        assert!(manifest.contains("entrypoint = \"/usr/local/bin/python3\""));
        assert!(!manifest.contains("csv-summary"));
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        for want in [
            "app.py",
            "test_app.py",
            "Containerfile",
            "nest-build.toml",
            "pigeonpost_nest.py",
        ] {
            assert!(paths.contains(&want), "{paths:?}");
        }
        assert!(!paths.contains(&"app.mjs"));
    }

    #[test]
    fn every_template_ships_an_example_job_for_one_of_its_operations() {
        for t in TEMPLATES {
            let job = t
                .shared
                .iter()
                .find(|f| f.path == "jobs/example.json")
                .unwrap();
            let manifest = t
                .shared
                .iter()
                .find(|f| f.path == "nest-release.toml")
                .unwrap();
            let op = job
                .contents
                .split("\"operation\": \"")
                .nth(1)
                .and_then(|r| r.split('"').next())
                .unwrap();
            assert!(
                manifest.contents.contains(&format!("name = \"{op}\"")),
                "{}: {op}",
                t.name
            );
            assert!(valid_name(t.manifest_name));
        }
    }
}
