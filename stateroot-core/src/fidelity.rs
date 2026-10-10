//! Bounded declarations, not a host backup or execution guarantee.
use crate::{roots, snap_context::SnapContext};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

pub const MANIFEST_PATH: &str = ".stateroot/reproducibility.json";
const LOCKFILES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "uv.lock",
    "poetry.lock",
    "go.sum",
    "Gemfile.lock",
    "composer.lock",
];

fn fingerprint(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Explicit references, not an inventory or a claim of delivery/use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub kind: String,
    pub scope: String,
    pub id: String,
    /// Declared path relative to the selected StateRoot artifact namespace.
    #[serde(default)]
    pub relative: String,
}

/// WS3 supplies observed presence/version facts; absent probes stay unknown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentComponent {
    pub name: String,
    pub status: String,
    pub observed_version: Option<String>,
}

pub fn references_from_packet(packet: &Value) -> Vec<ArtifactRef> {
    serde_json::from_value::<Vec<ArtifactRef>>(packet["artifact_refs"].clone()).unwrap_or_default()
}

/// Existing author declarations are references, not proof of injection/use.
pub fn boundary_references(project: &Path, home: &Path, packet: &Value) -> Vec<ArtifactRef> {
    let mut refs = references_from_packet(packet);
    for item in packet["relevant_memories"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let normalized = item.replace('\\', "/");
        let relative = normalized
            .strip_prefix(".stateroot/memories/")
            .or_else(|| normalized.strip_prefix("memories/"));
        if let Some(relative) = relative {
            refs.push(ArtifactRef {
                kind: "memory".into(),
                scope: "project".into(),
                id: fingerprint(relative.as_bytes()),
                relative: relative.into(),
            });
        } else {
            refs.push(ArtifactRef {
                kind: "memory".into(),
                scope: "unspecified".into(),
                id: fingerprint(item.as_bytes()),
                relative: String::new(),
            });
        }
    }
    for slug in packet["relevant_skills"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let scope = if project
            .join(".stateroot/skills")
            .join(slug)
            .join("SKILL.md")
            .is_file()
        {
            "project"
        } else if home
            .join(".stateroot/skills")
            .join(slug)
            .join("SKILL.md")
            .is_file()
        {
            "global"
        } else {
            "unspecified"
        };
        refs.push(ArtifactRef {
            kind: "skill".into(),
            scope: scope.into(),
            id: slug.into(),
            relative: String::new(),
        });
    }
    refs
}

fn bounded_bytes(path: &Path, remaining: &mut usize) -> Result<Vec<u8>, &'static str> {
    if *remaining == 0 {
        return Err("omitted: total read budget");
    }
    let limit = (*remaining).min(1024 * 1024);
    let file = std::fs::File::open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "missing"
        } else {
            "unreadable"
        }
    })?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "unreadable")?;
    *remaining = remaining.saturating_sub(bytes.len());
    if bytes.len() > limit {
        return Err("omitted: component read budget");
    }
    Ok(bytes)
}

