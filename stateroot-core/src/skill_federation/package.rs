//! Progressive package reads and explicit, immediately active authoring.
//! This uses the existing canonical package and projection owners, not a
//! selection runtime or evidence store. Evidence is author-supplied context.
use super::*;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SkillWriteRequest {
    pub slug: String,
    pub skill_md: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub rationale: Option<String>,
    /// Supplied resources are merged into the existing package; omitted files
    /// survive legacy SKILL.md-only updates. Binary resources are lossless.
    #[serde(default)]
    pub files_base64: BTreeMap<String, String>,
    /// Optional source references, including arbitrary future fields. Each
    /// entry can name kind, reference, status, project_id and body_digest.
    #[serde(default)]
    pub evidence: Option<Vec<Value>>,
}

#[derive(Debug, Serialize)]
pub struct SkillWriteResult {
    pub package: DiscoveredSkill,
    pub actions: Vec<SyncAction>,
    pub lifecycle: String,
    pub activates: String,
}

/// One factual read. Returning bytes establishes delivery, never agent
/// comprehension, application or outcome. A selector identifies an inspection
/// request; its presence or absence says nothing about task applicability.
#[derive(Debug, Serialize)]
pub struct SkillInspection {
    pub schema_version: String,
    pub availability: String,
    pub selector_state: String,
    pub selection: String,
    pub packages: Vec<DiscoveredSkill>,
    pub metadata_digest: Option<String>,
    pub package_metadata: Option<Value>,
    pub evidence_assessments: Vec<Value>,
    pub projections: Vec<Value>,
    pub route: Value,
    pub delivery: String,
    pub file: Option<String>,
    pub content_base64: Option<String>,
    pub application: String,
    pub outcome: String,
    pub detail: Option<String>,
}

fn safe_relative(raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty()
        || raw.contains('\\')
        || raw.contains(':')
        || raw.starts_with('/')
        || raw
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(format!("expected a package-relative path: {raw}"));
    }
    Ok(PathBuf::from(raw))
}

