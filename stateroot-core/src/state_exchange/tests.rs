use super::*;
use crate::{local_store, plans, roots, skill_federation, verification};
use base64::Engine;
use git2::Repository;
use serde_json::json;
use std::io::Read;
use std::path::{Path, PathBuf};

fn write(project: &Path, relative: &str, bytes: &[u8]) {
    let path = project.join(relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

fn fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    PathBuf,
    String,
    String,
    String,
    String,
) {
    let temp = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let project = temp.path().join("donor");
    std::fs::create_dir(&project).unwrap();
    local_store::init_skeleton(&project, "local-n4b-fixture", "donor", "local").unwrap();
    write(&project, ".gitignore", b"ignored.txt\n");
    write(&project, ".staterootignore", b"private.txt\n");
    write(&project, "note.txt", b"root A\n");
    write(&project, "crlf.txt", b"first\r\nsecond\r\n");
    write(&project, "binary.bin", &(0..=255).collect::<Vec<u8>>());
    write(&project, "ignored.txt", b"excluded\n");
    write(&project, "private.txt", b"excluded\n");
    write(
        &project,
        ".stateroot/ingest-gov.json",
        b"{\"origin\":\"donor-only operational cursor\"}\n",
    );
    let plan = plans::record(
        &project,
        "exchange fixture",
        "test",
        None,
        "# Exchange fixture\n",
    )
    .unwrap();
    let p0 = roots::create_root(&project, "test", "prerequisite", None)
        .unwrap()
        .0
        .id;
    let observation_id = format!("v_{}", uuid::Uuid::now_v7());
    let request = skill_federation::SkillWriteRequest {
        slug: "proof".into(),
        skill_md: "---\nname: proof\ndescription: Read retained evidence\n---\nRead references/value.txt.\n".into(),
        name: None,
        rationale: Some("fixture".into()),
        files_base64: [("references/value.txt".into(), "cmV1c2UtdmFsdWUK".into())].into_iter().collect(),
        evidence: Some(vec![
            json!({"kind":"plan","reference":plan.id,"status":"historical"}),
            json!({"kind":"root","reference":p0,"status":"historical"}),
            json!({"kind":"verification","reference":observation_id,"status":"observed"}),
        ]),
    };
    skill_federation::author_skill(
        &project,
        home.path(),
        "test",
        &request,
        &skill_federation::SyncOptions::default(),
    )
    .unwrap();
    let observation = verification::start_reserved(
        &project,
        &plan.id,
        "test",
        &["fixture".into()],
        &observation_id,
    )
    .unwrap();
    let observation =
        verification::finish(&project, observation, Some(0), None, vec!["digest".into()]).unwrap();
    let root_a = roots::create_root(&project, "test", "root A", None)
        .unwrap()
        .0
        .id;
    (temp, home, project, p0, root_a, plan.id, observation.id)
}

fn export(project: &Path, root: &str) -> PathBuf {
    let bundle = project
        .parent()
        .unwrap()
        .join(format!("exchange-{}.srfs", uuid::Uuid::now_v7()));
    super::export(project, root, &bundle).unwrap();
    bundle
}

fn refs(project: &Path, prefix: &str) -> Vec<String> {
    let repo = Repository::open(project).unwrap();
    repo.references_glob(&format!("{prefix}*"))
        .unwrap()
        .flatten()
        .filter_map(|reference| reference.name().map(str::to_owned))
        .collect()
}

fn read_frames(path: &Path) -> (format::BundleManifest, Vec<(u8, String, Vec<u8>)>) {
    let mut reader = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let manifest = format::read_manifest(&mut reader).unwrap();
    let mut frames = Vec::new();
    for _ in 0..manifest.object_count + manifest.artifacts.len() {
        let (kind, name, length) = format::read_frame_header(&mut reader).unwrap();
        let mut payload = Vec::new();
        std::io::Read::by_ref(&mut reader)
            .take(length)
            .read_to_end(&mut payload)
            .unwrap();
        assert_eq!(payload.len() as u64, length);
        frames.push((kind, name, payload));
    }
    (manifest, frames)
}

fn write_frames(path: &Path, manifest: &format::BundleManifest, frames: &[(u8, String, Vec<u8>)]) {
    let mut file = std::fs::File::create(path).unwrap();
    format::write_manifest(&mut file, manifest).unwrap();
    for (kind, name, payload) in frames {
        format::write_frame_header(&mut file, *kind, name, payload.len() as u64).unwrap();
        std::io::Write::write_all(&mut file, payload).unwrap();
    }
}

#[test]
fn independent_roundtrip_preserves_evidence_bytes_and_ancestry() {
    let (_temp, _home, donor, p0, root_a, _plan, observation) = fixture();
    let bundle = export(&donor, &root_a);
    let inspected = inspect(&bundle).unwrap();
    assert!(inspected.full_evidence);
    assert!(inspected.auxiliary_count >= 2);
    assert_eq!(inspected.omission_count, 0);
    let offline = donor.parent().unwrap().join("donor-offline");
    std::fs::rename(&donor, &offline).unwrap();
    assert!(!donor.exists());

    let recipient = donor.parent().unwrap().join("recipient");
    let imported = import(&recipient, &bundle).unwrap();
    assert!(imported.full_evidence);
    assert!(roots::latest_root(&recipient).unwrap().is_none());
    let materialized = materialize(&recipient, &bundle).unwrap();
    assert_eq!(materialized.root_oid, root_a);
    assert_eq!(materialized.previous_root, None);
    assert!(materialized.fresh.unwrap());
    assert_eq!(materialized.project_id, "local-n4b-fixture");
    assert_eq!(
        std::fs::read(recipient.join("note.txt")).unwrap(),
        b"root A\n"
    );
    assert_eq!(
        std::fs::read(recipient.join("crlf.txt")).unwrap(),
        b"first\r\nsecond\r\n"
    );
    assert_eq!(
        std::fs::read(recipient.join("binary.bin")).unwrap(),
        (0..=255).collect::<Vec<u8>>()
    );
    assert!(!recipient.join("ignored.txt").exists());
    assert!(!recipient.join("private.txt").exists());
    assert!(!recipient.join(".stateroot/local/sessions").exists());
    assert!(!recipient.join(".git/objects/info/alternates").exists());
    assert!(
        !recipient.join(".stateroot/ingest-gov.json").exists(),
        "donor cursor must not become receiving-machine authority"
    );
    assert!(refs(&recipient, "refs/heads").is_empty());

    let imported_a = roots::get_root(&recipient, &root_a).unwrap();
    assert_eq!(imported_a.parents, vec![p0]);
    assert_eq!(imported_a.metadata_status, "recorded");
    assert!(verification::load(&recipient, &observation).is_ok());
    let recorded_tree = verification::load(&recipient, &observation)
        .unwrap()
        .product_tree;
    let current_tree = roots::current_product_tree(&recipient).unwrap();
    assert!(
        roots::product_tree_matches(&recipient, &recorded_tree).unwrap(),
        "product tree mismatch at {}: recorded={recorded_tree} current={current_tree}",
        recipient.display()
    );
    let skill = skill_federation::inspect_package(
        &recipient,
        None,
        Some("proof"),
        None,
        Some("references/value.txt"),
        None,
    )
    .unwrap();
    let resource = skill.content_base64.unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(resource)
            .unwrap(),
        b"reuse-value\n"
    );
    let assessments = skill.evidence_assessments;
    assert!(assessments
        .iter()
        .any(|entry| entry["kind"] == "verification"
            && entry["assessment"] == "observed_command_exit_zero; skill_outcome_unverified"));

    write(&recipient, "continuation.txt", b"root B\n");
    let root_b = roots::create_root(&recipient, "test", "root B", None)
        .unwrap()
        .0
        .id;
    assert_eq!(
        roots::get_root(&recipient, &root_b).unwrap().parents,
        vec![root_a]
    );
}

