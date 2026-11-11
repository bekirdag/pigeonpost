//! The starter templates `pigeonpost nest init` writes, held to the build plane's own parser and
//! the deterministic scan's manifest rules, so a template can never drift from what review reads.

use pigeonpost_core::nest_package;
use pigeonpost_nest_kit::{render, Lang, TEMPLATES};
use pigeonpost_nests::analysis::check_manifest;
use pigeonpost_nests::release::{
    containerfile_assembly, containerfile_pinned, BuildSpec, Manifest,
};

#[test]
fn every_template_manifest_parses_and_only_declared_external_writes_are_blocked() {
    for t in TEMPLATES {
        for lang in [Lang::Node, Lang::Python] {
            let files = render(t, lang, "my-app", "acme");
            let text = &files
                .iter()
                .find(|(p, _)| p == "nest-release.toml")
                .expect("manifest")
                .1;
            let m = Manifest::parse(text).unwrap_or_else(|e| panic!("{}: {e}", t.name));
            assert_eq!(m.name, "my-app");
            // M2's shared rules: the full manifest (keyword list included) and its configuration
            // schema; the empty configuration is valid with the schema's defaults.
            let json = serde_json::to_value(toml::from_str::<toml::Value>(text).unwrap()).unwrap();
            let issues = nest_package::validate_manifest(&json);
            let errors = nest_package::errors(&issues);
            assert!(errors.is_empty(), "{}: {errors:?}", t.name);
            let schema = json
                .get("config_schema")
                .unwrap_or_else(|| panic!("{}: no config_schema", t.name));
            assert!(
                nest_package::validate_config_schema(schema).is_empty(),
                "{}",
                t.name
            );
            assert!(
                nest_package::validate_config(schema, &serde_json::json!({})).is_empty(),
                "{}",
                t.name
            );
            assert_eq!(
                issues.len() - errors.len(),
                usize::from(m.external_writes),
                "{}: only a declared external write is a blocker",
                t.name
            );
            assert_eq!(m.entrypoint, lang.entrypoint());
            assert!(m.licences.contains_key("my-app"), "{}", t.name);
            let blockers: Vec<String> = check_manifest(&m)
                .into_iter()
                .filter(|f| f.blocking)
                .map(|f| f.rule)
                .collect();
            if m.external_writes {
                // Declared honestly, and blocked until external writes are offered.
                assert_eq!(blockers, vec!["manifest.external_writes"], "{}", t.name);
            } else {
                assert!(blockers.is_empty(), "{}: {blockers:?}", t.name);
            }
            let containerfile = &files
                .iter()
                .find(|(p, _)| p == "Containerfile")
                .expect("Containerfile")
                .1;
            containerfile_pinned(containerfile).unwrap_or_else(|e| panic!("{}: {e}", t.name));
            // An assembly only: nothing in it runs a program.
            containerfile_assembly(containerfile, "localhost/pigeonpost-release")
                .unwrap_or_else(|e| panic!("{}: {e}", t.name));
            // Python compiles (installs its wheels) in nest-build.toml; Node has nothing to do.
            let build = files.iter().find(|(p, _)| p == "nest-build.toml");
            match lang {
                Lang::Node => assert!(build.is_none(), "{}", t.name),
                Lang::Python => {
                    let spec = BuildSpec::parse(&build.expect("nest-build.toml").1)
                        .unwrap_or_else(|e| panic!("{}: {e}", t.name));
                    assert!(spec.image.is_none(), "the build plane's own Python");
                }
            }
        }
    }
}

#[test]
fn operation_descriptors_match_the_manifest_fields() {
    for t in TEMPLATES {
        let files = render(t, Lang::Node, "my-app", "acme");
        let m = Manifest::parse(
            &files
                .iter()
                .find(|(p, _)| p == "nest-release.toml")
                .unwrap()
                .1,
        )
        .unwrap();
        for op in &m.operations {
            let path = format!("operations/{}.json", op.name);
            let descriptor: serde_json::Value = serde_json::from_str(
                &files
                    .iter()
                    .find(|(p, _)| *p == path)
                    .unwrap_or_else(|| panic!("{}: {path}", t.name))
                    .1,
            )
            .unwrap();
            assert_eq!(descriptor["operation"], op.name.as_str());
            assert_eq!(descriptor["version"], op.version.as_str());
            let fields = descriptor["input"]["fields"].as_array().unwrap();
            assert_eq!(fields.len(), op.fields.len(), "{path}");
            for (d, f) in fields.iter().zip(&op.fields) {
                assert_eq!(d["name"], f.name.as_str(), "{path}");
                assert_eq!(d["max_chars"], f.max_chars, "{path}");
                assert_eq!(d["required"], f.required, "{path}");
            }
            let effects = descriptor["effects"].as_array().unwrap();
            assert_eq!(!effects.is_empty(), m.external_writes, "{path}");
            assert_eq!(
                descriptor["uses_model"] == true,
                m.egress.iter().any(|e| e == "broker"),
                "{path}"
            );
        }
    }
}
