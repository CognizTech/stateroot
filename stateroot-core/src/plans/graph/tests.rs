use super::*;

fn project() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("project");
    std::fs::create_dir_all(dir.join(".stateroot")).unwrap();
    (tmp, dir)
}

fn plan(project: &Path, title: &str) -> PlanMeta {
    let p = record(
        project,
        title,
        "codex",
        None,
        &format!("# {title}\nDo the work"),
    )
    .unwrap();
    transition(project, &p.id, PlanStatus::Approved).unwrap().0
}

fn declare(project: &Path, p: &PlanMeta, dependencies: Vec<PlanDependency>) -> PlanMeta {
    declare_execution(
        project,
        &p.id,
        &revision(project, &p.id).unwrap(),
        ExecutionDeclaration {
            dependencies,
            ..Default::default()
        },
        "codex",
    )
    .unwrap()
}

fn edge(project: &Path, p: &PlanMeta) -> PlanDependency {
    PlanDependency {
        plan_id: p.id.clone(),
        body_digest: body_digest(&body_path(project, &p.id)).unwrap(),
        kind: DependencyKind::Completion,
        outputs: vec![],
        acceptance: None,
    }
}

fn node(project: &Path, id: &str) -> PlanReadiness {
    graph(project)
        .nodes
        .into_iter()
        .find(|n| n.id == id)
        .unwrap()
}

#[test]
fn diamond_declaration_edits_are_cas_checked_and_never_reset_lifecycle() {
    let (_tmp, dir) = project();
    let a = plan(&dir, "I1 capture");
    let b = plan(&dir, "I2 skills");
    let c = plan(&dir, "I5 sessions");
    let d = plan(&dir, "I4 federation");
    let original_body = std::fs::read(body_path(&dir, &d.id)).unwrap();
    let old_revision = revision(&dir, &a.id).unwrap();
    declare(&dir, &a, vec![]);
    declare(&dir, &b, vec![edge(&dir, &a)]);
    declare(&dir, &c, vec![edge(&dir, &a)]);
    declare(&dir, &d, vec![edge(&dir, &b), edge(&dir, &c)]);
    assert_eq!(graph(&dir).ready.as_slice(), std::slice::from_ref(&a.id));
    assert_eq!(graph(&dir).observation_id, graph(&dir).observation_id);
    assert!(declare_execution(
        &dir,
        &a.id,
        &old_revision,
        ExecutionDeclaration::default(),
        "codex"
    )
    .unwrap_err()
    .contains("revision"));
    let cycle = ExecutionDeclaration {
        dependencies: vec![edge(&dir, &d)],
        ..Default::default()
    };
    assert!(
        declare_execution(&dir, &a.id, &revision(&dir, &a.id).unwrap(), cycle, "codex")
            .unwrap_err()
            .contains("cycle")
    );
    let cycle = ExecutionDeclaration {
        dependencies: vec![edge(&dir, &a)],
        ..Default::default()
    };
    assert!(
        declare_execution(&dir, &a.id, &revision(&dir, &a.id).unwrap(), cycle, "codex")
            .unwrap_err()
            .contains("cycle")
    );
    complete(&dir, &a.id, "source tests", "codex", None).unwrap();
    assert_eq!(graph(&dir).ready.len(), 2);
    complete(&dir, &b.id, "source tests", "codex", None).unwrap();
    complete(&dir, &c.id, "source tests", "codex", None).unwrap();
    assert_eq!(graph(&dir).ready.as_slice(), std::slice::from_ref(&d.id));
    let reread = load(&dir, &d.id).unwrap().0;
    assert_eq!(reread.status, d.status);
    assert_eq!(reread.approved_digest, d.approved_digest);
    assert_eq!(
        std::fs::read(body_path(&dir, &d.id)).unwrap(),
        original_body
    );
    assert_eq!(reread.schema_version, SCHEMA_PLAN_V2);
    assert!(declare_execution(
        &dir,
        &a.id,
        &revision(&dir, &a.id).unwrap(),
        ExecutionDeclaration::default(),
        "codex"
    )
    .unwrap_err()
    .contains("immutable"));
}