#[test]
fn corruption_missing_and_conflicting_destination_publish_nothing() {
    let (_temp, _home, donor, _p0, root_a, _plan, _observation) = fixture();
    let bundle = export(&donor, &root_a);
    let (manifest, mut frames) = read_frames(&bundle);
    let first = frames.iter_mut().find(|(kind, _, _)| *kind != 4).unwrap();
    first.2[0] ^= 1;
    let corrupt = donor.parent().unwrap().join("corrupt.srfs");
    write_frames(&corrupt, &manifest, &frames);
    let corrupt_dest = donor.parent().unwrap().join("corrupt-dest");
    assert!(import(&corrupt_dest, &corrupt).is_err());
    assert!(!corrupt_dest.join(".stateroot/manifest.json").exists());

    let (mut missing_manifest, mut frames) = read_frames(&bundle);
    let index = frames.iter().position(|(kind, _, _)| *kind != 4).unwrap();
    frames.remove(index);
    missing_manifest.object_count -= 1;
    let missing = donor.parent().unwrap().join("missing.srfs");
    write_frames(&missing, &missing_manifest, &frames);
    let missing_dest = donor.parent().unwrap().join("missing-dest");
    assert!(import(&missing_dest, &missing).is_err());
    assert!(!missing_dest.join(".stateroot/manifest.json").exists());

    let conflict = donor.parent().unwrap().join("conflict");
    std::fs::create_dir(&conflict).unwrap();
    local_store::init_skeleton(&conflict, "different-project", "conflict", "local").unwrap();
    write(&conflict, "keep.txt", b"preserve me\n");
    assert!(import(&conflict, &bundle).is_err());
    assert_eq!(
        std::fs::read(conflict.join("keep.txt")).unwrap(),
        b"preserve me\n"
    );
    assert!(!conflict.join(".git").exists());
}

