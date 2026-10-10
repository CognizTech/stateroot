//! Per-harness integration health — the compact typed seam shared by
//! `stateroot doctor --json`, `stateroot install --json`, the editor setup
//! flow, and the WS4 fidelity renderer.
//!
//! Pure read-only evidence: registry-driven detection (binary + config),
//! hook registration, instruction-block/agent-file persona projection, MCP
//! registration, skill projection, and — when a project directory is given —
//! the digest-delivery ledger and episodic capture trail. No model or
//! network calls, no writes, no credential/content scanning.
//!
//! Truth contract: `configured` never reads as `observed_working` — only a
//! delivered digest or a captured checkpoint proves flow. Evidence that
//! cannot be read stays `unknown`, never guessed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::detect::{self, Prober};
use super::registry::{self, DeliveryTier, HarnessQuirk, HookFormat};
use super::{paths, BLOCK_BEGIN};

/// Schema id of the serialized [`IntegrationHealth`] document.
pub const INTEGRATION_HEALTH_SCHEMA: &str = "stateroot.integration-health.v1";

/// Top-line classification for one harness integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationStatus {
    /// Integration artifacts present AND digest delivery or checkpoint
    /// capture observed for this harness in the project.
    ObservedWorking,
    /// Integration artifacts present; no flow observed yet (a fresh
    /// harness session is the confirmation path).
    Configured,
    /// Harness present (or recorded) but required integration artifacts
    /// are absent or unreadable.
    Missing,
    /// Cannot tell: recorded as installed but no evidence survives, or the
    /// harness exposes no installable integration surface to this CLI.
    Unknown,
}

impl IntegrationStatus {
    /// Stable string form.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ObservedWorking => "observed_working",
            Self::Configured => "configured",
            Self::Missing => "missing",
            Self::Unknown => "unknown",
        }
    }
}

/// State of one evidence component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentState {
    /// Evidence present and readable.
    Ok,
    /// Expected but absent.
    Missing,
    /// Present but unreadable/invalid.
    Unreadable,
    /// This harness has no such integration surface.
    NotApplicable,
}

impl ComponentState {
    /// True when the component is present and readable.
    pub fn ok(self) -> bool {
        self == Self::Ok
    }
}

/// One evidence component with a human detail line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentHealth {
    pub state: ComponentState,
    pub detail: String,
}

impl ComponentHealth {
    fn new(state: ComponentState, detail: impl Into<String>) -> Self {
        Self {
            state,
            detail: detail.into(),
        }
    }

    fn not_applicable() -> Self {
        Self::new(ComponentState::NotApplicable, "n/a")
    }
}

/// Health of one harness integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessHealth {
    /// Canonical harness id.
    pub harness: String,
    /// Display name.
    pub display: String,
    /// Registry install-completeness tier (`A` | `B` | `C`).
    pub tier: String,
    pub status: IntegrationStatus,
    /// Harness present on this machine (binary or config marker).
    pub detected: bool,
    /// Detected harness binary version when PROVEN by a bounded
    /// `--version` probe of the resolved binary; `null` when unknown.
    /// Never inferred from prose, docs, or file names.
    pub version: Option<String>,
    /// Harness binary evidence.
    pub binary: ComponentHealth,
    /// Harness config-marker evidence.
    pub config: ComponentHealth,
    /// stateroot hook registration evidence.
    pub hooks: ComponentHealth,
    /// Instruction-block / agent-file (persona+rules) projection evidence.
    pub instructions: ComponentHealth,
    /// MCP server registration evidence.
    pub mcp: ComponentHealth,
    /// Skill projection evidence.
    pub skills: ComponentHealth,
    /// Identity-delivery capability tier for this harness.
    pub delivery_tier: DeliveryTier,
    /// Short delivery policy note (registry-authored).
    pub delivery_note: String,
    /// Degraded-delivery explanation when the tier is degraded.
    pub degraded: Option<String>,
    /// Last digest delivery observed in the project (RFC3339).
    pub last_delivery: Option<String>,
    /// Last checkpoint capture attributed to this harness (RFC3339).
    pub last_capture: Option<String>,
    /// Specific broken/incomplete components.
    pub problems: Vec<String>,
    /// Recommended repair commands (ordered).
    pub repair: Vec<String>,
}

/// The actual outcome of one `stateroot install` pass — carried by the
/// `install --json` document so a machine caller never has to infer success
/// from prose, and a partial integration can never read as Ready. Absent
/// from documents that are not install outcomes (doctor, status).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallOutcome {
    /// Harnesses whose integration steps completed without an ERROR action.
    pub configured: Vec<String>,
    /// Harnesses whose integration steps FAILED — an explicit partial
    /// install. A caller must not record a success receipt while this is
    /// non-empty.
    pub failed: Vec<String>,
    /// No agent harness was detected on this machine: a legitimate CLI-only
    /// install. Distinct from agent readiness — the CLI is set up, no
    /// harness integration exists.
    pub cli_only: bool,
}

/// Whole-machine integration health document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationHealth {
    pub schema_version: String,
    pub generated_at: String,
    pub harnesses: Vec<HarnessHealth>,
    /// Store-level evidence problems (unreadable/corrupt capture or delivery
    /// evidence). Distinct from "no flow observed": absent evidence says
    /// nothing happened; a problem here says we cannot tell. Read-only.
    #[serde(default)]
    pub evidence_problems: Vec<String>,
    /// The actual install outcome — present only in the `install --json`
    /// document.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<InstallOutcome>,
}

