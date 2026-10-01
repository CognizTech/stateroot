//! Optional continuity synthesis — flag-gated, advisory-only.
//!
//! `[continuity] synthesis = true` (default OFF) plus the existing
//! `[synthesis]` credentials lets a provider attach ONE labeled advisory
//! line to a project's continuity projection. The contract is hard:
//!
//! - receives only the bounded deterministic snapshot (attention items,
//!   obligation counts, plan directive) — never transcripts or memories;
//! - hash-idempotent: one provider call per distinct snapshot;
//! - writes a provenance-labeled advisory (`source: "synthesis"`) shown
//!   only while its inputs hash matches the current assessment;
//! - can never create, complete, assign, snooze, suppress, or reprioritize
//!   deterministic obligations — it writes one text field, nothing else;
//! - failure or missing credentials always falls back to deterministic
//!   reconciliation (no advisory, never an error).

use serde::{Deserialize, Serialize};
use stateroot_core::continuity::{self, ContinuityAdvisory, ContinuityAssessment};
use stateroot_core::local_store::now_rfc3339;

use super::{note, Ctx};

const GOV_REL: &str = "local/continuity-synthesis.json";
const GOV_SCHEMA: &str = "stateroot.continuity-synthesis.v1";
const ADVISORY_CHAR_CAP: usize = 400;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Gov {
    #[serde(default)]
    schema_version: String,
    #[serde(default)]
    last_inputs_hash: String,
    #[serde(default)]
    last_run_at: String,
}

fn gov_path(project_dir: &std::path::Path) -> std::path::PathBuf {
    stateroot_core::local_store::root(project_dir).join(GOV_REL)
}