#[test]
fn fast_forward_dirty_and_divergent_destinations_are_explicit() {
    let (_temp, _home, donor, _p0, _root_a, _plan, _observation) = fixture();
    write(&donor, "root-only.txt", b"removed in the next root\n");
    let root_a = roots::create_root(&donor, "test", "root-only deletion baseline", None)
        .unwrap()
        .0
        .id;
    let bundle_a = export(&donor, &root_a);
    std::fs::remove_file(donor.join("root-only.txt")).unwrap();
    write(&donor, "note.txt", b"root B\n");
    let root_b = roots::create_root(&donor, "test", "root B", None)
        .unwrap()
        .0
        .id;
    let bundle_b = export(&donor, &root_b);
    let recipient = donor.parent().unwrap().join("recipient-ff");
    materialize(&recipient, &bundle_a).unwrap();
    write(
        &recipient,
        ".stateroot/ingest-gov.json",
        b"{\"origin\":\"receiver cursor\"}\n",
    );
    write(&recipient, "keep.txt", b"untracked\n");
    write(&recipient, "note.txt", b"local dirty\n");
    assert!(materialize(&recipient, &bundle_b).is_err());
    assert_eq!(
        std::fs::read(recipient.join("note.txt")).unwrap(),
        b"local dirty\n"
    );
    assert_eq!(roots::latest_root(&recipient).unwrap().unwrap(), root_a);

    write(&recipient, "note.txt", b"root A\n");
    let report = materialize(&recipient, &bundle_b).unwrap();
    assert_eq!(report.previous_root, Some(root_a.clone()));
    assert!(!report.fresh.unwrap());
    assert_eq!(
        std::fs::read(recipient.join("note.txt")).unwrap(),
        b"root B\n"
    );
    assert_eq!(
        std::fs::read(recipient.join("keep.txt")).unwrap(),
        b"untracked\n"
    );
    assert_eq!(roots::latest_root(&recipient).unwrap().unwrap(), root_b);
    assert_eq!(
        std::fs::read(recipient.join(".stateroot/ingest-gov.json")).unwrap(),
        b"{\"origin\":\"receiver cursor\"}\n"
    );
    assert!(
        !recipient.join("root-only.txt").exists(),
        "root-only deletions are independent of the user's Git index"
    );

    let excluded = donor.parent().unwrap().join("recipient-exclusions");
    materialize(&excluded, &bundle_a).unwrap();
    write(&excluded, ".staterootignore", b"private.txt\nnote.txt\n");
    write(&excluded, "note.txt", b"keep excluded local work\n");
    assert!(materialize(&excluded, &bundle_b).is_err());
    assert_eq!(
        std::fs::read(excluded.join("note.txt")).unwrap(),
        b"keep excluded local work\n"
    );
    assert_eq!(
        std::fs::read(excluded.join(".staterootignore")).unwrap(),
        b"private.txt\nnote.txt\n"
    );
    assert_eq!(roots::latest_root(&excluded).unwrap().unwrap(), root_a);

    write(&recipient, "note.txt", b"recipient C\n");
    let recipient_c = roots::create_root(&recipient, "test", "recipient C", None)
        .unwrap()
        .0
        .id;
    assert!(materialize(&recipient, &bundle_b).is_err());
    assert_eq!(
        std::fs::read(recipient.join("note.txt")).unwrap(),
        b"recipient C\n"
    );
    let latest = roots::latest_root(&recipient).unwrap().unwrap();
    assert_eq!(
        latest, recipient_c,
        "latest={latest} A={root_a} B={root_b} C={recipient_c}"
    );
}