fn shared(project: &Path, home: &Path, refs: &[ArtifactRef]) -> BTreeMap<String, Value> {
    let mut result = BTreeMap::new();
    let mut remaining = 4 * 1024 * 1024;
    for reference in refs.iter().take(256) {
        let key = format!("{}:{}:{}", reference.kind, reference.scope, reference.id);
        let relative = Path::new(&reference.relative);
        let portable = |text: &str| {
            !text.is_empty()
                && text
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
                && !text.contains("..")
        };
        if !portable(&reference.id)
            || !matches!(
                reference.kind.as_str(),
                "persona" | "memory" | "learning" | "skill" | "tool"
            )
            || (!relative.as_os_str().is_empty()
                && relative
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_))))
            || reference.relative.contains([':', '\\'])
            || !(matches!(
                reference.scope.as_str(),
                "user" | "global" | "project" | "workspace" | "unspecified"
            ) || reference
                .scope
                .strip_prefix("domain:")
                .is_some_and(portable))
        {
            result.insert(
                fingerprint(key.as_bytes()),
                json!({"status":"unsupported reference"}),
            );
            continue;
        }
        let root = match reference.scope.as_str() {
            "project" => Some(project.join(".stateroot")),
            "user" | "global" => Some(home.join(".stateroot")),
            "workspace" => crate::learnings::workspace_id_for(project)
                .map(|id| home.join(".stateroot/workspaces").join(id)),
            scope if scope.starts_with("domain:") && portable(&scope[7..]) => {
                Some(home.join(".stateroot/domains").join(&scope[7..]))
            }
            _ => None,
        };
        let path = root.and_then(|root| match reference.kind.as_str() {
            "persona" if reference.id == "canonical" && reference.scope == "user" => {
                Some(root.join("soul/SOUL.md"))
            }
            "persona" if reference.id == "overlay" && reference.scope == "project" => {
                Some(root.join("soul/OVERLAY.md"))
            }
            "memory" if !reference.relative.is_empty() => {
                Some(root.join("memories").join(relative))
            }
            "learning" if !reference.relative.is_empty() => {
                Some(root.join("learnings").join(relative))
            }
            "skill" => Some(root.join("skills").join(&reference.id).join("SKILL.md")),
            _ => None,
        });
        let value = match path {
            Some(path) => match bounded_bytes(&path, &mut remaining) {
                Ok(bytes) => {
                    json!({"status":"available","fingerprint":fingerprint(&bytes),"reference":reference})
                }
                Err(status) => json!({"status":status,"reference":reference}),
            },
            None => json!({"status":"unknown: unsupported namespace","reference":reference}),
        };
        result.insert(key, value);
    }
    result
}

pub fn capture(
    repo: &git2::Repository,
    tree: &git2::Tree,
    project: &Path,
    ctx: Option<&SnapContext>,
) -> Value {
    // Checkpoint/autosnap callers have no native session context, but their
    // current authored handoff already declares relevant sources. Preserve
    // those references without guessing a latest session or surveying home.
    let fallback = if ctx.is_none() {
        crate::local_store::read_handoff_local(project)
            .ok()
            .flatten()
            .and_then(|packet| {
                crate::harness_install::home_dir().ok().map(|home| {
                    let refs = boundary_references(project, &home, &packet);
                    (home, refs)
                })
            })
    } else {
        None
    };
    let artifact_context = ctx
        .map(|context| (context.home.as_path(), context.artifact_refs.as_slice()))
        .or_else(|| {
            fallback
                .as_ref()
                .map(|(home, refs)| (home.as_path(), refs.as_slice()))
        });
    let mut locks = BTreeMap::new();
    let mut lock_status = BTreeMap::new();
    let mut lock_budget = 4 * 1024 * 1024;
    for name in LOCKFILES {
        if let Ok(entry) = tree.get_path(Path::new(name)) {
            let size = repo
                .odb()
                .and_then(|odb| odb.read_header(entry.id()))
                .ok()
                .map(|(size, _)| size);
            if size.is_none_or(|size| size > 1024 * 1024 || size > lock_budget) {
                lock_status.insert(*name, "omitted: lockfile read budget");
                continue;
            }
            if let Ok(blob) = repo.find_blob(entry.id()) {
                lock_budget -= blob.size();
                locks.insert(*name, fingerprint(blob.content()));
            } else {
                lock_status.insert(*name, "unreadable Git object");
            }
        }
    }
    json!({"schema_version":"stateroot.reproducibility.v1", "lockfiles":locks,
        "lockfile_omissions":lock_status,
        "platform":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH},
        "tools":{"stateroot_core":env!("CARGO_PKG_VERSION")},
        "components":ctx.map(|context| &context.components),
        "shared_artifacts":artifact_context.map(|(home,refs)| shared(project, home, refs)),
        "reference_status":if artifact_context.is_some_and(|(_,refs)| !refs.is_empty()) { "explicit references at snapshot boundary; availability is not delivery or use" } else { "unknown: no explicit artifact references" },
        "references_omitted":artifact_context.map(|(_,refs)| refs.len().saturating_sub(256)).unwrap_or(0),
        "scope":"known root lockfiles and explicitly referenced canonical artifacts; no host configuration, auth, environment values or native history pinned",
        "bounds":{"references":256,"component_bytes":1048576,"total_artifact_bytes":4194304},
        "execution_reproduction":"not guaranteed"})
}