/// Write through the existing canonical skill store, then the existing sync
/// owner. No receipt is required. Explicit evidence updates bind to these
/// package bytes; omitted evidence remains available with its old binding.
pub fn author_skill(
    project: &Path,
    home: &Path,
    caller: &str,
    request: &SkillWriteRequest,
    options: &SyncOptions,
) -> Result<SkillWriteResult, String> {
    let slug = request.slug.trim();
    if slug.is_empty() || slugify(slug) != slug || request.skill_md.trim().is_empty() {
        return Err(
            "slug must be a simple lowercase skill name and skill_md must be nonempty".into(),
        );
    }
    let mut resources = Vec::new();
    for (path, encoded) in &request.files_base64 {
        let rel = safe_relative(path)?;
        if matches!(
            path.as_str(),
            "SKILL.md" | "skill.md" | PACKAGE_META | PROJECTION_META
        ) {
            return Err(format!("reserved package file: {path}"));
        }
        resources.push((
            rel,
            base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(|e| format!("resource {path}: {e}"))?,
        ));
    }
    let paths: BTreeSet<_> = resources.iter().map(|(path, _)| path.clone()).collect();
    for path in &paths {
        if path
            .ancestors()
            .skip(1)
            .any(|parent| paths.contains(parent))
        {
            return Err(format!(
                "resource paths conflict: {} has a supplied file as parent",
                path.display()
            ));
        }
    }
    let skill;
    {
        let _guard = write_guard(project)?;
        let store = crate::local_store::root(project).join("skills");
        let root = store.join(slug);
        let local = crate::local_store::root(project).join("local/skill-authoring");
        fs::create_dir_all(&store).map_err(|e| e.to_string())?;
        fs::create_dir_all(&local).map_err(|e| e.to_string())?;
        // Keep the old active package intact until every resource and its
        // metadata has been written. Both stage and backup live in the
        // existing machine-local lane, excluded from discovery and roots.
        // Rename failure (including an unusual cross-device local lane)
        // leaves/restores the old package; no copy-over fallback is used.
        let stage = tempfile::Builder::new()
            .prefix(&format!(".{slug}-author-"))
            .tempdir_in(&local)
            .map_err(|e| e.to_string())?;
        // The scanner's legacy fallback name comes from the package directory.
        // Keep that logical basename stable even inside a random workspace.
        let staged_package = stage.path().join(slug);
        fs::create_dir_all(&staged_package).map_err(|e| e.to_string())?;
        if root.exists() {
            copy_dir_recursive(&root, &staged_package)
                .map_err(|e| format!("stage existing package: {e}"))?;
        }
        let mut meta = read_json_file(&root.join(PACKAGE_META))
            .filter(Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        for (rel, _) in &resources {
            checked_package_path(&staged_package, rel, false)?;
        }
        write_resource(
            &staged_package,
            Path::new("SKILL.md"),
            request.skill_md.as_bytes(),
        )?;
        for (rel, bytes) in resources {
            write_resource(&staged_package, &rel, &bytes)?;
        }
        meta["schema_version"] = Value::String("stateroot.skill_package.v1".into());
        meta["slug"] = Value::String(slug.into());
        meta["scope"] = Value::String("project".into());
        meta["lifecycle"] = Value::String("active".into());
        if meta.get("origin").is_none() {
            meta["origin"] = serde_json::json!({"harness": caller, "source_kind": "explicit_authoring", "source_path": root});
        }
        // New identities bind to the final canonical path, not the staging
        // directory. Existing package identities remain stable across edits.
        if meta.get("identity_key").and_then(Value::as_str).is_none() {
            let origin = meta["origin"]["harness"].as_str().unwrap_or(caller);
            meta["identity_key"] = json_identity(origin, &root);
        }
        meta["ownership_class"] = Value::String("harness_authored".into());
        meta["native_harness"] = Value::String(caller.into());
        write_json(&staged_package.join(PACKAGE_META), &meta).map_err(|e| e.to_string())?;
        let mut authored = scan_skill_dir(&staged_package, caller, "project", "package")
            .ok_or("authored SKILL.md could not be scanned")?;
        authored.source_path = root.display().to_string();
        if let Some(evidence) = &request.evidence {
            authored.package_metadata["evidence"] = serde_json::json!(evidence);
            authored.package_metadata["evidence_package_digest"] =
                serde_json::json!(authored.package_digest);
        }
        write_package_meta(&staged_package, &authored).map_err(|e| e.to_string())?;
        authored.package_metadata = package_metadata(&authored);
        // Publish as a directory, retaining the old complete package until the
        // new rename succeeds. On failure restore it under the same lock.
        let backup = local.join(format!(".{slug}-previous-{}", uuid::Uuid::new_v4()));
        let had_previous = root.exists();
        if had_previous {
            fs::rename(&root, &backup).map_err(|e| format!("retain previous package: {e}"))?;
        }
        if let Err(error) = fs::rename(&staged_package, &root) {
            if had_previous {
                fs::rename(&backup, &root).map_err(|rollback| format!("publish failed: {error}; previous package retained at {} but restore failed: {rollback}", backup.display()))?;
            }
            return Err(format!(
                "publish package failed; previous package unchanged: {error}"
            ));
        }
        if had_previous {
            fs::remove_dir_all(&backup).map_err(|e| {
                format!(
                    "package active at {}; old package cleanup failed at {}: {e}",
                    root.display(),
                    backup.display()
                )
            })?;
        }
        skill = authored;
    }
    // Pull includes the shared .agents projection; push-only historically
    // skipped it. Authoring always activates immediately in both surfaces.
    let mut options = options.clone();
    options.pull = true;
    options.push = true;
    options.dry_run = false;
    let actions = sync_project(project, &options, Some(home)).map_err(|e| {
        format!(
            "package active at {}; projection failed: {e}",
            skill.source_path
        )
    })?;
    Ok(SkillWriteResult {
        package: skill,
        actions,
        lifecycle: "active".into(),
        activates: "immediately".into(),
    })
}

fn json_identity(harness: &str, path: &Path) -> Value {
    Value::String(format!(
        "psi_{}",
        &sha256_hex(
            format!(
                "{}\0project\0{}",
                normalize_harness(harness),
                path.display()
            )
            .as_bytes()
        )[..32]
    ))
}

fn write_resource(root: &Path, relative: &Path, bytes: &[u8]) -> Result<(), String> {
    let dest = checked_package_path(root, relative, false)?;
    let permissions = fs::metadata(&dest).ok().map(|m| m.permissions());
    fs::create_dir_all(dest.parent().unwrap())
        .map_err(|e| format!("create resource parent {}: {e}", dest.display()))?;
    crate::safe_io::atomic_replace(&dest, bytes)
        .map_err(|e| format!("write resource {}: {e}", dest.display()))?;
    if let Some(permissions) = permissions {
        fs::set_permissions(&dest, permissions)
            .map_err(|e| format!("preserve resource permissions: {e}"))?;
    }
    Ok(())
}

fn checked_package_path(
    root: &Path,
    relative: &Path,
    require_file: bool,
) -> Result<PathBuf, String> {
    let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
    let path = root.join(relative);
    let mut existing = path.as_path();
    while !existing.exists() {
        existing = existing.parent().ok_or("package resource has no parent")?;
    }
    if !existing
        .canonicalize()
        .map_err(|e| e.to_string())?
        .starts_with(&canonical_root)
    {
        return Err("resource resolves outside its package".into());
    }
    if require_file && !path.is_file() {
        return Err("package resource unavailable".into());
    }
    Ok(path)
}

/// Inspect the full catalog's exact identity/slug, or progressively read one
/// file. No ranking or hidden subset is applied. Legacy names resolve only
/// when all matching copies identify the same current package.
pub fn inspect_package(
    project: &Path,
    home: Option<&Path>,
    selector: Option<&str>,
    expected_digest: Option<&str>,
    file: Option<&str>,
    harness: Option<&str>,
) -> Result<SkillInspection, String> {
    let all = discover_all(project, home)?;
    let mut result = SkillInspection {
        schema_version: "stateroot.skill_inspection.v1".into(),
        availability: "available".into(),
        selector_state: "explicit_selector".into(),
        selection: "unobserved".into(),
        packages: Vec::new(),
        metadata_digest: None,
        package_metadata: None,
        evidence_assessments: Vec::new(),
        projections: Vec::new(),
        route: Value::Null,
        delivery: "metadata_only".into(),
        file: file.map(str::to_owned),
        content_base64: None,
        application: "unobserved".into(),
        outcome: "unobserved".into(),
        detail: None,
    };
    let Some(selector) = selector else {
        result.availability = if all.is_empty() {
            "empty_catalog"
        } else {
            "available_catalog"
        }
        .into();
        result.selector_state = "no_selector".into();
        result.packages = all;
        return Ok(result);
    };
    let legacy_root = crate::local_store::root(project).join("skills");
    let mut matches: Vec<_> = all
        .into_iter()
        .filter(|s| {
            s.identity_key == selector
                || s.slug.eq_ignore_ascii_case(selector)
                || s.name.eq_ignore_ascii_case(selector)
                || (Path::new(&s.source_path).parent() == Some(legacy_root.as_path())
                    && Path::new(&s.source_path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.eq_ignore_ascii_case(selector)))
        })
        .collect();
    if expected_digest.is_none() {
        matches.retain(|s| !matches!(s.lifecycle.as_str(), "superseded" | "deactivated"));
    }
    if matches.is_empty() {
        result.availability = "unavailable_package".into();
        return Ok(result);
    }
    if let Some(digest) = expected_digest {
        let exact: Vec<_> = matches
            .iter()
            .filter(|s| s.package_digest == digest)
            .cloned()
            .collect();
        if exact.is_empty() {
            result.availability = "changed_package".into();
            result.packages = matches;
            return Ok(result);
        }
        matches = exact;
    }
    let identities: BTreeSet<_> = matches
        .iter()
        .map(|s| (&s.identity_key, &s.package_digest))
        .collect();
    if identities.len() != 1 {
        result.availability = "ambiguous_package".into();
        result.packages = matches;
        return Ok(result);
    }
    // Prefer the live producer over its stored copy, retaining all equivalent
    // source paths in the response for inspection.
    matches.sort_by_key(|s| {
        Path::new(&s.source_path)
            .components()
            .any(|c| c.as_os_str() == ".stateroot")
    });
    let skill = &matches[0];
    result.metadata_digest = Some(metadata_digest(skill));
    result.package_metadata = Some(package_metadata(skill));
    result.evidence_assessments = assess_evidence(project, skill);
    result.projections = inspect_projections(project, home, skill)?;
    let reg = load_registry()?;
    let native = reg.harnesses.iter().find(|h| h.id == skill.native_harness);
    let requested = harness.map(normalize_harness);
    let target = requested
        .as_ref()
        .and_then(|h| reg.harnesses.iter().find(|e| e.id == *h));
    result.route = serde_json::json!({
        "native_harness": skill.native_harness, "native_invocation": skill.native_invocation,
        "delegation_mode": native.map(|h| h.delegation.mode.as_str()),
        "requested_harness": requested,
        "skill_isolation_declared": target.map(|h| h.delegation.skill_isolation.isolated_by_default).unwrap_or(false),
        "isolation_scope": "registry declaration for StateRoot-owned launches; not proof of this session",
    });
    if requested.is_some() && target.is_none() {
        result.availability = "unsupported_route".into();
    } else if skill.lifecycle == "reference_only" {
        result.availability = "reference_only".into();
    } else if skill.lifecycle == "external_only" || skill.compatibility["compatible"] == false {
        result.availability = "incompatible_package".into();
    }
    if let Some(relative) = file {
        // References expose the native route, never closed implementation.
        // Compatibility is factual metadata, not a denial of readable bytes.
        if skill.lifecycle != "reference_only" {
            match read_bound_file(skill, relative) {
                Ok(bytes) => {
                    result.content_base64 =
                        Some(base64::engine::general_purpose::STANDARD.encode(bytes));
                    result.delivery = "file_delivered; read_and_application_unobserved".into();
                }
                Err(error) => {
                    result.availability = "unavailable_or_changed_file".into();
                    result.detail = Some(error);
                }
            }
        }
    }
    result.packages = matches;
    Ok(result)
}

fn inspect_projections(
    project: &Path,
    home: Option<&Path>,
    skill: &DiscoveredSkill,
) -> Result<Vec<Value>, String> {
    let home = match home {
        Some(home) => home.to_path_buf(),
        None => home_dir()?,
    };
    let mut roots = BTreeSet::from([project.join(".agents/skills"), home.join(".agents/skills")]);
    for entry in load_registry()?.harnesses {
        roots.extend(
            entry
                .projection_roots
                .project
                .iter()
                .map(|p| project.join(p)),
        );
        roots.extend(
            entry
                .projection_roots
                .global
                .iter()
                .map(|p| expand_home_relative(p, &home)),
        );
    }
    let mut result = Vec::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(meta) = read_json_file(&path.join(PROJECTION_META)) else {
                continue;
            };
            if meta["identity_key"] != skill.identity_key {
                continue;
            }
            let wrapper = meta["projection_kind"] == "delegation_wrapper";
            let matches = meta["package_digest"] == skill.package_digest
                && projection_metadata_matches(&path, skill)
                && (wrapper
                    || skill.files.iter().all(|(rel, digest)| {
                        fs::read(path.join(rel)).is_ok_and(|bytes| sha256_hex(&bytes) == *digest)
                    }));
            result.push(serde_json::json!({"path": path, "state": if !matches { "changed_projection" }
                else if wrapper { "projected_route_wrapper" } else { "projected" },
                "package_digest": meta["package_digest"], "metadata_digest": meta["metadata_digest"],
                "delivery": "unobserved", "application": "unobserved", "outcome": "unobserved"}));
        }
    }
    Ok(result)
}