#[test]
fn omissions_and_invalid_frames_never_claim_full_evidence() {
    let (_temp, _home, donor, _p0, root_a, plan, _observation) = fixture();
    let id = format!("v_{}", uuid::Uuid::now_v7());
    let record = json!({
        "schema_version": verification::SCHEMA,
        "id": id,
        "project_id": "local-n4b-fixture",
        "plan_id": plan,
        "plan_body_sha256": "digest",
        "product_tree": "1111111111111111111111111111111111111111",
        "source_root": null,
        "actor": "test",
        "command": ["fixture"],
        "started_at": local_store::now_rfc3339(),
        "finished_at": null,
        "exit_code": null,
        "status": "started; completion unavailable",
        "output_digests": [],
        "error": null,
    });
    write(
        &donor,
        &format!(".stateroot/plans/verification/{id}.started.json"),
        serde_json::to_string_pretty(&record).unwrap().as_bytes(),
    );
    let root_with_omission = roots::create_root(&donor, "test", "omission", None)
        .unwrap()
        .0
        .id;
    let bundle = export(&donor, &root_with_omission);
    let report = inspect(&bundle).unwrap();
    assert!(!report.full_evidence);
    assert!(report.omission_count >= 1);
    let (mut hidden_manifest, frames) = read_frames(&bundle);
    hidden_manifest.omissions.clear();
    let hidden = donor.parent().unwrap().join("hidden-omission.srfs");
    write_frames(&hidden, &hidden_manifest, &frames);
    assert!(
        inspect(&hidden).is_err(),
        "removing an omission cannot create a full-evidence claim"
    );
    let recipient = donor.parent().unwrap().join("omission-recipient");
    materialize(&recipient, &bundle).unwrap();
    assert!(refs(&recipient, &format!("refs/stateroot/verification/{id}")).is_empty());

    let mut bad_magic = std::fs::read(&bundle).unwrap();
    bad_magic[..8].copy_from_slice(b"SRFSBND1");
    let bad = donor.parent().unwrap().join("bad-magic.srfs");
    std::fs::write(&bad, bad_magic).unwrap();
    assert!(inspect(&bad).is_err());

    let trailing = donor.parent().unwrap().join("trailing.srfs");
    std::fs::copy(&bundle, &trailing).unwrap();
    std::io::Write::write_all(
        &mut std::fs::OpenOptions::new()
            .append(true)
            .open(&trailing)
            .unwrap(),
        &[0],
    )
    .unwrap();
    assert!(inspect(&trailing).is_err());

    let (mut duplicate_manifest, mut frames) = read_frames(&bundle);
    let first = frames
        .iter()
        .find(|(kind, _, _)| *kind != 4)
        .unwrap()
        .clone();
    frames.insert(0, first);
    duplicate_manifest.object_count += 1;
    let duplicate = donor.parent().unwrap().join("duplicate.srfs");
    write_frames(&duplicate, &duplicate_manifest, &frames);
    assert!(inspect(&duplicate).is_err());

    let _ = root_a;
}

