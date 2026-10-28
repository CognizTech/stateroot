//! Progressive Skill package authoring/discovery/read contract through the CLI.
#[path = "support/mod.rs"]
mod support;

use serde_json::{json, Value};
use support::homes;

fn stateroot(
    config: &std::path::Path,
    home: &std::path::Path,
    project: &std::path::Path,
) -> assert_cmd::Command {
    let mut command = support::stateroot(config, home, project);
    command.env("PATH", "");
    for name in [
        "OPENCLAW_BUNDLED_SKILLS_DIR",
        "OPENCLAW_STATE_DIR",
        "OPENCLAW_CONFIG_PATH",
        "NVM_SYMLINK",
        "NVM_HOME",
        "APPDATA",
        "LOCALAPPDATA",
    ] {
        command.env_remove(name);
    }
    command
}

#[test]
fn authored_resources_and_provenance_roundtrip_through_real_cli() {
    let (config, home) = homes();
    let project = tempfile::tempdir().unwrap();
    stateroot(config.path(), home.path(), project.path())
        .arg("init")
        .assert()
        .success();
    let input = json!({
        "slug": "cli-reuse", "skill_md": "---\nname: cli-reuse\ndescription: Read a retained resource\n---\nRead references/example.txt and report its value.\n",
        "files_base64": {"references/example.txt": "cmV1c2UtdmFsdWUK"},
        "evidence": [{"kind":"session","reference":"past-session","status":"historical","context":"full context survives"}]
    });
    let written = stateroot(config.path(), home.path(), project.path())
        .args(["skill", "write", "--input", "-", "--from", "codex"])
        .write_stdin(input.to_string())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let written: Value = serde_json::from_slice(&written).unwrap();
    assert_eq!(written["lifecycle"], "active");
    let identity = written["package"]["identity_key"].as_str().unwrap();
    let digest = written["package"]["package_digest"].as_str().unwrap();
    let scan = stateroot(config.path(), home.path(), project.path())
        .args(["skill", "scan", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let scan: Value = serde_json::from_slice(&scan).unwrap();
    let row = scan
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["identity_key"] == identity)
        .unwrap();
    assert!(row.get("package_metadata").is_none());
    assert_eq!(row["files"].as_object().unwrap().len(), 2);
    let inspected = stateroot(config.path(), home.path(), project.path())
        .args([
            "skill",
            "show",
            identity,
            "--digest",
            digest,
            "--json",
            "--file",
            "references/example.txt",
            "--harness",
            "codex",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let inspected: Value = serde_json::from_slice(&inspected).unwrap();
    assert_eq!(inspected["package_metadata"]["evidence"], input["evidence"]);
    assert_eq!(inspected["content_base64"], "cmV1c2UtdmFsdWUK");
    assert_eq!(inspected["selector_state"], "explicit_selector");
    assert_eq!(inspected["selection"], "unobserved");
    assert_eq!(inspected["application"], "unobserved");
    assert_eq!(inspected["outcome"], "unobserved");
    assert!(inspected["projections"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["state"] == "projected"));
    stateroot(config.path(), home.path(), project.path())
        .args(["skill", "show", "cli-reuse"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Read references/example.txt"));
    stateroot(config.path(), home.path(), project.path())
        .args(["skill", "show", identity, "--digest", "old-digest"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("changed_package"));
    stateroot(config.path(), home.path(), project.path())
        .args(["skill", "show", identity, "--file", "../outside"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("package-relative"));
    assert!(project
        .path()
        .join(".agents/skills/cli-reuse/references/example.txt")
        .is_file());
    // Legacy show accepted the canonical directory slug even when a friendly
    // frontmatter name supplied a different discovery slug.
    let alias = json!({"slug":"legacy-alias", "skill_md":"---\nname: Friendly Display\n---\nLegacy alias body.\n"});
    stateroot(config.path(), home.path(), project.path())
        .args(["skill", "write"])
        .write_stdin(alias.to_string())
        .assert()
        .success();
    stateroot(config.path(), home.path(), project.path())
        .args(["skill", "show", "legacy-alias"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Legacy alias body"));
}

#[test]
fn legacy_requests_without_frontmatter_keep_canonical_package_identity() {
    let (config, home) = homes();
    let project = tempfile::tempdir().unwrap();
    stateroot(config.path(), home.path(), project.path())
        .arg("init")
        .assert()
        .success();
    for input in [
        json!({"slug":"demo", "skill_md":"# Demo\nDo the work.\n"}),
        json!({"slug":"named-demo", "name":"Optional author label", "skill_md":"# Named demo\nDo the work.\n"}),
    ] {
        let slug = input["slug"].as_str().unwrap();
        let output = stateroot(config.path(), home.path(), project.path())
            .args(["skill", "write", "--from", "codex"])
            .write_stdin(input.to_string())
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let written: Value = serde_json::from_slice(&output).unwrap();
        let package = &written["package"];
        assert_eq!(package["slug"], slug);
        // Optional `name` remains an author/audit label; it does not inject
        // frontmatter or alter the legacy scanner's directory-name fallback.
        assert_eq!(package["name"], slug);
        let invocation =
            format!("stateroot delegate --to codex --skill {slug} --task \"<microtask>\"");
        assert_eq!(package["native_invocation"], invocation);
        let scan = stateroot(config.path(), home.path(), project.path())
            .args(["skill", "scan", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let scan: Value = serde_json::from_slice(&scan).unwrap();
        let discovered = scan
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["identity_key"] == package["identity_key"])
            .unwrap();
        for field in [
            "slug",
            "name",
            "native_invocation",
            "identity_key",
            "package_digest",
            "source_path",
        ] {
            assert_eq!(
                discovered[field], package[field],
                "{field} must survive publication"
            );
        }
        stateroot(config.path(), home.path(), project.path())
            .args([
                "skill",
                "show",
                package["slug"].as_str().unwrap(),
                "--digest",
                package["package_digest"].as_str().unwrap(),
            ])
            .assert()
            .success()
            .stdout(input["skill_md"].as_str().unwrap().to_owned());
        stateroot(config.path(), home.path(), project.path())
            .args([
                "skill",
                "show",
                package["identity_key"].as_str().unwrap(),
                "--digest",
                package["package_digest"].as_str().unwrap(),
            ])
            .assert()
            .success()
            .stdout(input["skill_md"].as_str().unwrap().to_owned());
        let projected = project.path().join(".agents/skills").join(slug);
        assert_eq!(
            std::fs::read_to_string(projected.join("SKILL.md")).unwrap(),
            input["skill_md"].as_str().unwrap()
        );
        let sidecar: Value = serde_json::from_slice(
            &std::fs::read(projected.join(".stateroot-projection.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(sidecar["slug"], slug);
        assert_eq!(sidecar["native_invocation"], invocation);
        assert_eq!(sidecar["package_metadata"]["native_invocation"], invocation);
        assert!(!sidecar.to_string().contains("skill-authoring"));
    }
}