pub fn report(project: &Path, hash: &str, home: &Path) -> Result<Value, roots::RootsError> {
    report_with_components(project, hash, home, &[])
}

pub fn report_with_components(
    project: &Path,
    hash: &str,
    home: &Path,
    components: &[EnvironmentComponent],
) -> Result<Value, roots::RootsError> {
    let root = roots::get_root(project, hash)?;
    let repo = roots::ensure_repo(project)?;
    let commit = repo.find_commit(git2::Oid::from_str(&root.id)?)?;
    let tree = commit.tree()?;
    let project_content = compare_project(&repo, &tree, project)?;
    let manifest = tree
        .get_path(Path::new(MANIFEST_PATH))
        .ok()
        .and_then(|entry| repo.find_blob(entry.id()).ok())
        .map(|blob| serde_json::from_slice::<Value>(blob.content()));
    let recorded = match manifest {
        None => {
            return Ok(
                json!({"root":root.id,"project_content":project_content,"manifest_status":"legacy_unknown","execution_reproduction":"unknown"}),
            )
        }
        Some(Err(error)) => {
            return Ok(
                json!({"root":root.id,"project_content":project_content,"manifest_status":"corrupt","error":error.to_string(),"execution_reproduction":"unknown"}),
            )
        }
        Some(Ok(value)) => value,
    };
    let refs: Vec<ArtifactRef> = recorded["shared_artifacts"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(_, value)| serde_json::from_value(value["reference"].clone()).ok())
        .collect();
    let current = shared(project, home, &refs);
    if recorded["schema_version"] != "stateroot.reproducibility.v1" {
        return Ok(
            json!({"root":root.id,"manifest_status":"unsupported","execution_reproduction":"unknown"}),
        );
    }
    let lockfiles: Vec<Value> = recorded["lockfiles"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, expected)| {
            let status = if !LOCKFILES.contains(&name.as_str()) {
                "unsupported declaration"
            } else {
                match bounded_bytes(&project.join(name), &mut (1024 * 1024)) {
                    Ok(bytes) if Some(fingerprint(&bytes).as_str()) == expected.as_str() => {
                        "matching"
                    }
                    Ok(_) => "changed",
                    Err(status) => status,
                }
            };
            json!({"component":name,"status":status,"recorded_fingerprint":expected})
        })
        .collect();
    let differences: Vec<Value> = recorded["shared_artifacts"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, expected)| {
            let status = match current.get(name) {
                Some(value) if value["status"] != "available" => {
                    value["status"].as_str().unwrap_or("unknown")
                }
                Some(_) if expected["status"] != "available" => "unknown: no captured fingerprint",
                Some(value) if value["fingerprint"] == expected["fingerprint"] => "matching",
                Some(_) => "changed",
                None => "unknown: no supported reference",
            };
            json!({"component":name,"status":status,"recorded":expected})
        })
        .collect();
    let component_comparison: Vec<Value> = recorded["components"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|source| {
            let destination = components
                .iter()
                .find(|current| Some(current.name.as_str()) == source["name"].as_str());
            let status = match destination {
                None => "unknown: no destination observation",
                Some(current) if matches!(current.status.as_str(), "missing" | "absent") => {
                    "missing"
                }
                Some(current)
                    if source["observed_version"].is_string()
                        && current.observed_version.as_deref().is_some_and(|version| {
                            Some(version) != source["observed_version"].as_str()
                        }) =>
                {
                    "changed"
                }
                Some(current)
                    if source["observed_version"].is_string()
                        && current.observed_version.is_some() =>
                {
                    "matching observed version"
                }
                Some(_) => "unknown version; presence/configuration is not observed-working",
            };
            json!({"recorded":source,"current":destination,"status":status})
        })
        .collect();
    Ok(
        json!({"schema_version":"stateroot.fidelity.v1","root":root.id,
        "project_content":project_content,"components":component_comparison,
        "manifest_status":"recorded","recorded":recorded,"shared_artifacts":differences,
        "lockfiles":lockfiles,
        "tools":[{"component":"stateroot_core","recorded":recorded["tools"]["stateroot_core"],"current":env!("CARGO_PKG_VERSION"),"status":if recorded["tools"]["stateroot_core"] == env!("CARGO_PKG_VERSION") { "matching" } else { "changed" }}],
        "current_platform":{"os":std::env::consts::OS,"arch":std::env::consts::ARCH},
        "platform_status":if !recorded["platform"]["os"].is_string() || !recorded["platform"]["arch"].is_string() { "unknown" } else if recorded["platform"]["os"] == std::env::consts::OS && recorded["platform"]["arch"] == std::env::consts::ARCH { "matching observed platform" } else { "changed platform; execution compatibility not established" },
        "native_history":"not pinned; inspect exact receipt source availability",
        "reconstructable":"pinned lockfile declarations; software and credentials are not installed or restored",
        "execution_reproduction":"not guaranteed"}),
    )
}