#[test]
fn legacy_missing_abandoned_superseded_unsupported_and_bad_receipt_remain_distinct() {
    let (_tmp, dir) = project();
    let upstream = plan(&dir, "upstream");
    let downstream = plan(&dir, "downstream");
    assert_eq!(
        node(&dir, &upstream.id).readiness,
        "dependencies_not_declared"
    );
    let upstream_edge = edge(&dir, &upstream);
    declare(&dir, &downstream, vec![upstream_edge.clone()]);
    assert_eq!(
        node(&dir, &downstream.id).dependencies[0].state,
        "incomplete"
    );
    let declaration = ExecutionDeclaration {
        superseded_by: Some("exact-new-plan".into()),
        ..Default::default()
    };
    declare_execution(
        &dir,
        &upstream.id,
        &revision(&dir, &upstream.id).unwrap(),
        declaration,
        "codex",
    )
    .unwrap();
    assert_eq!(
        node(&dir, &downstream.id).dependencies[0].state,
        "superseded"
    );
    declare(&dir, &upstream, vec![]);
    transition(&dir, &upstream.id, PlanStatus::Abandoned).unwrap();
    assert_eq!(
        node(&dir, &downstream.id).dependencies[0].state,
        "abandoned"
    );
    let raw = std::fs::read(meta_path(&dir, &upstream.id)).unwrap();
    std::fs::remove_file(meta_path(&dir, &upstream.id)).unwrap();
    assert_eq!(node(&dir, &downstream.id).dependencies[0].state, "missing");
    let mut value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    value["schema_version"] = serde_json::json!("stateroot.plan.v99");
    std::fs::write(
        meta_path(&dir, &upstream.id),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    assert_eq!(
        node(&dir, &downstream.id).dependencies[0].state,
        "unsupported"
    );
    assert_eq!(node(&dir, &upstream.id).readiness, "unsupported");
    value["schema_version"] = serde_json::json!(SCHEMA_PLAN_V2);
    value["status"] = serde_json::json!("done");
    std::fs::write(
        meta_path(&dir, &upstream.id),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    assert_eq!(
        node(&dir, &downstream.id).dependencies[0].state,
        "receipt_missing"
    );
    value["completion_receipt"] = serde_json::json!({"completed_at":"now","completed_by":"codex","evidence":"test","body_digest":"wrong"});
    std::fs::write(
        meta_path(&dir, &upstream.id),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    assert_eq!(
        node(&dir, &downstream.id).dependencies[0].state,
        "receipt_mismatch"
    );
    std::fs::write(body_path(&dir, &upstream.id), "changed body").unwrap();
    assert_eq!(
        node(&dir, &downstream.id).dependencies[0].state,
        "body_mismatch"
    );
}

#[test]
fn imported_cycle_is_reported_without_rewriting_and_annotations_roundtrip() {
    let (_tmp, dir) = project();
    let a = plan(&dir, "a");
    let b = plan(&dir, "b");
    declare(&dir, &a, vec![]);
    declare(&dir, &b, vec![edge(&dir, &a)]);
    let path = meta_path(&dir, &a.id);
    let mut raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    raw["execution"]["dependencies"] = serde_json::json!([edge(&dir, &b)]);
    raw["annotation"] = serde_json::json!({"native":"retained", "authority":false});
    let bytes = serde_json::to_vec(&raw).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(node(&dir, &a.id).dependencies[0].state, "cycle");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    transition(&dir, &a.id, PlanStatus::Active).unwrap();
    let updated: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(updated["annotation"], raw["annotation"]);
    assert_eq!(updated["execution"], raw["execution"]);
}

fn source_fixture() -> (
    tempfile::TempDir,
    PathBuf,
    PlanMeta,
    PlanMeta,
    String,
    String,
    PathBuf,
) {
    let (tmp, dir) = project();
    std::fs::write(dir.join("contract.rs"), "base").unwrap();
    let upstream = plan(&dir, "producer");
    let downstream = plan(&dir, "consumer");
    let base = crate::roots::create_root(&dir, "codex", "base", None)
        .unwrap()
        .0
        .id;
    let fork = "producer-fork";
    crate::roots::fork_root(&dir, &base, Some(fork), "codex").unwrap();
    let work = tmp.path().join("producer");
    crate::roots::fork_materialize(&dir, fork, &work, None).unwrap();
    std::fs::write(work.join("contract.rs"), "required").unwrap();
    let source = crate::roots::create_root(&work, "codex", "producer source", None)
        .unwrap()
        .0
        .id;
    // Receipt publication is independent from source publication in the consumer.
    complete(
        &dir,
        &upstream.id,
        "producer tests passed",
        "codex",
        Some(source.clone()),
    )
    .unwrap();
    let mut dependency = edge(&dir, &upstream);
    dependency.kind = DependencyKind::Source;
    dependency.outputs = vec![RequiredOutput {
        path: "contract.rs".into(),
        sha256: digest(b"required"),
    }];
    declare(&dir, &downstream, vec![dependency]);
    (tmp, dir, upstream, downstream, base, source, work)
}

#[test]
fn retained_staged_ancestral_but_rejected_source_never_becomes_available() {
    let (_tmp, dir, _upstream, downstream, _base, source, _work) = source_fixture();
    assert_eq!(
        node(&dir, &downstream.id).dependencies[0].state,
        "source_unpublished"
    );
    let attempt =
        crate::roots::prepare_merge_attempt(&dir, &["producer-fork".into()], "codex").unwrap();
    crate::roots::stage_merge_attempt(&dir, &attempt.id, &[]).unwrap();
    assert_eq!(
        node(&dir, &downstream.id).dependencies[0].state,
        "source_unpublished"
    );
    crate::roots::abort_merge_attempt(&dir, &attempt.id).unwrap();
    std::fs::write(dir.join("contract.rs"), "main revised").unwrap();
    crate::roots::create_root(&dir, "codex", "main source", None).unwrap();
    let attempt =
        crate::roots::prepare_merge_attempt(&dir, &["producer-fork".into()], "codex").unwrap();
    assert_eq!(attempt.state, "attention");
    let candidate = PathBuf::from(attempt.worktree.unwrap());
    std::fs::write(candidate.join("contract.rs"), "main revised").unwrap();
    crate::roots::stage_merge_attempt(&dir, &attempt.id, &[]).unwrap();
    crate::roots::continue_merge_attempt(&dir, &attempt.id, &[]).unwrap();
    assert!(
        published_contains(&dir, &source),
        "upstream really is a merge ancestor"
    );
    assert_eq!(
        node(&dir, &downstream.id).dependencies[0].state,
        "source_unpublished"
    );
}

#[test]
fn published_outputs_dirty_reverted_and_native_compatible_acceptance_are_source_bound() {
    let (_tmp, dir, upstream, downstream, base, _source, _work) = source_fixture();
    crate::roots::merge_forks(&dir, &["producer-fork".into()], "codex").unwrap();
    assert_eq!(node(&dir, &downstream.id).readiness, "ready");
    std::fs::write(dir.join("contract.rs"), "dirty").unwrap();
    assert_eq!(
        node(&dir, &downstream.id).dependencies[0].state,
        "source_changed"
    );
    assert!(accept_source(
        &dir,
        &downstream.id,
        &upstream.id,
        &revision(&dir, &downstream.id).unwrap(),
        "codex",
        "compatible"
    )
    .unwrap_err()
    .contains("unpublished"));
    crate::roots::create_root(&dir, "codex", "compatible source", None).unwrap();
    accept_source(
        &dir,
        &downstream.id,
        &upstream.id,
        &revision(&dir, &downstream.id).unwrap(),
        "codex",
        "reviewed compatible revised contract",
    )
    .unwrap();
    let row = node(&dir, &downstream.id);
    assert_eq!(row.readiness, "ready");
    assert!(row.dependencies[0].reason.contains("native coordinator"));
    std::fs::write(dir.join("contract.rs"), "changed again").unwrap();
    assert_eq!(
        node(&dir, &downstream.id).dependencies[0].state,
        "acceptance_stale"
    );
    crate::roots::revert_to_root(&dir, &base, "codex").unwrap();
    // Restore old source while preserving the declaration to test revalidation.
    let latest = crate::roots::latest_root(&dir).unwrap().unwrap();
    assert!(root_output(&dir, &latest, "contract.rs").as_deref() != Some(&digest(b"required")));
}

#[test]
fn claimed_assignment_is_not_running_and_unknown_workers_block_dispatch() {
    let (tmp, dir) = project();
    let p = plan(&dir, "assigned");
    declare(&dir, &p, vec![]);
    let root = crate::roots::create_root(&dir, "codex", "base", None)
        .unwrap()
        .0
        .id;
    crate::roots::fork_root(&dir, &root, Some("executor"), "codex").unwrap();
    let work = tmp.path().join("executor");
    crate::roots::fork_materialize(&dir, "executor", &work, Some(&p.id)).unwrap();
    transition(&work, &p.id, PlanStatus::Active).unwrap();
    assert_eq!(node(&dir, &p.id).worker_state, "awaiting_activity");
    let d = dir.join(".stateroot/delegations");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("uncertain.json"), serde_json::to_vec(&serde_json::json!({"schema_version":"stateroot.delegation.v2","id":"uncertain","plan_id":p.id,"status":"starting"})).unwrap()).unwrap();
    assert_eq!(node(&dir, &p.id).worker_state, "unknown");
    assert_eq!(node(&dir, &p.id).readiness, "blocked");
}

#[test]
fn programme_relationship_fixture_and_ownership_overlap_are_explicit() {
    let (_tmp, dir) = project();
    let mut programme = BTreeMap::new();
    for name in ["I1", "I2", "I3", "I4", "I5", "I6", "E1", "E2"] {
        programme.insert(name, plan(&dir, name));
    }
    // Handoff202's approved relationship topology, recorded through the same
    // supported operation used by CLI declare, never parsed from plan prose.
    for (name, prerequisites) in [
        ("I1", vec![]),
        ("I2", vec![]),
        ("I5", vec![]),
        ("E2", vec![]),
        ("I3", vec!["I2"]),
        ("I4", vec!["I1", "I2"]),
        ("I6", vec!["I1", "I2", "I3", "I4", "I5"]),
        ("E1", vec!["I2", "I3"]),
    ] {
        declare(
            &dir,
            &programme[name],
            prerequisites
                .iter()
                .map(|p| edge(&dir, &programme[p]))
                .collect(),
        );
    }
    let ready: BTreeSet<_> = graph(&dir)
        .nodes
        .into_iter()
        .filter(|n| n.readiness == "ready")
        .map(|n| n.title)
        .collect();
    assert_eq!(
        ready,
        ["I1", "I2", "I5", "E2"]
            .into_iter()
            .map(str::to_string)
            .collect()
    );
    for name in ["I1", "I2"] {
        declare_execution(
            &dir,
            &programme[name].id,
            &revision(&dir, &programme[name].id).unwrap(),
            ExecutionDeclaration {
                owned_paths: vec!["shared/contract.rs".into()],
                ..Default::default()
            },
            "codex",
        )
        .unwrap();
    }
    assert_eq!(graph(&dir).ownership_overlaps.len(), 1);
    assert_eq!(
        graph(&dir).ready.len(),
        4,
        "graph independence makes no conflict-free promise"
    );
}

#[test]
fn divergent_v2_forks_require_reconciliation_and_unchanged_side_roundtrips() {
    let (tmp, dir) = project();
    let p = plan(&dir, "owner");
    let a = plan(&dir, "a");
    let b = plan(&dir, "b");
    declare(&dir, &p, vec![]);
    let base = crate::roots::create_root(&dir, "codex", "base", None)
        .unwrap()
        .0
        .id;
    for (fork, dependency) in [("left", &a), ("right", &b)] {
        crate::roots::fork_root(&dir, &base, Some(fork), "codex").unwrap();
        let target = tmp.path().join(fork);
        crate::roots::fork_materialize(&dir, fork, &target, None).unwrap();
        declare_execution(
            &target,
            &p.id,
            &revision(&target, &p.id).unwrap(),
            ExecutionDeclaration {
                dependencies: vec![edge(&dir, dependency)],
                assignment: Some(PlanAssignment {
                    harness: fork.into(),
                    fork: None,
                    dispatch: None,
                }),
                ..Default::default()
            },
            fork,
        )
        .unwrap();
        std::fs::write(target.join(format!("{fork}.txt")), fork).unwrap();
        crate::roots::create_root(&target, "codex", fork, None).unwrap();
    }
    let attempt =
        crate::roots::prepare_merge_attempt(&dir, &["left".into(), "right".into()], "codex")
            .unwrap();
    assert_eq!(attempt.state, "attention");
    assert!(attempt
        .conflicts
        .iter()
        .any(|c| c.path == format!(".stateroot/plans/{}.json", p.id)));
    crate::roots::abort_merge_attempt(&dir, &attempt.id).unwrap();
    // Trunk's declaration is unchanged: the regular three-way merge preserves
    // the changed fork's complete v2 declaration without invoking a union.
    crate::roots::merge_forks(&dir, &["left".into()], "codex").unwrap();
    let merged = load(&dir, &p.id).unwrap().0;
    assert_eq!(
        merged
            .execution
            .unwrap()
            .declaration
            .assignment
            .unwrap()
            .harness,
        "left"
    );
    assert_eq!(node(&dir, &p.id).dependencies[0].dependency.plan_id, a.id);
}

#[test]
fn graph_read_is_read_only_even_before_first_root() {
    let (_tmp, dir) = project();
    let p = plan(&dir, "read only");
    let before = std::fs::read(meta_path(&dir, &p.id)).unwrap();
    std::fs::remove_dir_all(dir.join(".git")).unwrap();
    graph(&dir);
    assert!(!dir.join(".git").exists());
    assert_eq!(std::fs::read(meta_path(&dir, &p.id)).unwrap(), before);
}

#[test]
fn nonoverlapping_dependency_assignment_edits_conflict_but_identical_edits_merge() {
    for identical in [false, true] {
        let (tmp, dir) = project();
        let p = plan(&dir, "owner");
        let upstream = plan(&dir, "upstream");
        let initial = ExecutionDeclaration {
            assignment: Some(PlanAssignment {
                harness: "codex".into(),
                fork: None,
                dispatch: None,
            }),
            ..Default::default()
        };
        declare_execution(
            &dir,
            &p.id,
            &revision(&dir, &p.id).unwrap(),
            initial.clone(),
            "codex",
        )
        .unwrap();
        let base = crate::roots::create_root(&dir, "codex", "base", None)
            .unwrap()
            .0
            .id;
        for fork in ["left", "right"] {
            crate::roots::fork_root(&dir, &base, Some(fork), "codex").unwrap();
            let target = tmp.path().join(fork);
            crate::roots::fork_materialize(&dir, fork, &target, None).unwrap();
            let mut declaration = initial.clone();
            if identical || fork == "left" {
                declaration.dependencies.push(edge(&dir, &upstream));
            } else {
                declaration.assignment.as_mut().unwrap().harness = "claude".into();
            }
            declare_execution(
                &target,
                &p.id,
                &revision(&target, &p.id).unwrap(),
                declaration,
                fork,
            )
            .unwrap();
            std::fs::write(target.join(format!("{fork}.txt")), fork).unwrap();
            crate::roots::create_root(&target, "codex", fork, None).unwrap();
        }
        let attempt =
            crate::roots::prepare_merge_attempt(&dir, &["left".into(), "right".into()], "codex")
                .unwrap();
        assert_eq!(attempt.state, if identical { "ready" } else { "attention" });
    }
}

#[test]
fn live_execution_declaration_edit_cannot_be_overwritten_by_prepared_merge() {
    let (tmp, dir) = project();
    let p = plan(&dir, "owner");
    let upstream = plan(&dir, "upstream");
    declare(&dir, &p, vec![]);
    let base = crate::roots::create_root(&dir, "codex", "base", None)
        .unwrap()
        .0
        .id;
    crate::roots::fork_root(&dir, &base, Some("child"), "codex").unwrap();
    let target = tmp.path().join("child");
    crate::roots::fork_materialize(&dir, "child", &target, None).unwrap();
    declare(&target, &p, vec![edge(&dir, &upstream)]);
    std::fs::write(target.join("source.txt"), "child").unwrap();
    crate::roots::create_root(&target, "codex", "child", None).unwrap();
    let attempt = crate::roots::prepare_merge_attempt(&dir, &["child".into()], "codex").unwrap();
    declare_execution(
        &dir,
        &p.id,
        &revision(&dir, &p.id).unwrap(),
        ExecutionDeclaration {
            assignment: Some(PlanAssignment {
                harness: "claude".into(),
                fork: None,
                dispatch: None,
            }),
            ..Default::default()
        },
        "coordinator",
    )
    .unwrap();
    transition(&dir, &p.id, PlanStatus::Active).unwrap();
    let before = std::fs::read(meta_path(&dir, &p.id)).unwrap();
    assert!(crate::roots::continue_merge_attempt(&dir, &attempt.id, &[]).is_err());
    assert_eq!(std::fs::read(meta_path(&dir, &p.id)).unwrap(), before);
    assert_eq!(crate::roots::latest_root(&dir).unwrap().unwrap(), base);
}

#[test]
fn automatic_owner_requires_shared_project_registration_and_keeps_independent_scope() {
    let (tmp, dir) = project();
    local_store::init_skeleton(&dir, "owner-project", "owner", "fixture").unwrap();
    let p = plan(&dir, "automatic ownership");
    let declaration = ExecutionDeclaration {
        assignment: Some(PlanAssignment {
            harness: "claude".into(),
            fork: None,
            dispatch: Some(AuthorizedDispatch {
                operation_id: "operation".into(),
                task: "bounded fixture".into(),
                authorization: "fixture owner authorization".into(),
            }),
        }),
        ..Default::default()
    };
    declare_execution(
        &dir,
        &p.id,
        &revision(&dir, &p.id).unwrap(),
        declaration.clone(),
        "codex",
    )
    .unwrap();
    let base = crate::roots::create_root(&dir, "codex", "owner fixture", None)
        .unwrap()
        .0
        .id;
    crate::roots::fork_root(&dir, &base, Some("worker"), "codex").unwrap();
    let fork = tmp.path().join("worker");
    crate::roots::fork_materialize(&dir, "worker", &fork, Some(&p.id)).unwrap();
    assert_eq!(
        automatic_dispatch_owner(&fork).unwrap(),
        dir.canonicalize().unwrap()
    );
    std::fs::remove_file(fork.join(".stateroot/local/fork-context.json")).unwrap();
    assert!(automatic_dispatch_owner(&fork)
        .unwrap_err()
        .contains("fork context"));
    // A separately identified StateRoot project in the same Git worktree has
    // its own authority; the abandoned owner's programme must not govern it.
    let path = fork.join(".stateroot/manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    manifest["project_id"] = serde_json::json!("independent-project");
    std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    transition(&dir, &p.id, PlanStatus::Abandoned).unwrap();
    assert_eq!(
        automatic_dispatch_authority(&fork, &p.id).unwrap(),
        fork.canonicalize().unwrap()
    );
    assert_eq!(node(&fork, &p.id).readiness, "ready");
}