fn read_gov(project_dir: &std::path::Path) -> Gov {
    std::fs::read_to_string(gov_path(project_dir))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_gov(project_dir: &std::path::Path, gov: &Gov) {
    let path = gov_path(project_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(value) = serde_json::to_value(gov) {
        let _ = stateroot_core::safe_io::atomic_replace_json(&path, &value);
    }
}

/// The bounded snapshot the provider may see — the deterministic assessment
/// only, already reduced to its decision-relevant fields.
fn bounded_snapshot(assessment: &ContinuityAssessment) -> serde_json::Value {
    serde_json::json!({
        "attention": assessment.attention.iter().take(10).map(|item| serde_json::json!({
            "kind": item.kind,
            "title": item.title,
            "action": item.action,
        })).collect::<Vec<_>>(),
        "open_obligations": assessment.open_obligations,
        "plan": {
            "id": assessment.current_plan_id,
            "status": assessment.current_plan_status,
            "directive": assessment.plan_directive,
        },
    })
}

/// Synthesize (or reuse) the advisory for one project. Returns the advisory
/// text when one was written this run. Every gate failure is a silent
/// deterministic fallback — this never blocks continuity.
pub async fn maybe_advise(ctx: &Ctx, project_dir: &std::path::Path) -> Option<String> {
    // Gate 1: the flag. Default config makes NO provider call, provably —
    // this is the first check and returns before any endpoint resolution.
    if !ctx.config.continuity.synthesis || !ctx.config.synthesis.enabled {
        return None;
    }
    // Gate 2: credentials. Absence falls back to deterministic-only.
    let endpoint = super::compiler::resolved_endpoint()?;
    // Gate 3: a projection to label (reconcile runs first).
    let assessment = continuity::read_projection(project_dir)?;
    // Gate 4: hash idempotency — one call per distinct snapshot.
    let gov = read_gov(project_dir);
    if !gov.last_inputs_hash.is_empty() && gov.last_inputs_hash == assessment.inputs_hash {
        return None;
    }
    let system = "You label a deterministic StateRoot continuity snapshot with ONE short advisory \
                  sentence for the receiving agent. You may only restate priorities the snapshot \
                  already implies — never invent work, never add obligations, never change order. \
                  Plain text, at most 280 characters, no markdown.";
    let user = serde_json::to_string(&bounded_snapshot(&assessment)).ok()?;
    let text = match super::compiler::call_provider(ctx, &endpoint, system, &user).await {
        Ok(text) => text,
        Err(err) => {
            note!("continuity synthesis failed ({err:#}) — deterministic projection stands");
            return None;
        }
    };
    let capped: String = text.trim().chars().take(ADVISORY_CHAR_CAP).collect();
    if capped.is_empty() {
        return None;
    }
    let advisory = ContinuityAdvisory {
        schema_version: continuity::SCHEMA_ADVISORY_V1.into(),
        inputs_hash: assessment.inputs_hash.clone(),
        text: capped.clone(),
        generated_at: now_rfc3339(),
        source: "synthesis".into(),
    };
    if let Err(err) = continuity::write_advisory(project_dir, &advisory) {
        note!("continuity advisory write failed: {err}");
        return None;
    }
    write_gov(
        project_dir,
        &Gov {
            schema_version: GOV_SCHEMA.into(),
            last_inputs_hash: assessment.inputs_hash.clone(),
            last_run_at: now_rfc3339(),
        },
    );
    Some(capped)
}

/// Test seam: the gate order is the contract — with the flag off, no
/// endpoint is resolved and no snapshot is read.
#[cfg(test)]
pub fn synthesis_gated(ctx: &Ctx) -> bool {
    !ctx.config.continuity.synthesis || !ctx.config.synthesis.enabled
}

#[cfg(test)]
fn test_ctx(project: &std::path::Path, config_dir: &std::path::Path, synthesis: bool) -> Ctx {
    let mut config = stateroot_core::config::AppConfig::default();
    config.continuity.synthesis = synthesis;
    Ctx {
        cwd: project.to_path_buf(),
        config_dir: config_dir.to_path_buf(),
        config,
    }
}

#[cfg(test)]
struct EnvGuard {
    keys: Vec<&'static str>,
}

#[cfg(test)]
impl EnvGuard {
    fn set(pairs: &[(&'static str, &str)]) -> Self {
        let mut keys = Vec::new();
        for (key, value) in pairs {
            // SAFETY: tests hold test_env::env_lock().
            unsafe { std::env::set_var(key, value) };
            keys.push(*key);
        }
        Self { keys }
    }
}

#[cfg(test)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        for key in &self.keys {
            // SAFETY: tests hold test_env::env_lock().
            unsafe { std::env::remove_var(key) };
        }
    }
}

#[cfg(test)]
fn skeleton_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    stateroot_core::local_store::init_skeleton(dir.path(), "p", "n", "default").expect("init");
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_gates_before_any_work() {
        let dir = tempfile::tempdir().expect("tmp");
        let ctx = Ctx {
            cwd: dir.path().to_path_buf(),
            config_dir: dir.path().to_path_buf(),
            config: stateroot_core::config::AppConfig::default(),
        };
        // Default: continuity.synthesis = false → gated, even with keys set.
        assert!(synthesis_gated(&ctx));
    }

    #[test]
    fn snapshot_is_bounded() {
        let assessment = ContinuityAssessment {
            schema_version: continuity::SCHEMA_CONTINUITY_V1.into(),
            generated_at: "2026-10-01T00:00:00Z".into(),
            inputs_hash: "sha256:x".into(),
            attention: vec![continuity::AttentionItem {
                id: "plan_closure:p".into(),
                kind: "plan_closure".into(),
                rank: 20,
                title: "t".into(),
                detail: "d".into(),
                action: "a".into(),
                plan_id: Some("p".into()),
                obligation_id: None,
                handoff_seq: None,
                delegation_id: None,
            }],
            open_obligations: 2,
            corrupt_obligation_events: 0,
            current_plan_id: Some("p".into()),
            current_plan_status: Some("active".into()),
            plan_directive: "close".into(),
            service_registered: true,
            service_kind: None,
            service_running: true,
            service_last_beat_at: None,
        };
        let snapshot = bounded_snapshot(&assessment);
        let text = snapshot.to_string();
        assert!(text.contains("plan_closure"));
        assert!(text.contains("\"directive\":\"close\""));
        // The snapshot never carries volatile service fields or timestamps.
        assert!(!text.contains("beat"));
        assert!(!text.contains("generated_at"));
    }

    // env_lock serializes env mutation across parallel tests; holding it
    // across the provider await is exactly what it exists for.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn flag_off_never_calls_provider_even_with_credentials() {
        let _lock = crate::test_env::env_lock();
        let server = wiremock::MockServer::start().await;
        let _env = EnvGuard::set(&[
            ("DEEPSEEK_API_KEY", "test-key"),
            ("STATEROOT_SYNTHESIS_API_BASE", &server.uri()),
        ]);
        let project = skeleton_project();
        let config_home = tempfile::tempdir().expect("config");
        // Reconcile so a projection exists — the flag gate fires FIRST, so
        // the provider still never hears from us.
        continuity::reconcile(
            project.path(),
            config_home.path(),
            &stateroot_core::config::ContinuityConfig::default(),
        )
        .expect("reconcile");
        let ctx = test_ctx(project.path(), config_home.path(), false);
        assert!(maybe_advise(&ctx, project.path()).await.is_none());
        assert!(
            server
                .received_requests()
                .await
                .expect("requests")
                .is_empty(),
            "default config must make no provider call"
        );
    }

    // env_lock serializes env mutation across parallel tests; holding it
    // across the provider await is exactly what it exists for.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn enabled_synthesizes_labeled_advisory_and_dedups_by_hash() {
        let _lock = crate::test_env::env_lock();
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/chat/completions"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"choices":[{"message":{"content":"record the completion receipt first"}}]}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let _env = EnvGuard::set(&[
            ("DEEPSEEK_API_KEY", "test-key"),
            ("STATEROOT_SYNTHESIS_API_BASE", &server.uri()),
        ]);
        let project = skeleton_project();
        let config_home = tempfile::tempdir().expect("config");
        let assessment = continuity::reconcile(
            project.path(),
            config_home.path(),
            &stateroot_core::config::ContinuityConfig::default(),
        )
        .expect("reconcile");
        let attention_before = assessment.attention.len();
        let ctx = test_ctx(project.path(), config_home.path(), true);

        // First run: one provider call, one provenance-labeled advisory.
        let text = maybe_advise(&ctx, project.path())
            .await
            .expect("advisory written");
        assert_eq!(text, "record the completion receipt first");
        let current = continuity::read_projection(project.path()).expect("projection");
        assert_eq!(
            continuity::current_advisory(project.path(), &current).as_deref(),
            Some("record the completion receipt first")
        );
        // Second run against the SAME snapshot: hash dedup — no new call.
        assert!(maybe_advise(&ctx, project.path()).await.is_none());
        server.verify().await;

        // Advisory-only: the deterministic state is untouched.
        assert_eq!(current.attention.len(), attention_before);
        assert!(stateroot_core::obligations::list(project.path()).is_empty());
        assert!(stateroot_core::plans::list(project.path()).is_empty());
        // The advisory never leaks into the projection document itself.
        let raw = std::fs::read_to_string(
            project
                .path()
                .join(".stateroot/local/projections/continuity.v1.json"),
        )
        .expect("projection file");
        assert!(!raw.contains("record the completion receipt"));
    }

    // env_lock serializes env mutation across parallel tests; holding it
    // across the provider await is exactly what it exists for.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn provider_failure_falls_back_to_deterministic() {
        let _lock = crate::test_env::env_lock();
        let _env = EnvGuard::set(&[
            ("DEEPSEEK_API_KEY", "test-key"),
            // Closed port: the call must fail fast and quietly.
            ("STATEROOT_SYNTHESIS_API_BASE", "http://127.0.0.1:1"),
        ]);
        let project = skeleton_project();
        let config_home = tempfile::tempdir().expect("config");
        continuity::reconcile(
            project.path(),
            config_home.path(),
            &stateroot_core::config::ContinuityConfig::default(),
        )
        .expect("reconcile");
        let ctx = test_ctx(project.path(), config_home.path(), true);
        assert!(maybe_advise(&ctx, project.path()).await.is_none());
        // No advisory file, no error — the deterministic projection stands.
        let assessment = continuity::read_projection(project.path()).expect("projection");
        assert!(continuity::current_advisory(project.path(), &assessment).is_none());
    }
}