fn compare_project(
    repo: &git2::Repository,
    tree: &git2::Tree,
    project: &Path,
) -> Result<Value, roots::RootsError> {
    let ignore = crate::sync_engine::ignore::IgnoreRules::load(project);
    let mut pinned = std::collections::BTreeSet::new();
    let mut rows = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut budget = 128 * 1024 * 1024;
    let mut complete = true;
    tree.walk(git2::TreeWalkMode::PreOrder, |directory, entry| {
        let relative = format!("{directory}{}", entry.name().unwrap_or_default());
        if relative == ".stateroot" {
            return git2::TreeWalkResult::Skip;
        }
        if entry.kind() != Some(git2::ObjectType::Blob) {
            return git2::TreeWalkResult::Ok;
        }
        pinned.insert(relative.clone());
        let metadata=std::fs::symlink_metadata(project.join(&relative));
        let symlink=metadata.as_ref().is_ok_and(|metadata|metadata.file_type().is_symlink());
        let status = if ignore.is_ignored(&relative, false) {
            "unavailable: destination privacy exclusion"
        } else if symlink {
            "unavailable: destination symlink; target not read"
        } else {
            match bounded_bytes(&project.join(&relative), &mut budget) {
                Ok(bytes) => match repo.find_blob(entry.id()) {
                    Ok(blob) if bytes == blob.content() => "matching",
                    Ok(_) => "changed",
                    Err(_) => "unavailable Git blob",
                },
                Err(status) => status,
            }
        };
        if status != "matching" {
            complete = false;
        }
        *counts.entry(status.into()).or_default() += 1;
        if rows.len() < 200 {
            #[allow(unused_mut)]
            let mut row=json!({"path":relative,"status":status,"git_mode":format!("{:o}",entry.filemode()),"mode_fidelity":"original permissions/link layout are not captured by root builder"});
            #[cfg(unix)]
            if let Ok(metadata)=metadata {
                use std::os::unix::fs::PermissionsExt;
                let executable=metadata.permissions().mode() & 0o111 != 0;
                row["destination_executable"]=json!(executable);
                row["git_mode_comparison"]=json!(if executable == (entry.filemode()==0o100755) {"matching pinned Git executable flag; original mode not attested"} else {"changed relative to pinned Git executable flag"});
            }
            rows.push(row);
        }
        git2::TreeWalkResult::Ok
    })?;
    let filters = ignore.clone();
    let base = project.to_path_buf();
    let walker = ignore::WalkBuilder::new(project)
        .hidden(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .follow_links(false)
        .filter_entry(move |entry| {
            entry
                .path()
                .strip_prefix(&base)
                .ok()
                .is_some_and(|relative| {
                    let text = relative.to_string_lossy().replace('\\', "/");
                    !relative.starts_with(".stateroot")
                        && !filters
                            .is_ignored(&text, entry.file_type().is_some_and(|kind| kind.is_dir()))
                })
        })
        .build();
    for (number, entry) in walker.enumerate() {
        if number >= 20000 {
            complete = false;
            *counts
                .entry("omitted: directory entry budget".into())
                .or_default() += 1;
            break;
        }
        let Ok(entry) = entry else {
            complete = false;
            *counts.entry("unreadable directory".into()).or_default() += 1;
            continue;
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(project)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if !pinned.contains(&relative) {
            complete = false;
            *counts.entry("additional".into()).or_default() += 1;
            if rows.len() < 200 {
                rows.push(json!({"path":relative,"status":"additional"}));
            }
        }
    }
    Ok(
        json!({"status":if complete { "matching compared pinned file bytes; metadata/execution fidelity not guaranteed" } else { "changed, missing or unavailable; inspect rows" },"counts":counts,"rows":rows,"rows_omitted":counts.values().sum::<usize>().saturating_sub(200),"bounds":{"bytes":134217728,"component_bytes":1048576,"directory_entries":20000},"scope":"project file bytes only; original permissions/link layout, ignored/private files and StateRoot bookkeeping are not asserted equal"}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn mode_only_drift_is_visible_and_destination_links_never_open_external_bytes() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let project = fixture.path().join("project");
        let home = fixture.path().join("home");
        std::fs::create_dir_all(project.join(".stateroot")).unwrap();
        let tool = project.join("tool.sh");
        std::fs::write(&tool, "echo fixture\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).unwrap();
        let (root, _) = roots::create_root(&project, "cli", "fixture", None).unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let drift = report(&project, &root.id, &home).unwrap();
        assert_eq!(drift["project_content"]["rows"][0]["status"], "matching");
        assert_eq!(
            drift["project_content"]["rows"][0]["git_mode_comparison"],
            "changed relative to pinned Git executable flag"
        );
        let external = fixture.path().join("external");
        std::fs::write(&external, "echo fixture\n").unwrap();
        std::fs::remove_file(&tool).unwrap();
        std::os::unix::fs::symlink(&external, &tool).unwrap();
        let linked = report(&project, &root.id, &home).unwrap();
        assert_eq!(
            linked["project_content"]["rows"][0]["status"],
            "unavailable: destination symlink; target not read"
        );
        std::fs::write(project.join(".staterootignore"), "tool.sh\n").unwrap();
        let private = report(&project, &root.id, &home).unwrap();
        assert_eq!(
            private["project_content"]["rows"][0]["status"],
            "unavailable: destination privacy exclusion"
        );
    }

    #[test]
    fn existing_author_references_are_bounded_and_do_not_inventory_ambient_artifacts() {
        let fixture = tempfile::tempdir().unwrap();
        let project = fixture.path().join("project");
        let home = fixture.path().join("home");
        std::fs::create_dir_all(project.join(".stateroot/memories")).unwrap();
        std::fs::create_dir_all(project.join(".stateroot/skills/declared")).unwrap();
        std::fs::create_dir_all(project.join(".stateroot/skills/ambient")).unwrap();
        std::fs::write(
            project.join(".stateroot/memories/MEMORY.md"),
            "declared intelligence",
        )
        .unwrap();
        std::fs::write(
            project.join(".stateroot/skills/declared/SKILL.md"),
            "declared skill",
        )
        .unwrap();
        std::fs::write(
            project.join(".stateroot/skills/ambient/SKILL.md"),
            "not referenced",
        )
        .unwrap();
        let references = boundary_references(
            &project,
            &home,
            &json!({"relevant_memories":["memories/MEMORY.md"],"relevant_skills":["declared"]}),
        );
        let observed = shared(&project, &home, &references);
        assert_eq!(observed.len(), 2);
        assert!(observed
            .values()
            .all(|value| value["status"] == "available"));
        assert!(!serde_json::to_string(&observed)
            .unwrap()
            .contains("ambient"));
        std::fs::write(
            project.join(".stateroot/memories/MEMORY.md"),
            vec![b'x'; 1024 * 1024 + 1],
        )
        .unwrap();
        let oversized = shared(&project, &home, &references);
        assert!(oversized
            .values()
            .any(|value| value["status"] == "omitted: component read budget"));
    }

    #[test]
    fn platform_fixture_observations_never_claim_native_execution() {
        for os in ["linux", "windows", "macos"] {
            let fact = EnvironmentComponent {
                name: format!("{os}-fixture"),
                status: "fixture-observed presence".into(),
                observed_version: None,
            };
            let encoded = serde_json::to_value(&fact).unwrap();
            assert!(encoded["observed_version"].is_null());
            assert!(encoded["status"].as_str().unwrap().contains("fixture"));
        }
    }
    #[test]
    fn pinned_declarations_report_second_home_changes_without_host_contents() {
        let fixture = tempfile::tempdir().unwrap();
        let project = fixture.path().join("project");
        let source_home = fixture.path().join("source");
        let destination_home = fixture.path().join("destination");
        std::fs::create_dir_all(project.join(".stateroot")).unwrap();
        std::fs::create_dir_all(source_home.join(".stateroot/soul")).unwrap();
        std::fs::create_dir_all(destination_home.join(".stateroot/soul")).unwrap();
        std::fs::write(
            source_home.join(".stateroot/soul/SOUL.md"),
            "original persona",
        )
        .unwrap();
        std::fs::write(
            destination_home.join(".stateroot/soul/SOUL.md"),
            "changed persona",
        )
        .unwrap();
        std::fs::write(project.join("Cargo.lock"), "original lock declaration").unwrap();
        let ctx = SnapContext {
            home: source_home.clone(),
            artifact_refs: vec![ArtifactRef {
                kind: "persona".into(),
                scope: "user".into(),
                id: "canonical".into(),
                relative: String::new(),
            }],
            components: vec![EnvironmentComponent {
                name: "fixture-tool".into(),
                status: "present".into(),
                observed_version: Some("1.0".into()),
            }],
            ..Default::default()
        };
        let (root, _) = roots::create_root(&project, "codex", "fixture", Some(&ctx)).unwrap();
        std::fs::write(project.join("Cargo.lock"), "changed lock declaration").unwrap();
        let report = report(&project, &root.id, &destination_home).unwrap();
        assert_eq!(report["manifest_status"], "recorded");
        assert_eq!(report["lockfiles"][0]["status"], "changed");
        assert_eq!(report["shared_artifacts"][0]["status"], "changed");
        assert_eq!(report["project_content"]["counts"]["changed"], 1);
        let missing = report_with_components(
            &project,
            &root.id,
            &destination_home,
            &[EnvironmentComponent {
                name: "fixture-tool".into(),
                status: "missing".into(),
                observed_version: None,
            }],
        )
        .unwrap();
        assert_eq!(missing["components"][0]["status"], "missing");
        let repo = roots::ensure_repo(&project).unwrap();
        let commit = repo
            .find_commit(git2::Oid::from_str(&root.id).unwrap())
            .unwrap();
        let blob = repo
            .find_blob(
                commit
                    .tree()
                    .unwrap()
                    .get_path(Path::new(MANIFEST_PATH))
                    .unwrap()
                    .id(),
            )
            .unwrap();
        let text = std::str::from_utf8(blob.content()).unwrap();
        assert!(!text.contains("original persona"));
        assert!(!text.contains(&source_home.display().to_string()));
        assert!(!text.contains(&root.id));
        assert!(!text.contains("created_at"));
        assert!(matches!(
            roots::snap_if_changed(&project, "codex", "new lock", Some(&ctx)).unwrap(),
            roots::SnapOutcome::Created(..)
        ));
        assert!(matches!(
            roots::snap_if_changed(&project, "codex", "metadata only", Some(&ctx)).unwrap(),
            roots::SnapOutcome::Unchanged { .. }
        ));
    }
}