#[test]
fn portable_path_and_manifest_bounds_are_enforced() {
    for name in [
        "", ".", "..", ".GIT", "NUL", "con.txt", "file:", "space ", "dot.", "a\\b",
    ] {
        assert!(!closure::valid_component(name), "accepted {name:?}");
    }
    for name in ["normal.txt", "unicode-界", ".stateroot"] {
        assert!(closure::valid_component(name), "rejected {name:?}");
    }
    let (_temp, _home, donor, _p0, root_a, _plan, _observation) = fixture();
    let bundle = export(&donor, &root_a);
    let (mut manifest, _frames) = read_frames(&bundle);
    let repo = Repository::open(&donor).unwrap();
    let link = repo.blob(b"../outside-project").unwrap();
    let mut unsupported = repo.treebuilder(None).unwrap();
    unsupported.insert("link", link, 0o120000).unwrap();
    assert!(
        closure::enumerate_tree(
            &repo,
            unsupported.write().unwrap(),
            &mut closure::Closure::default()
        )
        .is_err(),
        "unsupported link entries must refuse before destination files can change"
    );
    let mut aliased = manifest.clone();
    aliased.auxiliary_refs.push(format::AuxiliaryRef {
        reference: "refs/stateroot/roots/1111111111111111111111111111111111111111".into(),
        oid: root_a.clone(),
        kind: "commit".into(),
    });
    assert!(
        format::validate_manifest(&aliased).is_err(),
        "canonical root ref names must equal their target OID"
    );
    manifest.object_count = format::OBJECT_COUNT + 1;
    assert!(format::validate_manifest(&manifest).is_err());
}

#[test]
fn late_writes_and_lineage_races_preserve_destination_work() {
    let (_temp, _home, donor, _p0, root_a, _plan, observation) = fixture();
    let bundle_a = export(&donor, &root_a);
    write(&donor, "note.txt", b"root B\n");
    let root_b = roots::create_root(&donor, "test", "root B", None)
        .unwrap()
        .0
        .id;
    let bundle_b = export(&donor, &root_b);
    let recipient = donor.parent().unwrap().join("recipient-late-write");
    materialize(&recipient, &bundle_a).unwrap();
    let declaration = std::fs::read(recipient.join(".stateroot/reproducibility.json")).unwrap();
    destination::after_checkout_for_test(|_repo, destination| {
        write(destination, "note.txt", b"late destination work\n");
    });
    assert!(materialize(&recipient, &bundle_b).is_err());
    assert_eq!(
        std::fs::read(recipient.join("note.txt")).unwrap(),
        b"late destination work\n"
    );
    assert_eq!(roots::latest_root(&recipient).unwrap().unwrap(), root_a);

    write(&recipient, "note.txt", b"root A\n");
    let recipient_c = roots::create_root(&recipient, "test", "recipient C", None)
        .unwrap()
        .0
        .id;
    write(&recipient, ".stateroot/reproducibility.json", &declaration);
    safe_io::update_ref_cas(
        &Repository::open(&recipient).unwrap(),
        &local_store::root(&recipient).join("local/locks"),
        "refs/stateroot/latest",
        Some(recipient_c.parse().unwrap()),
        root_a.parse().unwrap(),
        "reset for race fixture",
    )
    .unwrap();
    let race_root = recipient_c.clone();
    destination::after_checkout_for_test(move |repo, destination| {
        write(destination, "note.txt", b"concurrent bytes and lineage\n");
        repo.reference(
            "refs/stateroot/latest",
            race_root.parse().unwrap(),
            true,
            "concurrent lineage",
        )
        .unwrap();
    });
    let race_error = materialize(&recipient, &bundle_b).unwrap_err();
    assert_eq!(
        std::fs::read(recipient.join("note.txt")).unwrap(),
        b"concurrent bytes and lineage\n",
        "race error: {race_error}"
    );
    let latest = roots::latest_root(&recipient).unwrap().unwrap();
    assert_eq!(
        latest, recipient_c,
        "latest={latest} A={root_a} B={root_b} C={recipient_c}"
    );

    let ref_race = donor.parent().unwrap().join("recipient-ref-race");
    materialize(&ref_race, &bundle_a).unwrap();
    let verification_ref = format!("refs/stateroot/verification/{observation}");
    let changed_ref = verification_ref.clone();
    destination::after_checkout_for_test(move |repo, _destination| {
        let different = repo.treebuilder(None).unwrap().write().unwrap();
        repo.reference(&changed_ref, different, true, "intervening conflicting ref")
            .unwrap();
    });
    assert!(materialize(&ref_race, &bundle_b).is_err());
    let repo = Repository::open(&ref_race).unwrap();
    assert_eq!(
        repo.refname_to_id(&verification_ref).unwrap(),
        repo.treebuilder(None).unwrap().write().unwrap()
    );
    assert_eq!(roots::latest_root(&ref_race).unwrap().unwrap(), root_a);
    assert_eq!(
        std::fs::read(ref_race.join("note.txt")).unwrap(),
        b"root A\n"
    );
}