fn read_bound_file(skill: &DiscoveredSkill, relative: &str) -> Result<Vec<u8>, String> {
    let relative_path = safe_relative(relative)?;
    let expected = skill
        .files
        .get(relative)
        .ok_or("file absent from hashed manifest; inspect hash_exclusions")?;
    let root = Path::new(&skill.source_path);
    let refreshed = scan_skill_dir(root, &skill.harness, &skill.scope, &skill.source_kind)
        .ok_or("package unavailable since discovery")?;
    if refreshed.package_digest != skill.package_digest {
        return Err("package changed since discovery".into());
    }
    let path = checked_package_path(root, &relative_path, true)?;
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if sha256_hex(&bytes) != *expected {
        return Err("file changed since discovery".into());
    }
    Ok(bytes)
}

fn assess_evidence(project: &Path, skill: &DiscoveredSkill) -> Vec<Value> {
    let project_id = crate::local_store::read_manifest(project)
        .ok()
        .flatten()
        .and_then(|m| m["project_id"].as_str().map(str::to_owned));
    skill
        .package_metadata
        .get("evidence")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, reference)| {
            let kind = reference["kind"].as_str().unwrap_or("");
            let id = reference["reference"].as_str().unwrap_or("");
            let declared = reference["status"].as_str().unwrap_or("declared");
            let foreign = reference["project_id"]
                .as_str()
                .is_some_and(|p| Some(p) != project_id.as_deref());
            let bound = skill.package_metadata["evidence_package_digest"].as_str();
            let mut status = "declared_unverified";
            let mut detail = None;
            let mut resolved = Value::Null;
            if foreign {
                status = "foreign_project";
            } else if bound.is_some_and(|d| d != skill.package_digest) {
                status = "stale_package_binding";
            } else {
                match kind {
                    "plan" | "body" => {
                        if let Some((meta, body)) = crate::plans::load(project, id).filter(|(meta, _)| !id.is_empty() && meta.id == id) {
                            let actual = crate::plans::body_digest(&body);
                        resolved = serde_json::json!({"kind":"plan", "plan_id":meta.id, "body_digest":actual});
                            status = match (reference["body_digest"].as_str(), actual.as_deref()) {
                                (_, None) => "unavailable_reference",
                                (Some(wanted), Some(actual)) if wanted != actual => {
                                    "stale_reference"
                                }
                                _ => "reference_available; success_unverified",
                            };
                        } else {
                            status = "unavailable_reference";
                        }
                    }
                    "root" => {
                        status = if id.len() == 40 && id.bytes().all(|c| c.is_ascii_hexdigit()) && crate::roots::repository_owner(project)
                            .ok()
                            .and_then(|owner| git2::Repository::open(owner).ok())
                            .is_some()
                            && crate::roots::resolve_hash(project, id).is_ok()
                        {
                            "historical_reference_available"
                        } else {
                            "unavailable_reference"
                        };
                    }
                    "session" => {
                        status = if crate::sessions::resolve(project, id).is_ok_and(|session| !id.is_empty() && session.session_id == id) {
                            "reference_available; success_unverified"
                        } else {
                            "unavailable_reference"
                        };
                    }
                    "verification" => match crate::verification::load(project, id) {
                        Ok(record) if Some(record.project_id.as_str()) != project_id.as_deref() => {
                            status = "foreign_project"
                        }
                        Ok(record) => {
                            resolved = serde_json::json!({"kind":"verification", "verification_id":record.id,
                                "project_id":record.project_id, "plan_id":record.plan_id, "plan_body_digest":record.plan_body_sha256,
                                "product_tree":record.product_tree, "source_root":record.source_root});
                            let body = crate::plans::load(project, &record.plan_id)
                                .and_then(|(_, path)| crate::plans::body_digest(&path));
                            let supplied_body_matches = reference["body_digest"]
                                .as_str()
                                .is_none_or(|digest| digest == record.plan_body_sha256);
                            if body.as_deref() != Some(record.plan_body_sha256.as_str())
                                || !supplied_body_matches
                            {
                                status = "stale_reference";
                            } else if record.exit_code != Some(0)
                                || record.finished_at.is_none()
                                || record.error.is_some()
                                || record.status
                                    != "observed exit zero; semantic acceptance not established"
                            {
                                status = "not_successful_verification";
                            } else if declared == "historical" {
                                status = "historical_command_exit_zero; current_success_unverified";
                            } else {
                                // This existing comparison is read-only; validate()
                                // captures a tree and must not be used by inspection.
                                match crate::roots::product_tree_matches(
                                    project,
                                    &record.product_tree,
                                ) {
                                    Ok(true) => {
                                        status =
                                            "observed_command_exit_zero; skill_outcome_unverified"
                                    }
                                    Ok(false) => status = "stale_reference",
                                    Err(error) => {
                                        status = "unavailable_reference";
                                        detail = Some(error);
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            status = "unavailable_reference";
                            detail = Some(error);
                        }
                    },
                    // Delegation is owned by the CLI runtime. Preserve the full
                    // reference without inventing completion from worker status.
                    "delegation" => status = "unavailable_delegation_inspection",
                    _ => {}
                }
            }
            if declared == "historical"
                && matches!(
                    status,
                    "reference_available; success_unverified"
                        | "observed_command_exit_zero; skill_outcome_unverified"
                )
            {
                status = "historical_reference_available; current_success_unverified";
            }
            serde_json::json!({"reference_index": index, "kind": kind, "reference": id, "declared_status": declared, "assessment": status, "detail": detail, "resolved": resolved,
            "authority": "author_declared_reference; assessments do not establish skill success"})
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Home overrides alone do not isolate installed OpenClaw bundles: the
    // real scanner also follows PATH/npm roots. Run this test in a child with
    // those machine sources absent; never mutate process-global env alongside
    // other tests or weaken production discovery to satisfy a fixture.
    fn isolated(test: &str) -> bool {
        if std::env::var_os("STATEROOT_I2_ISOLATED_TEST").is_some() {
            return false;
        }
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .arg(format!("skill_federation::package::tests::{test}"))
            .arg("--exact")
            .env("STATEROOT_I2_ISOLATED_TEST", "1")
            .env("PATH", "");
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
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        let home = tmp.path().join("home");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&home).unwrap();
        crate::local_store::init_skeleton(&project, "test-project", "test", "test").unwrap();
        (tmp, project, home)
    }

    fn options() -> SyncOptions {
        SyncOptions {
            pull: true,
            push: true,
            dry_run: false,
            cmd_probe: Some(vec![]),
        }
    }

    fn request() -> SkillWriteRequest {
        serde_json::from_value(json!({
            "slug": "reuse-demo", "skill_md": "---\nname: reuse-demo\ndescription: Check a resource\n---\nRead references/context.txt, then run scripts/check.py.\n",
            "files_base64": {
                "references/context.txt": base64::engine::general_purpose::STANDARD.encode("retained context\n"),
                "scripts/check.py": base64::engine::general_purpose::STANDARD.encode("print('checked')\n"),
                "assets/sample.bin": base64::engine::general_purpose::STANDARD.encode([0, 255, 7, 3]),
            },
            "evidence": [{"kind": "session", "reference": "old-session", "status": "historical", "note": "author-reported useful past procedure", "future": {"keep": true}}]
        })).unwrap()
    }

    #[test]
    fn author_scanner_metadata_sync_projection_report_roundtrip() {
        if isolated("author_scanner_metadata_sync_projection_report_roundtrip") {
            return;
        }
        let (_tmp, project, home) = fixture();
        let request = request();
        let first = author_skill(&project, &home, "codex", &request, &options()).unwrap();
        assert_eq!(first.lifecycle, "active");
        let stored = project.join(".stateroot/skills/reuse-demo");
        let projected = project.join(".agents/skills/reuse-demo");
        let scan = discover_all(&project, Some(&home)).unwrap();
        let skill = scan.iter().find(|s| s.slug == "reuse-demo").unwrap();
        assert_eq!(skill.package_digest, first.package.package_digest);
        assert_eq!(skill.package_metadata["evidence"], json!(request.evidence));
        assert_eq!(skill.files.len(), 4);
        for relative in skill.files.keys() {
            assert_eq!(
                fs::read(stored.join(relative)).unwrap(),
                fs::read(projected.join(relative)).unwrap()
            );
        }
        let projected_meta = read_json_file(&projected.join(PROJECTION_META)).unwrap();
        assert_eq!(
            projected_meta["package_metadata"]["evidence"],
            skill.package_metadata["evidence"]
        );
        let report = packages_for_report(std::slice::from_ref(skill)).unwrap();
        assert_eq!(
            report[0]["package_metadata"]["evidence"],
            skill.package_metadata["evidence"]
        );
        assert_eq!(
            report[0]["package_files_base64"]["assets/sample.bin"],
            request.files_base64["assets/sample.bin"]
        );
        assert!(report[0]["projections"][0]["observation"]
            .as_str()
            .unwrap()
            .contains("unobserved"));
        let second = author_skill(&project, &home, "codex", &request, &options()).unwrap();
        assert_eq!(second.package.package_digest, first.package.package_digest);
        assert!(second
            .actions
            .iter()
            .filter(|a| a.slug == "reuse-demo")
            .all(|a| a.action == "unchanged"));
        let inspection = inspect_package(
            &project,
            Some(&home),
            Some(&skill.identity_key),
            Some(&skill.package_digest),
            Some("assets/sample.bin"),
            Some("codex"),
        )
        .unwrap();
        assert_eq!(
            inspection.content_base64,
            Some(request.files_base64["assets/sample.bin"].clone())
        );
        assert_eq!(inspection.route["skill_isolation_declared"], false);
        assert_eq!(inspection.selector_state, "explicit_selector");
        assert_eq!(inspection.selection, "unobserved");
        assert_eq!(inspection.application, "unobserved");
        assert_eq!(inspection.outcome, "unobserved");
        assert_eq!(inspection.projections[0]["state"], "projected");
        assert_eq!(
            inspection.evidence_assessments[0]["assessment"],
            "unavailable_reference"
        );
    }

    #[test]
    fn foreign_producer_retains_unknown_metadata_and_metadata_only_updates() {
        if isolated("foreign_producer_retains_unknown_metadata_and_metadata_only_updates") {
            return;
        }
        let (_tmp, project, home) = fixture();
        let native = home.join(".cursor/skills/foreign-demo");
        super::super::tests::write_skill(&native, "foreign-demo", "read me");
        fs::create_dir_all(native.join("references")).unwrap();
        fs::write(native.join("references/full.txt"), "full source resource").unwrap();
        write_json(&native.join(PACKAGE_META), &json!({
            "evidence": [{"kind":"plan","reference":"former-plan","status":"historical","source_span":{"start":5,"end":12}}],
            "future_evidence_contract": {"tool": "fixture", "version": 42}
        })).unwrap();
        let large = fs::File::create(native.join("references/large.bin")).unwrap();
        large.set_len(HASH_MAX_FILE_BYTES + 1).unwrap();
        sync_project(&project, &options(), Some(&home)).unwrap();
        let canonical = home.join(".stateroot/skills/foreign-demo");
        let projection = home.join(".agents/skills/foreign-demo");
        let first = scan_skill_dir(&canonical, "statesmith", "global", "portable").unwrap();
        assert!(first
            .hash_exclusions
            .iter()
            .any(|e| e.contains("references/large.bin")));
        assert_eq!(
            fs::metadata(projection.join("references/large.bin"))
                .unwrap()
                .len(),
            HASH_MAX_FILE_BYTES + 1
        );
        let report = packages_for_report(std::slice::from_ref(&first)).unwrap();
        assert_eq!(report[0]["hash_exclusions"], json!(first.hash_exclusions));
        assert!(report[0]["package_files_base64"]
            .get("references/large.bin")
            .is_none());
        assert_eq!(
            first.package_metadata["future_evidence_contract"]["version"],
            42
        );
        assert_eq!(
            fs::read(canonical.join("references/full.txt")).unwrap(),
            fs::read(projection.join("references/full.txt")).unwrap()
        );
        let mut native_meta = read_json_file(&native.join(PACKAGE_META)).unwrap();
        native_meta["evidence"][0]["note"] = json!("more source context");
        write_json(&native.join(PACKAGE_META), &native_meta).unwrap();
        let changed = sync_project(&project, &options(), Some(&home)).unwrap();
        assert!(changed
            .iter()
            .any(|a| a.slug == "foreign-demo" && a.action == "updated"));
        let second = scan_skill_dir(&canonical, "statesmith", "global", "portable").unwrap();
        assert_eq!(first.package_digest, second.package_digest);
        assert_ne!(metadata_digest(&first), metadata_digest(&second));
        assert_eq!(
            read_json_file(&projection.join(PROJECTION_META)).unwrap()["package_metadata"]
                ["evidence"],
            second.package_metadata["evidence"]
        );
        let stable = sync_project(&project, &options(), Some(&home)).unwrap();
        assert!(stable
            .iter()
            .filter(|a| a.slug == "foreign-demo")
            .all(|a| a.action == "unchanged"));
        // A change between discovery and reporting is not silently transmitted
        // under the old package digest.
        fs::write(canonical.join("references/full.txt"), "different").unwrap();
        assert!(packages_for_report(&[second])
            .unwrap_err()
            .contains("changed since discovery"));
    }

    #[test]
    fn exact_binding_legacy_ambiguity_missing_incompatible_and_routes() {
        if isolated("exact_binding_legacy_ambiguity_missing_incompatible_and_routes") {
            return;
        }
        let (_tmp, project, home) = fixture();
        let empty = inspect_package(&project, Some(&home), None, None, None, None).unwrap();
        assert_eq!(empty.availability, "empty_catalog");
        assert_eq!(empty.selector_state, "no_selector");
        assert_eq!(empty.selection, "unobserved");
        let first = author_skill(&project, &home, "codex", &request(), &options()).unwrap();
        let identity = first.package.identity_key.clone();
        let digest = first.package.package_digest.clone();
        let mut update = request();
        update.skill_md.push_str("Updated procedure.\n");
        update.evidence = None;
        update.files_base64.clear();
        let second = author_skill(&project, &home, "codex", &update, &options()).unwrap();
        assert_ne!(digest, second.package.package_digest);
        let stale = inspect_package(
            &project,
            Some(&home),
            Some(&identity),
            Some(&digest),
            Some("SKILL.md"),
            None,
        )
        .unwrap();
        assert_eq!(stale.availability, "changed_package");
        assert!(stale.content_base64.is_none());
        let current = inspect_package(
            &project,
            Some(&home),
            Some("reuse-demo"),
            None,
            Some("references/context.txt"),
            Some("pi"),
        )
        .unwrap();
        assert_eq!(current.route["skill_isolation_declared"], true);
        assert_eq!(
            current.evidence_assessments[0]["assessment"],
            "stale_package_binding"
        );
        assert!(
            current.content_base64.is_some(),
            "legacy write preserves resources"
        );
        assert_eq!(
            inspect_package(
                &project,
                Some(&home),
                Some(&identity),
                None,
                None,
                Some("missing-harness")
            )
            .unwrap()
            .availability,
            "unsupported_route"
        );
        super::super::tests::write_skill(
            &home.join(".cursor/skills/reuse-demo"),
            "reuse-demo",
            "different origin",
        );
        assert_eq!(
            inspect_package(
                &project,
                Some(&home),
                Some("reuse-demo"),
                None,
                Some("SKILL.md"),
                None
            )
            .unwrap()
            .availability,
            "ambiguous_package"
        );
        assert!(inspect_package(
            &project,
            Some(&home),
            Some(&identity),
            None,
            Some("SKILL.md"),
            None
        )
        .unwrap()
        .content_base64
        .is_some());
        fs::remove_file(project.join(".stateroot/skills/reuse-demo/SKILL.md")).unwrap();
        assert_eq!(
            inspect_package(
                &project,
                Some(&home),
                Some(&identity),
                None,
                Some("SKILL.md"),
                None
            )
            .unwrap()
            .availability,
            "unavailable_package"
        );
        let reference = home.join(".cursor/skills-cursor/native-only");
        super::super::tests::write_skill(&reference, "native-only", "reference capability");
        let inspected = inspect_package(
            &project,
            Some(&home),
            Some("native-only"),
            None,
            Some("SKILL.md"),
            None,
        )
        .unwrap();
        assert_eq!(inspected.availability, "reference_only");
        assert!(inspected.content_base64.is_none());
        assert_eq!(inspected.route["native_harness"], "cursor");
        let evidence =
            json!([{"kind":"session", "reference":"native-history", "status":"historical"}]);
        write_json(
            &reference.join(PACKAGE_META),
            &json!({"evidence": evidence}),
        )
        .unwrap();
        sync_project(&project, &options(), Some(&home)).unwrap();
        let wrapper = home.join(".agents/skills/native-only--cursor-ref");
        assert_eq!(
            read_json_file(&wrapper.join(PROJECTION_META)).unwrap()["package_metadata"]["evidence"],
            evidence
        );
        let reference = discover_all(&project, Some(&home))
            .unwrap()
            .into_iter()
            .find(|s| s.slug == "native-only")
            .unwrap();
        let report = packages_for_report(&[reference]).unwrap();
        assert!(report[0].get("package_files_base64").is_none());
        assert_eq!(report[0]["package_metadata"]["evidence"], evidence);
        super::super::tests::write_skill_with_metadata(
            &home.join(".openclaw/skills/unavailable"),
            "unavailable",
            json!({"openclaw":{"requires":{"bins":["certainly-missing-i2-fixture"]}}}),
        );
        assert_eq!(
            inspect_package(&project, Some(&home), Some("unavailable"), None, None, None)
                .unwrap()
                .availability,
            "incompatible_package"
        );
        let legacy = project.join(".stateroot/skills/legacy");
        super::super::tests::write_skill(&legacy, "Friendly Display", "old format");
        let result = inspect_package(
            &project,
            Some(&home),
            Some("legacy"),
            None,
            Some("SKILL.md"),
            None,
        )
        .unwrap();
        assert!(result.content_base64.is_some());
        assert!(result.evidence_assessments.is_empty());
    }

    #[test]
    fn evidence_claims_never_manufacture_current_success_and_catalog_stays_compact() {
        if isolated("evidence_claims_never_manufacture_current_success_and_catalog_stays_compact") {
            return;
        }
        let (_tmp, project, home) = fixture();
        let plan = crate::plans::record(
            &project,
            "source",
            "codex",
            None,
            "# source\nA checked procedure",
        )
        .unwrap();
        let (_, body) = crate::plans::load(&project, &plan.id).unwrap();
        let body_digest = crate::plans::body_digest(&body).unwrap();
        let mut request = request();
        request.evidence = Some(vec![
            json!({"kind":"plan","reference":plan.id,"status":"historical","body_digest":body_digest,"full_context":"x".repeat(20000)}),
            json!({"kind":"plan","reference":plan.id,"status":"observed","body_digest":"stale"}),
            json!({"kind":"verification","reference":"missing","status":"observed"}),
            json!({"kind":"plan","reference":plan.id,"status":"observed","project_id":"foreign"}),
            json!({"kind":"delegation","reference":"worker","status":"observed"}),
        ]);
        let package = author_skill(&project, &home, "codex", &request, &options())
            .unwrap()
            .package;
        let inspection = inspect_package(
            &project,
            Some(&home),
            Some(&package.identity_key),
            None,
            None,
            None,
        )
        .unwrap();
        let assessments: Vec<_> = inspection
            .evidence_assessments
            .iter()
            .map(|e| e["assessment"].as_str().unwrap())
            .collect();
        assert_eq!(
            assessments,
            [
                "historical_reference_available; current_success_unverified",
                "stale_reference",
                "unavailable_reference",
                "foreign_project",
                "unavailable_delegation_inspection"
            ]
        );
        let scan = serde_json::to_vec(&discover_all(&project, Some(&home)).unwrap()).unwrap();
        let detail = serde_json::to_vec(&inspection).unwrap();
        assert!(scan.len() < 10000, "catalog {} bytes", scan.len());
        assert!(
            detail.len() > 20000 && detail.len() < 30000,
            "detail retains metadata and source assessment"
        );
        assert!(!String::from_utf8(scan).unwrap().contains("full_context"));
        // Inspecting missing roots cannot initialize a repository.
        let other = project.join("uninitialized");
        fs::create_dir_all(&other).unwrap();
        let mut record = package;
        record.package_metadata["evidence"] = json!([{"kind":"root","reference":"missing"}]);
        assert_eq!(
            assess_evidence(&other, &record)[0]["assessment"],
            "unavailable_reference"
        );
        assert!(!other.join(".git").exists());
    }
    #[test]
    fn verification_evidence_is_read_only_and_never_upgrades_historical_sources() {
        if isolated("verification_evidence_is_read_only_and_never_upgrades_historical_sources") {
            return;
        }
        let (_tmp, project, home) = fixture();
        fs::write(project.join(".gitignore"), ".stateroot/\n.agents/\n").unwrap();
        fs::write(project.join("source.txt"), "before").unwrap();
        let plan =
            crate::plans::record(&project, "observed", "codex", None, "# observed source").unwrap();
        let observation =
            crate::verification::start(&project, &plan.id, "test", &["fixture-command".into()])
                .unwrap();
        let observation =
            crate::verification::finish(&project, observation, Some(0), None, vec![]).unwrap();
        let mut request = request();
        request.evidence = Some(vec![
            json!({"kind":"verification", "reference":observation.id, "status":"observed"}),
            json!({"kind":"verification", "reference":observation.id, "status":"historical"}),
        ]);
        let package = author_skill(&project, &home, "codex", &request, &options())
            .unwrap()
            .package;
        let current = assess_evidence(&project, &package);
        assert_eq!(
            current[0]["assessment"],
            "observed_command_exit_zero; skill_outcome_unverified"
        );
        assert_eq!(
            current[1]["assessment"],
            "historical_command_exit_zero; current_success_unverified"
        );
        let before = fs::read_dir(project.join(".git/objects")).unwrap().count();
        fs::write(project.join("source.txt"), "after").unwrap();
        let stale = assess_evidence(&project, &package);
        assert_eq!(stale[0]["assessment"], "stale_reference");
        assert_eq!(
            stale[1]["assessment"],
            "historical_command_exit_zero; current_success_unverified"
        );
        assert_eq!(
            fs::read_dir(project.join(".git/objects")).unwrap().count(),
            before
        );
        let mut changed = package;
        changed.package_metadata["evidence"][0]["body_digest"] = json!("foreign-body");
        assert_eq!(
            assess_evidence(&project, &changed)[0]["assessment"],
            "stale_reference"
        );
    }
    #[test]
    fn failed_resource_authoring_keeps_complete_active_package_and_projection() {
        if isolated("failed_resource_authoring_keeps_complete_active_package_and_projection") {
            return;
        }
        let (_tmp, project, home) = fixture();
        let first = author_skill(&project, &home, "codex", &request(), &options()).unwrap();
        let root = PathBuf::from(&first.package.source_path);
        let projection = project.join(".agents/skills/reuse-demo");
        let original_md = fs::read(root.join("SKILL.md")).unwrap();
        let original_meta = fs::read(root.join(PACKAGE_META)).unwrap();
        let original_projection = fs::read(projection.join(PROJECTION_META)).unwrap();
        let mut conflict = request();
        conflict
            .skill_md
            .push_str("Must never become active on failure.\n");
        conflict.files_base64 = BTreeMap::from([
            ("foo".into(), "YQ==".into()),
            ("foo/bar".into(), "Yg==".into()),
        ]);
        assert!(
            author_skill(&project, &home, "codex", &conflict, &options())
                .unwrap_err()
                .contains("paths conflict")
        );
        // Existing file used as a resource parent fails in the actual staged
        // filesystem operation, after SKILL.md was written only in the stage.
        conflict.files_base64 = BTreeMap::from([("scripts/check.py/nested".into(), "YQ==".into())]);
        assert!(
            author_skill(&project, &home, "codex", &conflict, &options())
                .unwrap_err()
                .contains("resource parent")
        );
        // Existing directory used as a file reaches atomic_replace and fails.
        conflict.files_base64 = BTreeMap::from([("scripts".into(), "YQ==".into())]);
        assert!(
            author_skill(&project, &home, "codex", &conflict, &options())
                .unwrap_err()
                .contains("write resource")
        );
        assert_eq!(fs::read(root.join("SKILL.md")).unwrap(), original_md);
        assert_eq!(fs::read(root.join(PACKAGE_META)).unwrap(), original_meta);
        assert_eq!(
            fs::read(projection.join(PROJECTION_META)).unwrap(),
            original_projection
        );
        assert_eq!(
            scan_skill_dir(&root, "codex", "project", "package")
                .unwrap()
                .package_digest,
            first.package.package_digest
        );
        assert!(!root.join("foo").exists());
        let local = project.join(".stateroot/local/skill-authoring");
        assert_eq!(fs::read_dir(&local).unwrap().count(), 0);
        // Crash leftovers are machine-local, never another active package or
        // source objects in a concurrent shared snapshot.
        super::super::tests::write_skill(
            &local.join(".reuse-demo-author-incomplete"),
            "incomplete-stage",
            "not active",
        );
        super::super::tests::write_skill(
            &local.join(".reuse-demo-previous-retained"),
            "retained-backup",
            "not active",
        );
        assert!(discover_all(&project, Some(&home))
            .unwrap()
            .iter()
            .all(|s| s.slug != "incomplete-stage" && s.slug != "retained-backup"));
        let (snapshot, _) =
            crate::roots::create_root(&project, "test", "test local exclusion", None).unwrap();
        let repo = git2::Repository::open(&project).unwrap();
        let tree = repo
            .find_commit(git2::Oid::from_str(&snapshot.id).unwrap())
            .unwrap()
            .tree()
            .unwrap();
        assert!(tree
            .get_path(Path::new(".stateroot/skills/reuse-demo/SKILL.md"))
            .is_ok());
        assert!(tree.get_path(Path::new(".stateroot/local")).is_err());
        assert!(fs::read_dir(root.parent().unwrap())
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().starts_with(".reuse-demo-")));
    }

    #[cfg(unix)]
    #[test]
    fn resource_updates_preserve_existing_executable_mode() {
        if isolated("resource_updates_preserve_existing_executable_mode") {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let (_tmp, project, home) = fixture();
        let first = author_skill(&project, &home, "codex", &request(), &options()).unwrap();
        let script = PathBuf::from(first.package.source_path).join("scripts/check.py");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let mut update = request();
        update.files_base64.insert(
            "scripts/check.py".into(),
            base64::engine::general_purpose::STANDARD.encode("print('updated')\n"),
        );
        author_skill(&project, &home, "codex", &update, &options()).unwrap();
        assert_eq!(
            fs::metadata(&script).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(project.join(".agents/skills/reuse-demo/scripts/check.py"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
}