impl IntegrationHealth {
    /// `(observed_working, configured, missing, unknown)` row counts.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut counts = (0, 0, 0, 0);
        for row in &self.harnesses {
            match row.status {
                IntegrationStatus::ObservedWorking => counts.0 += 1,
                IntegrationStatus::Configured => counts.1 += 1,
                IntegrationStatus::Missing => counts.2 += 1,
                IntegrationStatus::Unknown => counts.3 += 1,
            }
        }
        counts
    }

    /// Compact one-line summary for install/init/status surfaces.
    pub fn summary_line(&self) -> String {
        let (working, configured, missing, unknown) = self.counts();
        let mut parts = Vec::new();
        if working > 0 {
            parts.push(format!("{working} observed-working"));
        }
        if configured > 0 {
            parts.push(format!("{configured} configured (not yet observed)"));
        }
        if missing > 0 {
            parts.push(format!("{missing} needing install"));
        }
        if unknown > 0 {
            parts.push(format!("{unknown} unknown"));
        }
        if parts.is_empty() {
            "no harness integrations detected".to_string()
        } else {
            parts.join(" · ")
        }
    }
}

/// Every stateroot hook command found in one installed hook config. Shared
/// by health evidence and doctor's hook-binary grading so both read configs
/// the same way. Each format is decoded by its real grammar (TOML values are
/// TOML-decoded, JSON shapes are walked structurally) — never a raw text
/// search, so an escaped quoted `stateroot.exe` path with spaces or Unicode
/// is found exactly the way the harness will read it.
pub fn extract_hook_commands(path: &Path, format: HookFormat) -> Vec<String> {
    match format {
        HookFormat::TomlHooks => {
            let Ok(text) = std::fs::read_to_string(path) else {
                return Vec::new();
            };
            let Ok(doc) = toml::from_str::<toml::Value>(&text) else {
                return Vec::new();
            };
            doc.get("hooks")
                .and_then(toml::Value::as_array)
                .map(|hooks| {
                    hooks
                        .iter()
                        .filter_map(|entry| entry.get("command").and_then(toml::Value::as_str))
                        .filter(|command| is_stateroot_hook_command(command))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        }
        HookFormat::ZeroExecJson => {
            let Ok(text) = std::fs::read_to_string(path) else {
                return Vec::new();
            };
            let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
                return Vec::new();
            };
            doc.get("hooks")
                .and_then(serde_json::Value::as_array)
                .map(|hooks| {
                    hooks
                        .iter()
                        .filter(|entry| {
                            entry.get("command").and_then(serde_json::Value::as_str)
                                == Some("stateroot")
                                && entry
                                    .get("args")
                                    .and_then(serde_json::Value::as_array)
                                    .and_then(|args| args.first())
                                    .and_then(serde_json::Value::as_str)
                                    == Some("hook")
                        })
                        .map(|_| "stateroot".to_string())
                        .collect()
                })
                .unwrap_or_default()
        }
        HookFormat::NativePlugin => {
            // The generated extension invokes bare `stateroot` via execFile.
            let Ok(text) = std::fs::read_to_string(path.join("index.ts")) else {
                return Vec::new();
            };
            if text.contains("\"stateroot\"") {
                vec!["stateroot".to_string()]
            } else {
                Vec::new()
            }
        }
        HookFormat::NamedGroupsJson => {
            // `{"stateroot": {<event>: [handlers]}}` — handlers are flat
            // `{command}` or nested `{matcher, hooks: [{command}]}`.
            let Ok(text) = std::fs::read_to_string(path) else {
                return Vec::new();
            };
            let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            if let Some(groups) = doc.get("stateroot").and_then(serde_json::Value::as_object) {
                collect_event_map_commands(groups, &mut out);
            }
            out
        }
        _ => {
            // NestedJson / FlatJson / CopilotJson: `{"hooks": {<event>:
            // [entries]}}`; devin's hooks.v1.json IS the event map.
            let Ok(text) = std::fs::read_to_string(path) else {
                return Vec::new();
            };
            let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            let root = if path.file_name() == Some("hooks.v1.json".as_ref()) {
                doc.as_object()
            } else {
                doc.get("hooks").and_then(serde_json::Value::as_object)
            };
            if let Some(map) = root {
                collect_event_map_commands(map, &mut out);
            }
            out
        }
    }
}

/// Walk one `event -> [entries]` map: an entry's command is `entry.command`
/// (flat handler) or each `entry.hooks[].command` (nested matcher shape).
fn collect_event_map_commands(
    map: &serde_json::Map<String, serde_json::Value>,
    out: &mut Vec<String>,
) {
    let mut push = |entry: &serde_json::Value| {
        if let Some(command) = entry.get("command").and_then(serde_json::Value::as_str) {
            if is_stateroot_hook_command(command) {
                out.push(command.to_string());
            }
        }
    };
    for value in map.values() {
        let Some(entries) = value.as_array() else {
            continue;
        };
        for entry in entries {
            push(entry);
            if let Some(hooks) = entry.get("hooks").and_then(serde_json::Value::as_array) {
                for nested in hooks {
                    push(nested);
                }
            }
        }
    }
}

/// The registered-hook matcher: true only for a command that invokes a
/// stateroot binary as `<binary> hook …` — bare `stateroot`, an absolute
/// path ending in `stateroot`/`stateroot.exe` (either separator, quoted or
/// not, spaces and Unicode included). A command that merely MENTIONS
/// stateroot (a wrapper script, a log path) is not a registration.
pub fn is_stateroot_hook_command(command: &str) -> bool {
    binary_of_command(command).is_some()
}

/// The binary a stateroot hook command invokes: bare `stateroot`, or the
/// (possibly quoted) path before the ` hook <event> --harness <id>` suffix
/// the installer writes. The binary itself may contain spaces, so the split
/// anchors on the LAST ` hook ` delimiter — never on the first whitespace.
pub fn binary_of_command(command: &str) -> Option<String> {
    let command = command.trim();
    if command == "stateroot" {
        return Some("stateroot".to_string());
    }
    let (binary, _) = command.rsplit_once(" hook ")?;
    let binary = binary.trim().trim_matches('"');
    if binary.is_empty() {
        return None;
    }
    // Both separators: configs are read on the OS that wrote them, and a
    // shared home can carry a Windows-written path to a unix reader.
    let name = binary.rsplit(['/', '\\']).next().unwrap_or(binary);
    if binary == "stateroot" || name == "stateroot" || name.eq_ignore_ascii_case("stateroot.exe") {
        Some(binary.to_string())
    } else {
        None
    }
}

/// Last durable capture per harness from the WS1 observation store
/// (immutable v2 segments + legacy v1 spool) — the ONLY capture evidence
/// health may promote on. The authored episodic journal is never consulted:
/// an ordinary checkpoint or a hand-written "via hook" note is not a
/// captured observation. Unreadable/corrupt evidence is diagnosed via
/// [`crate::observations::capture_trail`], never silently absent.
pub fn last_capture_by_harness(project_dir: &Path) -> BTreeMap<String, String> {
    crate::observations::capture_trail(project_dir).last_by_harness
}

fn binary_component(detection: &detect::Detection) -> ComponentHealth {
    if detection.binary_found {
        ComponentHealth::new(
            ComponentState::Ok,
            detection
                .evidence
                .iter()
                .find(|e| e.starts_with("binary "))
                .cloned()
                .unwrap_or_else(|| "binary on PATH".to_string()),
        )
    } else {
        ComponentHealth::new(ComponentState::Missing, "no harness binary on PATH")
    }
}

fn config_component(detection: &detect::Detection) -> ComponentHealth {
    if detection.config_dir {
        ComponentHealth::new(
            ComponentState::Ok,
            detection
                .evidence
                .iter()
                .find(|e| e.starts_with("config "))
                .cloned()
                .unwrap_or_else(|| "config marker present".to_string()),
        )
    } else {
        ComponentHealth::new(ComponentState::Missing, "no config marker under home")
    }
}

fn hooks_component(home: &Path, quirk: &HarnessQuirk) -> ComponentHealth {
    let Some(target) = quirk.hooks else {
        return ComponentHealth::not_applicable();
    };
    let mut found: Vec<(PathBuf, usize)> = Vec::new();
    let mut unreadable: Vec<PathBuf> = Vec::new();
    let mut absent: Vec<PathBuf> = Vec::new();
    for path in paths::hook_target_candidates(home, quirk) {
        let exists = if target.format == HookFormat::NativePlugin {
            path.is_dir()
        } else {
            path.is_file()
        };
        if !exists {
            continue;
        }
        // Truthful tri-state: unreadable (read/parse failure) vs missing
        // (readable config, no stateroot registration) vs ok. Malformed
        // config — JSON or TOML — is NEVER reported as "no registration".
        if target.format != HookFormat::NativePlugin {
            let Ok(text) = std::fs::read_to_string(&path) else {
                unreadable.push(path);
                continue;
            };
            let parseable = if target.format == HookFormat::TomlHooks {
                toml::from_str::<toml::Value>(&text).is_ok()
            } else {
                serde_json::from_str::<serde_json::Value>(&text).is_ok()
            };
            if !parseable {
                unreadable.push(path);
                continue;
            }
        }
        let commands = extract_hook_commands(&path, target.format);
        if commands.is_empty() {
            absent.push(path);
        } else {
            found.push((path, commands.len()));
        }
    }
    if let Some((path, count)) = found.first() {
        let duplicates = *count > quirk.event_map.len() && !quirk.event_map.is_empty();
        ComponentHealth::new(
            ComponentState::Ok,
            format!(
                "{} · {count} hook entr{}{}",
                path.display(),
                if *count == 1 { "y" } else { "ies" },
                if duplicates {
                    format!(
                        " · {} duplicate(s) beyond {} events",
                        count - quirk.event_map.len(),
                        quirk.event_map.len()
                    )
                } else {
                    String::new()
                },
            ),
        )
    } else if let Some(path) = unreadable.first() {
        ComponentHealth::new(
            ComponentState::Unreadable,
            format!("{} exists but is not readable/parseable", path.display()),
        )
    } else if let Some(path) = absent.first() {
        ComponentHealth::new(
            ComponentState::Missing,
            format!("{} has no stateroot hook registration", path.display()),
        )
    } else {
        ComponentHealth::new(ComponentState::Missing, "no hook registration")
    }
}

fn instructions_component(home: &Path, quirk: &HarnessQuirk) -> ComponentHealth {
    let mut surfaces: Vec<PathBuf> = paths::instruction_file_candidates(home, quirk);
    if let Some(rel) = registry::agent_file_target(quirk.id) {
        surfaces.push(registry::quirk_path(home, rel));
    }
    if surfaces.is_empty() {
        return ComponentHealth::not_applicable();
    }
    let mut block_found = false;
    let mut file_found: Option<PathBuf> = None;
    for path in &surfaces {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        file_found = Some(path.clone());
        if text.contains(BLOCK_BEGIN) || registry::agent_file_target(quirk.id).is_some() {
            block_found = true;
            break;
        }
    }
    if block_found {
        ComponentHealth::new(
            ComponentState::Ok,
            format!(
                "identity projection in {}",
                file_found.expect("file").display()
            ),
        )
    } else if let Some(path) = file_found {
        ComponentHealth::new(
            ComponentState::Missing,
            format!("{} exists without a stateroot block", path.display()),
        )
    } else {
        ComponentHealth::new(ComponentState::Missing, "no instruction projection")
    }
}

fn mcp_component(home: &Path, quirk: &HarnessQuirk) -> ComponentHealth {
    let Some(target) = quirk.mcp else {
        return ComponentHealth::not_applicable();
    };
    let path = paths::mcp_target_path(home, quirk).unwrap_or_else(|| home.join(target.path));
    if !path.is_file() {
        return ComponentHealth::new(ComponentState::Missing, "no MCP registration");
    }
    let root_key = match target.shape {
        super::registry::McpShape::ServersJson => "servers",
        _ => "mcpServers",
    };
    let servers = match target.shape {
        super::registry::McpShape::YamlMcpServers => super::read_named_yaml_mcp_servers(&path),
        _ => super::read_named_mcp_servers_json(&path, root_key),
    };
    match servers {
        Ok(map) if map.contains_key("stateroot") => ComponentHealth::new(
            ComponentState::Ok,
            format!("{root_key}.stateroot in {}", path.display()),
        ),
        Ok(_) => ComponentHealth::new(
            ComponentState::Missing,
            format!("{} has no stateroot server entry", path.display()),
        ),
        Err(err) => ComponentHealth::new(
            ComponentState::Unreadable,
            format!("{} unreadable ({err})", path.display()),
        ),
    }
}

fn skills_component(home: &Path, quirk: &HarnessQuirk) -> ComponentHealth {
    let Ok(contract) = crate::skill_federation::load_registry() else {
        return ComponentHealth::new(ComponentState::NotApplicable, "n/a");
    };
    let Some(entry) = contract.harnesses.iter().find(|e| e.id == quirk.id) else {
        return ComponentHealth::not_applicable();
    };
    if entry.projection_roots.global.is_empty() {
        return ComponentHealth::not_applicable();
    }
    for rel in &entry.projection_roots.global {
        let dir = home.join(rel).join("stateroot");
        if dir.join("SKILL.md").is_file() {
            return ComponentHealth::new(
                ComponentState::Ok,
                format!("{}/stateroot projected", dir.display()),
            );
        }
    }
    ComponentHealth::new(
        ComponentState::Missing,
        format!(
            "no stateroot skill under {}",
            entry.projection_roots.global.join(", ")
        ),
    )
}

/// Whether a component counts toward the integration core: the harness has
/// that surface (not `NotApplicable`).
fn applies(component: &ComponentHealth) -> bool {
    component.state != ComponentState::NotApplicable
}

/// Compute whole-machine integration health.
///
/// `recorded` is the CLI config's `installed_harnesses` list — a harness
/// recorded there but with no surviving evidence classifies `unknown`.
/// Rows are emitted only for harnesses that are detected, recorded, or
/// still carry integration artifacts; untouched machines stay empty.
pub fn integration_health(
    home: &Path,
    project_dir: Option<&Path>,
    prober: &dyn Prober,
    recorded: &[String],
) -> IntegrationHealth {
    let detections: BTreeMap<String, detect::Detection> = detect::detect_harnesses(home, prober)
        .into_iter()
        .map(|d| (d.id.clone(), d))
        .collect();
    // Flow evidence: the delivery ledger and the durable observation store.
    // Both are read with their diagnosis channels — an unreadable store is
    // surfaced in `evidence_problems`, never silently treated as "no flow".
    let (deliveries, delivery_diagnosis) = match project_dir {
        Some(dir) => {
            let (map, diagnosis) = crate::digest_delivery::delivery_evidence(dir);
            (Some(map), diagnosis)
        }
        None => (None, None),
    };
    let captures = project_dir.map(crate::observations::capture_trail);

    // Store-level evidence problems (document-wide): delivery-ledger
    // unreadability and any observation-store diagnosis not attributable to
    // one harness's segment.
    let mut evidence_problems: Vec<String> = Vec::new();
    if let Some(diagnosis) = delivery_diagnosis {
        evidence_problems.push(format!("digest delivery ledger: {diagnosis}"));
    }
    if let Some(trail) = &captures {
        for diagnosed in &trail.diagnosed {
            if segment_harness(diagnosed).is_none() {
                evidence_problems.push(format!("capture evidence: {diagnosed}"));
            }
        }
    }

    let mut rows = Vec::new();
    for quirk in registry::adapters() {
        let empty = detect::Detection {
            id: quirk.id.to_string(),
            binary_found: false,
            config_dir: false,
            evidence: Vec::new(),
            tier: quirk.tier,
        };
        let detection = detections.get(quirk.id).unwrap_or(&empty);
        let is_recorded = recorded
            .iter()
            .any(|id| id == quirk.id || registry::quirk_any(id).is_some_and(|q| q.id == quirk.id));

        let binary = binary_component(detection);
        let config = config_component(detection);
        let hooks = hooks_component(home, quirk);
        let instructions = instructions_component(home, quirk);
        let mcp = mcp_component(home, quirk);
        let skills = skills_component(home, quirk);

        let components = [&hooks, &instructions, &mcp, &skills];
        let any_artifact = components.iter().any(|c| c.state == ComponentState::Ok);
        // Harness-specific artifacts (NOT the shared .agents/skills
        // projection, which says nothing about an absent harness) drive row
        // inclusion.
        let harness_artifact = [&hooks, &instructions, &mcp]
            .iter()
            .any(|c| c.state == ComponentState::Ok);
        // The integration core spans every applicable pillar surface:
        // hooks, instructions (persona/rules), MCP, and the skill
        // projection. A missing pillar makes the integration incomplete —
        // `missing`, never silently "configured".
        let core_components: Vec<&ComponentHealth> =
            components.into_iter().filter(|c| applies(c)).collect();
        // A harness this CLI cannot integrate at all (no hook/instruction/
        // mcp/skill surface) has no observable configuration state.
        let any_surface =
            !core_components.is_empty() || skills.state != ComponentState::NotApplicable;

        if !detection.installed() && !harness_artifact && !is_recorded {
            continue; // harness not on this machine — nothing to say
        }

        let policy = quirk.delivery();
        let mut problems: Vec<String> = Vec::new();
        let mut repair: Vec<String> = Vec::new();
        let push_repair = |repair: &mut Vec<String>, cmd: &str| {
            let cmd = cmd.to_string();
            if !repair.contains(&cmd) {
                repair.push(cmd);
            }
        };

        for (name, component) in [
            ("hooks", &hooks),
            ("instructions", &instructions),
            ("mcp", &mcp),
            ("skills", &skills),
        ] {
            match component.state {
                ComponentState::Missing if applies(component) => {
                    problems.push(format!("{name}: {}", component.detail));
                    push_repair(&mut repair, "stateroot install");
                }
                ComponentState::Unreadable => {
                    problems.push(format!("{name}: {}", component.detail));
                    push_repair(&mut repair, "stateroot install");
                }
                _ => {}
            }
        }
        if hooks.detail.contains("duplicate(s)") {
            problems.push(format!("hooks: {}", hooks.detail));
            push_repair(&mut repair, "stateroot install");
        }
        if detection.config_dir && !detection.binary_found {
            problems.push(format!("binary: {}", binary.detail));
        }
        if !any_surface {
            problems.push("no installable integration surface in this CLI version".to_string());
        }

        let last_delivery = deliveries.as_ref().and_then(|m| m.get(quirk.id).cloned());
        let last_capture = captures
            .as_ref()
            .and_then(|trail| trail.last_by_harness.get(quirk.id).cloned());
        let flow_observed = last_delivery.is_some() || last_capture.is_some();
        // This harness's own capture-evidence diagnoses attach to its row —
        // unreadable evidence never promotes, and it reads as diagnosed,
        // not absent.
        if let Some(trail) = &captures {
            for diagnosed in &trail.diagnosed {
                if segment_harness(diagnosed) == Some(quirk.id) {
                    problems.push(format!("capture evidence: {diagnosed}"));
                }
            }
        }

        let core_ok = any_artifact
            && core_components
                .iter()
                .all(|c| c.state == ComponentState::Ok);
        let status = if core_ok && flow_observed {
            IntegrationStatus::ObservedWorking
        } else if core_ok {
            IntegrationStatus::Configured
        } else if !any_surface {
            // Detected/recorded, but this CLI has no integration surface
            // for the harness — nothing truthful to claim.
            IntegrationStatus::Unknown
        } else if detection.installed() || any_artifact {
            // Something to integrate (or a partial integration) but a
            // required component is absent/broken.
            IntegrationStatus::Missing
        } else {
            // Recorded as installed but no surviving evidence.
            IntegrationStatus::Unknown
        };
        if status == IntegrationStatus::Missing && repair.is_empty() {
            push_repair(&mut repair, "stateroot install");
        }
        if status == IntegrationStatus::Unknown && is_recorded {
            push_repair(&mut repair, "stateroot install");
        }
        if status == IntegrationStatus::Configured {
            // Not a repair — the confirmation path for a fresh integration.
            repair.push(format!(
                "start a fresh {} session in this project",
                quirk.display
            ));
        }

        let degraded = (policy.tier == DeliveryTier::Degraded).then(|| policy.note.to_string());
        rows.push(HarnessHealth {
            harness: quirk.id.to_string(),
            display: quirk.display.to_string(),
            tier: match quirk.tier {
                registry::Tier::A => "A",
                registry::Tier::B => "B",
                registry::Tier::C => "C",
            }
            .to_string(),
            status,
            detected: detection.installed(),
            // Presence is detected/recorded evidence; the binary VERSION is
            // unknown here — it is only ever filled by a bounded `--version`
            // probe of the resolved binary, never parsed from prose.
            version: None,
            binary,
            config,
            hooks,
            instructions,
            mcp,
            skills,
            delivery_tier: policy.tier,
            delivery_note: policy.note.to_string(),
            degraded,
            last_delivery,
            last_capture,
            problems,
            repair,
        });
    }
    IntegrationHealth {
        schema_version: INTEGRATION_HEALTH_SCHEMA.to_string(),
        generated_at: crate::local_store::now_rfc3339(),
        harnesses: rows,
        evidence_problems,
        install: None,
    }
}

/// The harness a capture-evidence diagnosis belongs to, parsed from the
/// `spool/segments/<harness>__<session>.jsonl` file the diagnosis names.
/// Store-wide files (the legacy spool) return None — they are reported at
/// the document level instead.
fn segment_harness(diagnosed: &str) -> Option<&str> {
    diagnosed
        .strip_prefix("spool/segments/")?
        .split("__")
        .next()
        .filter(|name| !name.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    struct StubProber {
        present: HashSet<String>,
    }

    impl StubProber {
        fn with(cmds: &[&str]) -> Self {
            Self {
                present: cmds.iter().map(|c| c.to_string()).collect(),
            }
        }
    }

    impl Prober for StubProber {
        fn probe(&self, cmd: &str) -> bool {
            self.present.contains(cmd)
        }
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, body).expect("write");
    }

    fn wire_cursor(home: &Path) {
        write(
            &home.join(".cursor/hooks.json"),
            &serde_json::to_string_pretty(&serde_json::json!({
                "version": 1,
                "hooks": {
                    "sessionStart": [{"type": "command", "command": "stateroot hook session_start --harness cursor", "matcher": ""}],
                    "stop": [{"type": "command", "command": "stateroot hook stop --harness cursor", "matcher": ""}]
                }
            }))
            .expect("json"),
        );
        write(
            &home.join(".cursor/mcp.json"),
            &serde_json::to_string_pretty(&serde_json::json!({
                "mcpServers": { "stateroot": { "command": "stateroot", "args": ["mcp-stdio"] } }
            }))
            .expect("json"),
        );
        write(
            &home.join(".cursor/AGENTS.md"),
            "<!-- stateroot:begin -->\nblock\n<!-- stateroot:end -->\n",
        );
        write(
            &home.join(".agents/skills/stateroot/SKILL.md"),
            "# stateroot\n",
        );
    }

    #[test]
    fn wired_without_flow_is_configured_never_observed_working() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        wire_cursor(home.path());
        let prober = StubProber::with(&["cursor"]);
        let health = integration_health(home.path(), Some(project.path()), &prober, &[]);
        let row = health
            .harnesses
            .iter()
            .find(|h| h.harness == "cursor")
            .expect("cursor row");
        assert_eq!(row.status, IntegrationStatus::Configured);
        assert!(row.hooks.state.ok(), "{:?}", row.hooks);
        assert!(row.mcp.state.ok(), "{:?}", row.mcp);
        assert!(row.instructions.state.ok(), "{:?}", row.instructions);
        assert!(row.skills.state.ok(), "{:?}", row.skills);
        assert!(row.last_delivery.is_none());
        assert!(row.last_capture.is_none());
        assert!(
            row.repair
                .iter()
                .any(|r| r.contains("fresh Cursor session")),
            "{:?}",
            row.repair
        );
        // Schema is the published seam.
        let value = serde_json::to_value(&health).expect("serialize");
        assert_eq!(value["schema_version"], INTEGRATION_HEALTH_SCHEMA);
        assert_eq!(value["harnesses"][0]["status"], "configured");
    }

    #[test]
    fn delivery_or_capture_promotes_to_observed_working() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        crate::local_store::init_skeleton(project.path(), "local-test", "test", "local")
            .expect("skeleton");
        wire_cursor(home.path());
        let prober = StubProber::with(&["cursor"]);
        crate::digest_delivery::mark_delivered(
            project.path(),
            "cursor",
            crate::digest_delivery::DeliveryIntent::Session,
            crate::digest_delivery::DeliveryChannel::Hook,
            "session_start",
            &serde_json::json!({}),
            "fp",
        );
        let health = integration_health(home.path(), Some(project.path()), &prober, &[]);
        let row = health
            .harnesses
            .iter()
            .find(|h| h.harness == "cursor")
            .expect("cursor row");
        assert_eq!(row.status, IntegrationStatus::ObservedWorking);
        assert!(row.last_delivery.is_some());
    }

    #[test]
    fn detected_without_artifacts_is_missing_with_install_repair() {
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".cursor")).expect("marker");
        let prober = StubProber::with(&["cursor"]);
        let health = integration_health(home.path(), None, &prober, &[]);
        let row = health
            .harnesses
            .iter()
            .find(|h| h.harness == "cursor")
            .expect("cursor row");
        assert_eq!(row.status, IntegrationStatus::Missing);
        assert!(row.repair.iter().any(|r| r == "stateroot install"));
        assert!(!row.problems.is_empty());
    }

    #[test]
    fn undetected_unrecorded_harness_has_no_row() {
        let home = tempfile::tempdir().expect("home");
        let prober = StubProber::with(&[]);
        let health = integration_health(home.path(), None, &prober, &[]);
        assert!(health.harnesses.is_empty(), "{:?}", health.harnesses);
        assert_eq!(health.summary_line(), "no harness integrations detected");
    }

    #[test]
    fn recorded_but_vanished_is_unknown() {
        let home = tempfile::tempdir().expect("home");
        let prober = StubProber::with(&[]);
        let health = integration_health(home.path(), None, &prober, &["cursor".to_string()]);
        let row = health
            .harnesses
            .iter()
            .find(|h| h.harness == "cursor")
            .expect("cursor row");
        assert_eq!(row.status, IntegrationStatus::Unknown);
        assert!(row.repair.iter().any(|r| r == "stateroot install"));
    }

    #[test]
    fn unreadable_hook_config_is_specific() {
        let home = tempfile::tempdir().expect("home");
        write(&home.path().join(".cursor/hooks.json"), "not json {");
        std::fs::create_dir_all(home.path().join(".cursor")).expect("marker");
        let prober = StubProber::with(&["cursor"]);
        let health = integration_health(home.path(), None, &prober, &[]);
        let row = health
            .harnesses
            .iter()
            .find(|h| h.harness == "cursor")
            .expect("cursor row");
        assert_eq!(row.hooks.state, ComponentState::Unreadable);
        assert_eq!(row.status, IntegrationStatus::Missing);
    }

    #[test]
    fn parseable_config_without_registration_is_missing_not_unreadable() {
        let home = tempfile::tempdir().expect("home");
        // Valid JSON, foreign hooks only — readable but no registration.
        write(
            &home.path().join(".cursor/hooks.json"),
            &serde_json::to_string_pretty(&serde_json::json!({
                "version": 1,
                "hooks": { "sessionStart": [{"type": "command", "command": "eslint --fix ."}] }
            }))
            .expect("json"),
        );
        std::fs::create_dir_all(home.path().join(".cursor")).expect("marker");
        let prober = StubProber::with(&["cursor"]);
        let health = integration_health(home.path(), None, &prober, &[]);
        let row = health
            .harnesses
            .iter()
            .find(|h| h.harness == "cursor")
            .expect("cursor row");
        assert_eq!(row.hooks.state, ComponentState::Missing, "{:?}", row.hooks);
        assert!(row.hooks.detail.contains("no stateroot hook registration"));
    }

    #[test]
    fn duplicate_hook_blocks_are_a_problem_not_a_status() {
        let home = tempfile::tempdir().expect("home");
        wire_cursor(home.path());
        // kimi-code style TomlHooks with a duplicate pile: more hook entries
        // than registered events.
        let toml = (0..12)
            .map(|_| {
                "[[hooks]]\ncommand = \"stateroot hook session_start --harness kimi-code\"\nevent = \"SessionStart\"\n"
            })
            .collect::<String>();
        write(&home.path().join(".kimi-code/config.toml"), &toml);
        write(
            &home.path().join(".kimi-code/mcp.json"),
            &serde_json::to_string_pretty(&serde_json::json!({
                "mcpServers": { "stateroot": { "command": "stateroot", "args": ["mcp-stdio"] } }
            }))
            .expect("json"),
        );
        write(
            &home.path().join(".kimi-code/AGENTS.md"),
            "<!-- stateroot:begin -->\nblock\n<!-- stateroot:end -->\n",
        );
        let prober = StubProber::with(&["kimi"]);
        let health = integration_health(home.path(), None, &prober, &[]);
        let row = health
            .harnesses
            .iter()
            .find(|h| h.harness == "kimi-code")
            .expect("kimi-code row");
        assert!(row.hooks.state.ok(), "{:?}", row.hooks);
        assert!(
            row.problems.iter().any(|p| p.contains("duplicate")),
            "{:?}",
            row.problems
        );
        assert_eq!(row.status, IntegrationStatus::Configured);
    }

    #[test]
    fn degraded_tier_carries_the_note() {
        let home = tempfile::tempdir().expect("home");
        // hermes: MCP-only, degraded delivery.
        write(
            &home.path().join(".hermes/config.yaml"),
            "mcp_servers:\n  stateroot:\n    command: stateroot\n    args: [mcp-stdio]\n",
        );
        let prober = StubProber::with(&["hermes"]);
        let health = integration_health(home.path(), None, &prober, &[]);
        let row = health
            .harnesses
            .iter()
            .find(|h| h.harness == "hermes")
            .expect("hermes row");
        assert_eq!(row.delivery_tier, DeliveryTier::Degraded);
        assert!(row.degraded.is_some());
        assert!(row.mcp.state.ok(), "{:?}", row.mcp);
    }

    #[test]
    fn summary_counts_group_rows() {
        let health = IntegrationHealth {
            schema_version: INTEGRATION_HEALTH_SCHEMA.into(),
            generated_at: "2026-10-09T00:00:00Z".into(),
            harnesses: vec![],
            evidence_problems: vec![],
            install: None,
        };
        assert_eq!(health.counts(), (0, 0, 0, 0));
    }

    #[test]
    fn authored_episodic_checkpoints_never_promote_observed_working() {
        // C1 truth contract: an authored checkpoint whose prose says
        // "via cursor hook" is NOT capture evidence — only the durable
        // observation store (a real hook capture) can promote.
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        crate::local_store::init_skeleton(project.path(), "local-test", "test", "local")
            .expect("skeleton");
        wire_cursor(home.path());
        let prober = StubProber::with(&["cursor"]);
        crate::local_store::append_episodic(
            project.path(),
            &serde_json::json!({
                "ts": "2026-10-09T00:00:00Z",
                "note": "session_end via cursor hook",
                "harness": "cursor",
            }),
        )
        .expect("episodic");
        let health = integration_health(home.path(), Some(project.path()), &prober, &[]);
        let row = health
            .harnesses
            .iter()
            .find(|h| h.harness == "cursor")
            .expect("cursor row");
        assert_eq!(
            row.status,
            IntegrationStatus::Configured,
            "authored prose must not promote: {row:?}"
        );
        assert!(row.last_capture.is_none());

        // A REAL durable hook capture promotes.
        crate::observations::capture(
            project.path(),
            crate::observations::CaptureRequest {
                ts: "2026-10-09T01:00:00Z".into(),
                event: "session_end".into(),
                harness: "cursor".into(),
                session_id: Some("s-1".into()),
                session_identity: "native".into(),
                event_identity: None,
                text: "captured payload".into(),
                kind_hint: None,
                tool: None,
                excerpt: None,
                source: None,
                source_status: "complete".into(),
                meta: None,
            },
        )
        .expect("capture");
        let health = integration_health(home.path(), Some(project.path()), &prober, &[]);
        let row = health
            .harnesses
            .iter()
            .find(|h| h.harness == "cursor")
            .expect("cursor row");
        assert_eq!(row.status, IntegrationStatus::ObservedWorking);
        assert_eq!(row.last_capture.as_deref(), Some("2026-10-09T01:00:00Z"));
    }

    #[test]
    fn corrupt_capture_evidence_is_diagnosed_not_absent() {
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        crate::local_store::init_skeleton(project.path(), "local-test", "test", "local")
            .expect("skeleton");
        wire_cursor(home.path());
        let prober = StubProber::with(&["cursor"]);
        // A torn segment line for cursor: the store exists and is damaged —
        // distinct from "no captures".
        let segments = crate::observations::segments_dir(project.path());
        std::fs::create_dir_all(&segments).expect("segments");
        std::fs::write(segments.join("cursor__s-9.jsonl"), "{torn\n").expect("write");
        let health = integration_health(home.path(), Some(project.path()), &prober, &[]);
        let row = health
            .harnesses
            .iter()
            .find(|h| h.harness == "cursor")
            .expect("cursor row");
        assert_eq!(row.status, IntegrationStatus::Configured);
        assert!(row.last_capture.is_none());
        assert!(
            row.problems
                .iter()
                .any(|p| p.contains("capture evidence") && p.contains("cursor__s-9.jsonl")),
            "{:?}",
            row.problems
        );
        // And a corrupt DELIVERY ledger is a document-level evidence problem.
        let ledger = project
            .path()
            .join(".stateroot/local/digest-delivery.v1.json");
        std::fs::create_dir_all(ledger.parent().expect("parent")).expect("mkdir");
        std::fs::write(&ledger, "{not json").expect("ledger");
        let health = integration_health(home.path(), Some(project.path()), &prober, &[]);
        assert!(
            health
                .evidence_problems
                .iter()
                .any(|p| p.contains("digest delivery ledger")),
            "{:?}",
            health.evidence_problems
        );
    }

    #[test]
    fn toml_hooks_decode_quoted_windows_exe_paths_with_spaces_and_unicode() {
        // The native-Windows shape that broke the line-based reader: the
        // TOML string is escaped, the decoded command is a quoted absolute
        // stateroot.exe path with spaces and Unicode.
        let dir = tempfile::tempdir().expect("dir");
        let config = dir.path().join("config.toml");
        let command = "\"C:\\Program Files\\StateRoot 工具\\stateroot.exe\" hook session_start --harness kimi-code";
        write(
            &config,
            &format!(
                "[model]\nname = \"k2\"\n\n[[hooks]]\nevent = \"SessionStart\"\ncommand = \"{}\"\n",
                command.replace('\\', "\\\\").replace('"', "\\\"")
            ),
        );
        let commands = extract_hook_commands(&config, HookFormat::TomlHooks);
        assert_eq!(commands, vec![command.to_string()]);
        let binary = binary_of_command(&commands[0]).expect("binary");
        assert_eq!(binary, "C:\\Program Files\\StateRoot 工具\\stateroot.exe");

        // Unquoted verbatim-prefix form (what the installer actually writes).
        let command2 = "\\\\?\\C:\\Users\\usama\\stateroot.exe hook stop --harness kimi-code";
        write(
            &config,
            &format!(
                "[[hooks]]\nevent = \"Stop\"\ncommand = \"{}\"\n",
                command2.replace('\\', "\\\\")
            ),
        );
        let commands = extract_hook_commands(&config, HookFormat::TomlHooks);
        assert_eq!(commands, vec![command2.to_string()]);
        assert_eq!(
            binary_of_command(&commands[0]).as_deref(),
            Some("\\\\?\\C:\\Users\\usama\\stateroot.exe")
        );

        // Malformed TOML yields no commands (the caller grades unreadable).
        write(&config, "[[hooks]\nbroken");
        assert!(extract_hook_commands(&config, HookFormat::TomlHooks).is_empty());
    }

    #[test]
    fn binary_of_command_rejects_non_stateroot_invocations() {
        assert_eq!(binary_of_command("stateroot").as_deref(), Some("stateroot"));
        assert_eq!(
            binary_of_command("stateroot hook stop --harness cursor").as_deref(),
            Some("stateroot")
        );
        // A wrapper or a path that merely MENTIONS stateroot is not ours.
        assert_eq!(binary_of_command("bash -c \"stateroot hook stop\""), None);
        assert_eq!(
            binary_of_command("/opt/notstateroot.exe hook stop --harness cursor"),
            None
        );
        assert_eq!(binary_of_command("echo stateroot hook stop"), None);
        // Spaces in the binary path are fine — the anchor is the LAST hook.
        assert_eq!(
            binary_of_command("/opt/my tools/stateroot hook stop --harness cursor").as_deref(),
            Some("/opt/my tools/stateroot")
        );
    }

    #[test]
    fn malformed_toml_hook_config_is_unreadable_not_missing() {
        let home = tempfile::tempdir().expect("home");
        write(
            &home.path().join(".kimi-code/config.toml"),
            "[[hooks]\nbroken",
        );
        std::fs::create_dir_all(home.path().join(".kimi-code")).expect("marker");
        let prober = StubProber::with(&["kimi"]);
        // Recorded as installed: the broken config is surviving evidence, so
        // the row exists and reads Unreadable — never "no registration".
        let health = integration_health(home.path(), None, &prober, &["kimi-code".to_string()]);
        let row = health
            .harnesses
            .iter()
            .find(|h| h.harness == "kimi-code")
            .expect("kimi-code row");
        assert_eq!(
            row.hooks.state,
            ComponentState::Unreadable,
            "{:?}",
            row.hooks
        );
    }
}
