//! Cross-harness skill federation — discovery, sync, projection.
//!
//! Consumes `stateroot_harness_registry.v1.json` (same contract as Python).
//! Portable packages materialize primarily under `.agents/skills` and
//! `.stateroot/skills`; closed-source built-ins are reference-only.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

// Compile the repository's authoritative contract directly. Do not keep a
// Rust-local mirror: duplicated JSON was the source of server/client drift.
const REGISTRY_JSON: &str = include_str!("../../contracts/stateroot_harness_registry.v1.json");
const PROJECTION_MARKER: &str = "<!-- stateroot:projection";
const MANAGED_MARKER: &str = "<!-- stateroot:managed-projection";
const PROJECTION_META: &str = ".stateroot-projection.json";
const PACKAGE_META: &str = "skill.federation.json";
/// R2.7 hashing guard: files larger than this are excluded from the package
/// digest (loss recorded in `hash_exclusions`).
const HASH_MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// R2.7 hashing guard: vendored/generated directories pruned from the walk.
const HASH_PRUNE_DIRS: [&str; 4] = ["node_modules", ".git", "__pycache__", ".venv"];
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryContract {
    pub schema_version: String,
    pub native_harness_id: String,
    /// Product-owned skill namespaces from the shared contract.
    #[serde(default)]
    pub product_skills: Vec<String>,
    pub harnesses: Vec<HarnessEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HarnessEntry {
    pub id: String,
    pub display: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub framing: String,
    #[serde(default)]
    pub tier: String,
    #[serde(default)]
    pub detect: Vec<String>,
    #[serde(default)]
    pub detect_cmds: Vec<String>,
    #[serde(default)]
    pub skill_source_roots: SkillRoots,
    #[serde(default)]
    pub builtin_policy: String,
    #[serde(default)]
    pub builtin_roots: Vec<String>,
    #[serde(default)]
    pub projection_roots: SkillRoots,
    #[serde(default)]
    pub mcp_config: McpConfigRoots,
    #[serde(default)]
    pub delegation: DelegationSpec,
    #[serde(default)]
    pub reload: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillRoots {
    #[serde(default)]
    pub global: Vec<String>,
    #[serde(default)]
    pub project: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct McpConfigRoots {
    #[serde(default)]
    pub global: Vec<McpConfigTarget>,
    #[serde(default)]
    pub project: Vec<McpConfigTarget>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpConfigTarget {
    pub path: String,
    /// `mcpServers` | `servers` | `mcp_servers`
    pub shape: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DelegationSpec {
    #[serde(default)]
    pub mode: String,
    pub command: Option<String>,
    #[serde(default)]
    pub pty: bool,
    #[serde(default)]
    pub print_mode: bool,
    #[serde(default)]
    pub handoff_only: bool,
    /// Argv template (args after the binary) with the `{prompt}` placeholder
    /// — moved into the contract per D3; both planners read it from here.
    /// Absent/empty → the default `["{prompt}"]` passthrough.
    #[serde(default)]
    pub argv: Vec<String>,
}

/// Default argv template when a registry row has none: bare prompt.
pub fn default_delegation_argv() -> Vec<String> {
    vec!["{prompt}".to_string()]
}

/// Build the launch argv for a harness from its registry delegation spec
/// (D3): `[command] + argv template with {prompt} substituted`.
pub fn build_argv_from_spec(spec: &DelegationSpec, prompt: &str) -> Option<Vec<String>> {
    let command = spec.command.clone()?;
    let template = if spec.argv.is_empty() {
        default_delegation_argv()
    } else {
        spec.argv.clone()
    };
    let mut argv = vec![command];
    argv.extend(template.iter().map(|arg| arg.replace("{prompt}", prompt)));
    Some(argv)
}

/// Binary probe used by detection-gating. `None` allowlist probes the host
/// PATH via `SystemProber`; `Some(list)` answers from the list (test seam).
pub(crate) fn binary_probe(allowlist: Option<&[String]>) -> impl Fn(&str) -> bool + '_ {
    move |cmd: &str| match allowlist {
        Some(list) => list.iter().any(|c| c == cmd),
        None => crate::harness_install::detect::Prober::probe(
            &crate::harness_install::detect::SystemProber,
            cmd,
        ),
    }
}

/// Detection gate (fix round R2 item 1): write configs/dirs ONLY for
/// harnesses detected on the machine — a `detect` marker under home, a
/// `detect_cmds` binary on PATH, or the target already existing on disk (it
/// was present once; updating/reclaiming it stays correct).
pub fn harness_detected(entry: &HarnessEntry, home: &Path, existing_target: Option<&Path>) -> bool {
    harness_detected_with(entry, home, existing_target, &binary_probe(None))
}

/// [`harness_detected`] with an injectable binary probe.
pub fn harness_detected_with(
    entry: &HarnessEntry,
    home: &Path,
    existing_target: Option<&Path>,
    probe: &dyn Fn(&str) -> bool,
) -> bool {
    if let Some(path) = existing_target {
        if path.exists() {
            return true;
        }
    }
    for marker in &entry.detect {
        let path = home.join(marker);
        if path.is_dir() || path.is_file() {
            return true;
        }
    }
    for cmd in &entry.detect_cmds {
        if probe(cmd) {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveredSkill {
    /// Stable portable identity independent of mutable slug/content.
    pub identity_key: String,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub harness: String,
    pub source_path: String,
    pub scope: String,
    pub ownership_class: String,
    pub lifecycle: String,
    /// Visibility gate (Wave-2 scope ladder): `shared` | `private`; empty on
    /// legacy packages — treated as shareable by the projection gate.
    #[serde(default)]
    pub visibility: String,
    pub package_digest: String,
    pub files: BTreeMap<String, String>,
    pub source_kind: String,
    #[serde(default)]
    pub license: Option<String>,
    /// Harness that can execute this capability natively. Portable packages
    /// may be consumed by any compatible harness; reference-only records use
    /// this route for explicit black-box delegation.
    #[serde(default)]
    pub native_harness: String,
    /// Human-readable native invocation route (never executed implicitly).
    #[serde(default)]
    pub native_invocation: String,
    /// Compatibility result for the current host. Incompatible shareable
    /// packages remain visible as honest delegation/reference wrappers.
    #[serde(default)]
    pub compatibility: Value,
    /// Paths skipped during whole-file hashing (size guard, R2.7) — vendored
    /// dirs like `node_modules` and files over the cap. Their bytes are NOT
    /// covered by `package_digest`; surfaced so the loss is visible.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hash_exclusions: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SyncOptions {
    pub dry_run: bool,
    pub push: bool,
    pub pull: bool,
    /// Test seam: when `Some`, binary detection (`detect_cmds`) is answered
    /// from this allowlist instead of probing the host PATH.
    #[doc(hidden)]
    pub cmd_probe: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncAction {
    pub action: String,
    pub slug: String,
    pub detail: String,
}

pub fn load_registry() -> Result<RegistryContract, String> {
    serde_json::from_str(REGISTRY_JSON).map_err(|e| format!("harness registry parse: {e}"))
}

pub fn normalize_harness(raw: &str) -> String {
    let key = raw.trim().to_ascii_lowercase();
    if key.is_empty() {
        return "skillsagent".to_string();
    }
    if let Ok(reg) = load_registry() {
        for entry in &reg.harnesses {
            if entry.id == key {
                return entry.id.clone();
            }
            for alias in &entry.aliases {
                if alias.eq_ignore_ascii_case(&key) {
                    return entry.id.clone();
                }
            }
        }
    }
    key
}

pub fn display_name(id: &str) -> String {
    let canon = normalize_harness(id);
    if let Ok(reg) = load_registry() {
        if let Some(entry) = reg.harnesses.iter().find(|e| e.id == canon) {
            return entry.display.clone();
        }
    }
    if canon == "skillsagent" {
        return "StateSmith".to_string();
    }
    canon
}

fn home_dir() -> Result<PathBuf, String> {
    crate::harness_install::home_dir()
        .map_err(|err| format!("could not resolve user home for skill discovery: {err}"))
}

fn expand_home_relative(rel: &str, home: &Path) -> PathBuf {
    if rel.starts_with('/') {
        PathBuf::from(rel)
    } else {
        home.join(rel)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn package_digest(files: &BTreeMap<String, String>) -> String {
    let mut canonical = String::new();
    for (path, digest) in files {
        canonical.push_str(path);
        canonical.push('\0');
        canonical.push_str(digest);
        canonical.push('\n');
    }
    sha256_hex(canonical.as_bytes())
}

/// Strip a single leading `<!-- … -->` comment block (server provenance
/// headers) so frontmatter parsing still sees the opening `---`.
fn strip_leading_html_comment(skill_md: &str) -> &str {
    let trimmed = skill_md.trim_start();
    if let Some(rest) = trimmed.strip_prefix("<!--") {
        if let Some(end) = rest.find("-->") {
            return rest[end + 3..].trim_start();
        }
    }
    skill_md
}

fn parse_frontmatter(skill_md: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if !skill_md.starts_with("---") {
        return out;
    }
    let Some(end) = skill_md[3..].find("\n---") else {
        return out;
    };
    let block = &skill_md[3..3 + end];
    for line in block.lines() {
        if let Some((k, v)) = line.split_once(':') {
            out.insert(
                k.trim().to_ascii_lowercase(),
                v.trim().trim_matches('"').trim_matches('\'').to_string(),
            );
        }
    }
    out
}

fn parse_frontmatter_document(skill_md: &str) -> Option<Value> {
    if !skill_md.starts_with("---") {
        return None;
    }
    let end = skill_md[3..].find("\n---")?;
    let block = &skill_md[3..3 + end];
    let yaml: serde_yaml::Value = serde_yaml::from_str(block).ok()?;
    serde_json::to_value(yaml).ok()
}

fn slugify(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "skill".to_string()
    } else {
        trimmed.chars().take(64).collect()
    }
}

fn is_projection_loop(skill_dir: &Path, skill_md: &str) -> bool {
    if skill_dir.join(PROJECTION_META).is_file() {
        return true;
    }
    let head = if skill_md.len() > 400 {
        &skill_md[..400]
    } else {
        skill_md
    };
    head.contains(PROJECTION_MARKER) || head.contains(MANAGED_MARKER)
}

fn product_skill_slugs() -> Vec<String> {
    match load_registry() {
        Ok(reg) if !reg.product_skills.is_empty() => reg.product_skills,
        _ => vec!["stateroot".into(), "stateroot-skill-router".into()],
    }
}

pub fn is_product_owned_slug(slug: &str) -> bool {
    product_skill_slugs().iter().any(|s| s == slug)
}

fn product_identity_key(slug: &str) -> String {
    let digest = sha256_hex(format!("product\0{slug}").as_bytes());
    format!("psi_{}", &digest[..32])
}

/// Detect the first-party StateRoot skill even when discovered from a harness
/// install path that was not yet marked as a managed projection.
fn is_product_stateroot_fingerprint(skill_dir: &Path, fm: &BTreeMap<String, String>) -> bool {
    if fm.get("name").map(String::as_str) != Some("stateroot") {
        return false;
    }
    if let Some(manifest) = read_json_file(&skill_dir.join("skill.manifest.json")) {
        if manifest.get("install_strategy").and_then(Value::as_str) == Some("first_party_bootstrap")
        {
            return true;
        }
        if manifest
            .get("required_binaries")
            .and_then(Value::as_array)
            .is_some_and(|bins| bins.iter().any(|v| v.as_str() == Some("stateroot")))
        {
            return true;
        }
    }
    fm.get("description")
        .is_some_and(|d| d.contains("`stateroot`") || d.contains("stateroot CLI"))
}

fn scan_skill_dir(
    skill_dir: &Path,
    harness: &str,
    scope: &str,
    source_kind: &str,
) -> Option<DiscoveredSkill> {
    let skill_md_path = if skill_dir.join("SKILL.md").is_file() {
        skill_dir.join("SKILL.md")
    } else if skill_dir.join("skill.md").is_file() {
        skill_dir.join("skill.md")
    } else {
        return None;
    };
    let skill_md = fs::read_to_string(&skill_md_path).ok()?;
    if is_projection_loop(skill_dir, &skill_md) {
        return None;
    }
    // Server-written portable skills carry a leading provenance comment
    // (`<!-- stateroot:skill origin=… -->`) before the frontmatter — strip it
    // or name/description never parse.
    let parse_src = strip_leading_html_comment(&skill_md);
    let mut files = BTreeMap::new();
    let mut hash_exclusions = Vec::new();
    let mut pruned_dirs = Vec::new();
    let walker = walkdir_shallow(skill_dir, 8, &mut pruned_dirs);
    for rel_dir in &pruned_dirs {
        let rel_s = rel_dir
            .strip_prefix(skill_dir)
            .map(|r| r.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| rel_dir.display().to_string());
        hash_exclusions.push(format!("{rel_s}/ (vendored dir not hashed)"));
    }
    for path in walker {
        if !path.is_file() {
            continue;
        }
        let Ok(rel) = path.strip_prefix(skill_dir) else {
            continue;
        };
        let rel_s = rel.to_string_lossy().replace('\\', "/");
        if rel_s.starts_with(".git/")
            || rel_s.ends_with(".pyc")
            || rel_s == PROJECTION_META
            || rel_s == PACKAGE_META
        {
            continue;
        }
        // Size guard (R2.7): never slurp huge vendored/generated blobs into
        // memory for hashing; record the loss instead.
        let len = path.metadata().map(|m| m.len()).unwrap_or(0);
        if len > HASH_MAX_FILE_BYTES {
            hash_exclusions.push(format!("{rel_s} ({len} bytes > cap)"));
            continue;
        }
        if let Ok(bytes) = fs::read(&path) {
            files.insert(rel_s, sha256_hex(&bytes));
        }
    }
    hash_exclusions.sort();
    if !hash_exclusions.is_empty() {
        tracing::warn!(
            "skill {}: {} path(s) excluded from hashing (loss noted in hash_exclusions)",
            skill_dir.display(),
            hash_exclusions.len()
        );
    }
    let digest = package_digest(&files);
    let fm = parse_frontmatter(parse_src);
    let frontmatter_doc = parse_frontmatter_document(parse_src);
    let slug = slugify(fm.get("name").map(String::as_str).unwrap_or_else(|| {
        skill_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("skill")
    }));
    let name = fm.get("name").cloned().unwrap_or_else(|| slug.clone());
    let description = fm.get("description").cloned().unwrap_or_default();
    let package_meta = read_json_file(&skill_dir.join(PACKAGE_META));
    let (default_ownership, default_lifecycle) = match source_kind {
        "builtin_reference" => ("closed_builtin", "reference_only"),
        "open_source_bundled" => ("open_source", "active"),
        "plugin" => ("harness_authored", "active"),
        _ => ("user_installed", "active"),
    };
    let is_product = is_product_owned_slug(&slug)
        || (slug == "stateroot" && is_product_stateroot_fingerprint(skill_dir, &fm));
    // Product namespaces always win over stale package meta.
    let ownership = if is_product {
        "statesmith_authored".to_string()
    } else {
        package_meta
            .as_ref()
            .and_then(|meta| meta.get("ownership_class"))
            .and_then(Value::as_str)
            .unwrap_or(default_ownership)
            .to_string()
    };
    let lifecycle = package_meta
        .as_ref()
        .and_then(|meta| meta.get("lifecycle"))
        .and_then(Value::as_str)
        .unwrap_or(default_lifecycle)
        .to_string();
    let visibility = package_meta
        .as_ref()
        .and_then(|meta| meta.get("visibility"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let origin_harness = package_meta
        .as_ref()
        .and_then(|meta| meta.pointer("/origin/harness"))
        .and_then(Value::as_str)
        .unwrap_or(harness);
    let native_harness = package_meta
        .as_ref()
        .and_then(|meta| meta.get("native_harness"))
        .and_then(Value::as_str)
        .map(normalize_harness)
        .unwrap_or_else(|| {
            if is_product {
                "skillsagent".to_string()
            } else {
                normalize_harness(origin_harness)
            }
        });
    let native_invocation = package_meta
        .as_ref()
        .and_then(|meta| meta.get("native_invocation"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!("stateroot harness run {native_harness} --skill {slug} --objective <microtask>")
        });
    let identity_key = if is_product {
        product_identity_key(&slug)
    } else {
        let identity_key = package_meta
            .as_ref()
            .and_then(|meta| meta.get("identity_key"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                sha256_hex(
                    format!(
                        "{}\0{}\0{}",
                        normalize_harness(origin_harness),
                        scope,
                        skill_dir.display()
                    )
                    .as_bytes(),
                )[..32]
                    .to_string()
            });
        if identity_key.starts_with("psi_") {
            identity_key
        } else {
            format!("psi_{identity_key}")
        }
    };
    Some(DiscoveredSkill {
        identity_key,
        slug,
        name,
        description,
        harness: normalize_harness(origin_harness),
        source_path: skill_dir.display().to_string(),
        scope: scope.to_string(),
        ownership_class: ownership,
        lifecycle,
        visibility,
        package_digest: digest,
        files,
        source_kind: source_kind.to_string(),
        license: fm.get("license").cloned(),
        native_harness,
        native_invocation,
        compatibility: serde_json::json!({
            "compatible": true,
            "reasons": [],
            "requirements": frontmatter_doc
                .as_ref()
                .and_then(|doc| doc.get("metadata"))
                .cloned()
                .unwrap_or(Value::Null),
        }),
        hash_exclusions,
    })
}

fn walkdir_shallow(root: &Path, max_depth: usize, pruned: &mut Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    fn walk(
        dir: &Path,
        depth: usize,
        max_depth: usize,
        out: &mut Vec<PathBuf>,
        pruned: &mut Vec<PathBuf>,
    ) {
        if depth > max_depth {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth > 0
                    && path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| HASH_PRUNE_DIRS.contains(&n))
                        .unwrap_or(false)
                {
                    pruned.push(path);
                    continue;
                }
                walk(&path, depth + 1, max_depth, out, pruned);
            } else {
                out.push(path);
            }
        }
    }
    walk(root, 0, max_depth, &mut out, pruned);
    out
}

fn scan_tree(
    root: &Path,
    harness: &str,
    scope: &str,
    source_kind: &str,
    out: &mut Vec<DiscoveredSkill>,
) {
    if !root.exists() {
        return;
    }
    if root.join("SKILL.md").is_file() || root.join("skill.md").is_file() {
        if let Some(skill) = scan_skill_dir(root, harness, scope, source_kind) {
            out.push(skill);
        }
        return;
    }
    fn walk(
        dir: &Path,
        depth: usize,
        harness: &str,
        scope: &str,
        source_kind: &str,
        out: &mut Vec<DiscoveredSkill>,
    ) {
        if depth > 6 {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            // Hidden container directories (notably Codex `.system`) are
            // scanned only when explicitly registered as a built-in root.
            if name.starts_with('.') {
                continue;
            }
            if path.join("SKILL.md").is_file() || path.join("skill.md").is_file() {
                if let Some(skill) = scan_skill_dir(&path, harness, scope, source_kind) {
                    out.push(skill);
                }
                continue;
            }
            walk(&path, depth + 1, harness, scope, source_kind, out);
        }
    }
    walk(root, 0, harness, scope, source_kind, out);
}

fn read_json_file(path: &Path) -> Option<Value> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn push_unique_root(roots: &mut Vec<PathBuf>, seen: &mut BTreeSet<String>, path: PathBuf) {
    if !path.is_dir() {
        return;
    }
    let resolved = path.canonicalize().unwrap_or(path);
    let key = resolved.to_string_lossy().to_ascii_lowercase();
    if seen.insert(key) {
        roots.push(resolved);
    }
}

fn openclaw_bundled_skill_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut seen = BTreeSet::new();

    if let Some(raw) = env::var_os("OPENCLAW_BUNDLED_SKILLS_DIR") {
        push_unique_root(
            &mut roots,
            &mut seen,
            crate::openclaw_identity::expand_user_path(&raw.to_string_lossy(), home),
        );
    }

    // npm/nvm shims live beside `node_modules/openclaw`. Deriving from PATH
    // avoids hard-coding an nvm version or AppData layout on Windows.
    if let Some(path_var) = env::var_os("PATH") {
        for dir in env::split_paths(&path_var) {
            push_unique_root(
                &mut roots,
                &mut seen,
                dir.join("node_modules").join("openclaw").join("skills"),
            );
            push_unique_root(
                &mut roots,
                &mut seen,
                dir.join("..")
                    .join("lib")
                    .join("node_modules")
                    .join("openclaw")
                    .join("skills"),
            );
        }
    }

    for var in ["NVM_SYMLINK", "NVM_HOME", "APPDATA", "LOCALAPPDATA"] {
        let Some(raw) = env::var_os(var) else {
            continue;
        };
        let base = PathBuf::from(raw);
        push_unique_root(
            &mut roots,
            &mut seen,
            base.join("node_modules").join("openclaw").join("skills"),
        );
        push_unique_root(
            &mut roots,
            &mut seen,
            base.join("npm")
                .join("node_modules")
                .join("openclaw")
                .join("skills"),
        );
    }
    roots
}

fn openclaw_config(home: &Path) -> Option<Value> {
    read_json_file(&crate::openclaw_identity::openclaw_config_path(home))
}

/// True when a command resolves in the current local execution environment.
pub fn command_available(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() {
        return false;
    }
    let candidate = Path::new(command);
    if candidate.components().count() > 1 {
        return candidate.is_file();
    }
    let extensions: Vec<String> = if cfg!(windows) {
        env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD;.PS1".into())
            .split(';')
            .map(|value| value.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    let Some(path_var) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path_var).any(|dir| {
        if dir.join(command).is_file() {
            return true;
        }
        extensions.iter().any(|extension| {
            if extension.is_empty() {
                false
            } else {
                dir.join(format!("{command}{extension}")).is_file()
                    || dir
                        .join(format!("{command}{}", extension.to_ascii_uppercase()))
                        .is_file()
            }
        })
    })
}

fn config_value<'a>(config: &'a Value, dotted_path: &str) -> Option<&'a Value> {
    let mut current = config;
    for segment in dotted_path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

fn value_is_enabled(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::Null) | None => false,
        Some(Value::String(value)) => !value.trim().is_empty(),
        Some(Value::Array(value)) => !value.is_empty(),
        Some(Value::Object(value)) => !value.is_empty(),
        Some(Value::Number(_)) => true,
    }
}

fn evaluate_openclaw_compatibility(skill: &mut DiscoveredSkill, config: Option<&Value>) {
    let metadata = skill
        .compatibility
        .pointer("/requirements/openclaw")
        .cloned()
        .unwrap_or(Value::Null);
    if metadata.is_null() {
        skill.compatibility = serde_json::json!({"compatible": true, "reasons": []});
        return;
    }
    let mut reasons = Vec::new();
    let host_os = env::var("STATEROOT_TEST_OS").unwrap_or_else(|_| match env::consts::OS {
        "macos" => "darwin".into(),
        other => other.into(),
    });
    if let Some(supported) = metadata.get("os").and_then(Value::as_array) {
        let supported: Vec<&str> = supported.iter().filter_map(Value::as_str).collect();
        if !supported.is_empty() && !supported.iter().any(|value| *value == host_os) {
            reasons.push(format!("requires OS {}", supported.join("|")));
        }
    }
    // Binaries an installer spec could provide (`install[].bins`), mapped to
    // the installer id — missing-but-installable requirements stay
    // incompatible (OpenClaw gates at load time) but say so honestly.
    let mut installable: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    if let Some(installers) = metadata.get("install").and_then(Value::as_array) {
        for spec in installers {
            let id = spec
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("installer");
            if let Some(bins) = spec.get("bins").and_then(Value::as_array) {
                for bin in bins.iter().filter_map(Value::as_str) {
                    installable
                        .entry(bin.to_string())
                        .or_insert_with(|| id.to_string());
                }
            }
        }
    }
    let requires = metadata.get("requires").cloned().unwrap_or(Value::Null);
    if let Some(bins) = requires.get("bins").and_then(Value::as_array) {
        for bin in bins.iter().filter_map(Value::as_str) {
            if !command_available(bin) {
                reasons.push(match installable.get(bin) {
                    Some(id) => format!("missing binary `{bin}` (installable via `{id}`)"),
                    None => format!("missing binary `{bin}`"),
                });
            }
        }
    }
    if let Some(any_bins) = requires.get("anyBins").and_then(Value::as_array) {
        let bins: Vec<&str> = any_bins.iter().filter_map(Value::as_str).collect();
        if !bins.is_empty() && !bins.iter().any(|bin| command_available(bin)) {
            let installable_note = bins
                .iter()
                .find_map(|bin| installable.get(*bin).map(|id| format!("{bin}` via `{id}")))
                .map(|note| format!(" (`{note}`)"))
                .unwrap_or_default();
            reasons.push(format!(
                "requires any binary {}{installable_note}",
                bins.join("|")
            ));
        }
    }
    // Env requirements are satisfied by the process env OR by config:
    // OpenClaw injects `skills.entries.<name>.env` (and `apiKey` for the
    // skill's `primaryEnv`) into the agent run. Checking only the process env
    // would mark loadable skills `external_only` — dishonest.
    let primary_env = metadata.get("primaryEnv").and_then(Value::as_str);
    let entry_env =
        config.and_then(|body| body.pointer(&format!("/skills/entries/{}/env", skill.slug)));
    let api_key_configured = config
        .map(|body| {
            value_is_enabled(config_value(
                body,
                &format!("skills.entries.{}.apiKey", skill.slug),
            ))
        })
        .unwrap_or(false);
    let env_satisfied = |var: &str| -> bool {
        if env::var_os(var).is_some() {
            return true;
        }
        if entry_env
            .and_then(|entries| entries.get(var))
            .map(|value| value_is_enabled(Some(value)))
            .unwrap_or(false)
        {
            return true;
        }
        if Some(var) == primary_env && api_key_configured {
            return true;
        }
        false
    };
    if let Some(primary) = primary_env {
        if !env_satisfied(primary) {
            reasons.push(format!(
                "missing environment variable `{primary}` (or set skills.entries.{}.apiKey)",
                skill.slug
            ));
        }
    }
    if let Some(vars) = requires.get("env").and_then(Value::as_array) {
        for var in vars.iter().filter_map(Value::as_str) {
            if Some(var) == primary_env {
                continue; // already evaluated with the apiKey hint above
            }
            if !env_satisfied(var) {
                reasons.push(format!("missing environment variable `{var}`"));
            }
        }
    }
    if let Some(paths) = requires.get("config").and_then(Value::as_array) {
        for path in paths.iter().filter_map(Value::as_str) {
            if !config
                .map(|body| value_is_enabled(config_value(body, path)))
                .unwrap_or(false)
            {
                reasons.push(format!("missing enabled config `{path}`"));
            }
        }
    }
    let compatible = reasons.is_empty();
    if !compatible && skill.lifecycle == "active" {
        skill.lifecycle = "external_only".into();
    }
    skill.compatibility = serde_json::json!({
        "compatible": compatible,
        "reasons": reasons,
        "requirements": metadata,
        "host_os": host_os,
    });
}

fn openclaw_skill_enabled(config: Option<&Value>, skill: &DiscoveredSkill) -> bool {
    let Some(config) = config else {
        return true;
    };
    if config
        .pointer(&format!("/skills/entries/{}/enabled", skill.slug))
        .and_then(Value::as_bool)
        == Some(false)
    {
        return false;
    }
    if skill.source_kind == "open_source_bundled" {
        if let Some(allow) = config
            .pointer("/skills/allowBundled")
            .and_then(Value::as_array)
        {
            return allow
                .iter()
                .filter_map(Value::as_str)
                .any(|name| name == skill.slug);
        }
    }
    true
}

fn discover_openclaw_skills(home: &Path, out: &mut Vec<DiscoveredSkill>) {
    let state = crate::openclaw_identity::openclaw_state_dir(home);
    let config = openclaw_config(home);

    let mut discovered = Vec::new();
    scan_tree(
        &state.join("skills"),
        "openclaw",
        "global",
        "managed",
        &mut discovered,
    );
    // OpenClaw publishes only enabled plugin skill links into this directory.
    scan_tree(
        &state.join("plugin-skills"),
        "openclaw",
        "global",
        "plugin",
        &mut discovered,
    );
    for workspace in crate::openclaw_identity::discover_openclaw_workspace_dirs(home) {
        scan_tree(
            &workspace.join("skills"),
            "openclaw",
            "global",
            "workspace",
            &mut discovered,
        );
    }
    if let Some(config) = config.as_ref() {
        if let Some(extra) = config
            .pointer("/skills/load/extraDirs")
            .and_then(Value::as_array)
        {
            for raw in extra.iter().filter_map(Value::as_str) {
                scan_tree(
                    &crate::openclaw_identity::expand_user_path(raw, home),
                    "openclaw",
                    "global",
                    "extra",
                    &mut discovered,
                );
            }
        }
    }
    for root in openclaw_bundled_skill_roots(home) {
        scan_tree(
            &root,
            "openclaw",
            "global",
            "open_source_bundled",
            &mut discovered,
        );
    }
    out.extend(discovered.into_iter().filter_map(|mut skill| {
        if !openclaw_skill_enabled(config.as_ref(), &skill) {
            return None;
        }
        evaluate_openclaw_compatibility(&mut skill, config.as_ref());
        Some(skill)
    }));
}

fn cursor_active_builtin_names(home: &Path) -> Option<BTreeSet<String>> {
    let manifest = read_json_file(&home.join(".cursor/skills-cursor/.sync-manifest.json"))?;
    let skills = manifest.get("skills")?.as_object()?;
    Some(skills.keys().cloned().collect())
}

fn is_shared_federation_root(path: &str) -> bool {
    matches!(
        path.trim_matches('/').replace('\\', "/").as_str(),
        ".agents/skills" | ".stateroot/skills"
    )
}

/// Discover skills across every registered harness (shareable + reference-only).
pub fn discover_all(
    project_dir: &Path,
    home: Option<&Path>,
) -> Result<Vec<DiscoveredSkill>, String> {
    discover_with_scope(project_dir, home, true)
}

/// Discover only user-global skill roots. This is used by machine-global
/// setup, which must never turn its current directory into a project skill
/// projection by accident.
pub fn discover_global(home: &Path) -> Result<Vec<DiscoveredSkill>, String> {
    discover_with_scope(Path::new(""), Some(home), false)
}

fn discover_with_scope(
    project_dir: &Path,
    home: Option<&Path>,
    include_project_scope: bool,
) -> Result<Vec<DiscoveredSkill>, String> {
    let home = match home {
        Some(path) => path.to_path_buf(),
        None => home_dir()?,
    };
    let reg = load_registry()?;
    let mut found = Vec::new();
    for entry in &reg.harnesses {
        if entry.id == "openclaw" {
            discover_openclaw_skills(&home, &mut found);
            continue;
        }
        if entry.id == "skillsagent" || entry.id == "planner" {
            continue;
        }
        for rel in &entry.skill_source_roots.global {
            if is_shared_federation_root(rel) {
                continue;
            }
            let root = expand_home_relative(rel, &home);
            scan_tree(&root, &entry.id, "global", "package", &mut found);
        }
        if include_project_scope {
            for rel in &entry.skill_source_roots.project {
                if is_shared_federation_root(rel) {
                    continue;
                }
                let root = project_dir.join(rel);
                scan_tree(&root, &entry.id, "project", "package", &mut found);
            }
        }
        if entry.builtin_policy == "reference_only" {
            let cursor_active = (entry.id == "cursor")
                .then(|| cursor_active_builtin_names(&home))
                .flatten();
            for rel in &entry.builtin_roots {
                let root = expand_home_relative(rel, &home);
                let start = found.len();
                scan_tree(&root, &entry.id, "global", "builtin_reference", &mut found);
                if let Some(active) = cursor_active.as_ref() {
                    let mut suffix = found.split_off(start);
                    suffix.retain(|skill| active.contains(&skill.slug));
                    found.extend(suffix);
                }
            }
        }
    }
    // Portable canonical roots are origins of record, scanned exactly once.
    scan_tree(
        &home.join(".stateroot/skills"),
        "skillsagent",
        "global",
        "portable",
        &mut found,
    );
    if include_project_scope {
        scan_tree(
            &project_dir.join(".stateroot/skills"),
            "skillsagent",
            "project",
            "package",
            &mut found,
        );
    }
    found.sort_by(|a, b| {
        (&a.slug, &a.harness, &a.package_digest).cmp(&(&b.slug, &b.harness, &b.package_digest))
    });
    found.dedup_by(|a, b| {
        a.slug == b.slug && a.package_digest == b.package_digest && a.source_path == b.source_path
    });
    Ok(found)
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == PROJECTION_META || name == PACKAGE_META {
            continue;
        }
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

fn write_json(path: &Path, value: &Value) -> std::io::Result<()> {
    fs::write(
        path,
        serde_json::to_vec_pretty(value).unwrap_or_else(|_| b"{}".to_vec()),
    )
}

fn write_package_meta(skill_dir: &Path, skill: &DiscoveredSkill) -> std::io::Result<()> {
    // D2 union schema — both writers (this client and the server's
    // `skill_federation_service`) emit the same full field set.
    let mut meta = serde_json::json!({
        "schema_version": "stateroot.skill_package.v1",
        "identity_key": skill.identity_key,
        "slug": skill.slug,
        "scope": skill.scope,
        "package_digest": skill.package_digest,
        "origin": {
            "harness": skill.harness,
            "source_path": skill.source_path,
            "source_kind": skill.source_kind,
        },
        "ownership_class": skill.ownership_class,
        "license": skill.license,
        "lifecycle": skill.lifecycle,
        "compatibility": skill.compatibility,
        "native_harness": skill.native_harness,
        "native_invocation": skill.native_invocation,
    });
    // Wave-2 visibility gate: round-trip when set; absent means legacy
    // shareable, so never write an empty value.
    if !skill.visibility.is_empty() {
        meta["visibility"] = serde_json::json!(skill.visibility);
    }
    write_json(&skill_dir.join(PACKAGE_META), &meta)
}

fn projection_digest(path: &Path) -> Option<String> {
    read_json_file(&path.join(PROJECTION_META))?
        .get("package_digest")?
        .as_str()
        .map(str::to_string)
}

fn materialize_managed_projection(
    src: &Path,
    dst: &Path,
    skill: &DiscoveredSkill,
    projection_kind: &str,
    dry_run: bool,
) -> Result<&'static str, String> {
    let mut reclaiming_product = false;
    if dst.exists() {
        match projection_digest(dst) {
            Some(existing_digest) if existing_digest == skill.package_digest => {
                return Ok("unchanged");
            }
            Some(_) => {}
            None if is_product_owned_slug(&skill.slug)
                || skill.ownership_class == "statesmith_authored" =>
            {
                // Product skill: reclaim and refresh. Do not treat our own
                // installer output as a user-authored conflict.
                reclaiming_product = true;
            }
            None => return Ok("unmanaged_conflict"),
        }
    }
    if dry_run {
        return Ok(if reclaiming_product {
            "would_reclaim"
        } else if dst.exists() {
            "would_update"
        } else {
            "would_project"
        });
    }

    let parent = dst
        .parent()
        .ok_or_else(|| format!("projection has no parent: {}", dst.display()))?;
    fs::create_dir_all(parent)
        .map_err(|err| format!("create projection parent {}: {err}", parent.display()))?;
    let file_name = dst
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("skill");
    let tmp = parent.join(format!(".{file_name}.stateroot-tmp-{}", std::process::id()));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)
            .map_err(|err| format!("remove stale projection temp {}: {err}", tmp.display()))?;
    }
    copy_dir_recursive(src, &tmp)
        .map_err(|err| format!("copy projection {}: {err}", tmp.display()))?;
    write_json(
        &tmp.join(PROJECTION_META),
        &serde_json::json!({
            "schema_version": "stateroot.skill_projection.v1",
            "managed_by": "stateroot",
            "projection_kind": projection_kind,
            "identity_key": skill.identity_key,
            "slug": skill.slug,
            "package_digest": skill.package_digest,
            "source_harness": skill.harness,
            "native_harness": skill.native_harness,
            "native_invocation": skill.native_invocation,
            "ownership_class": if is_product_owned_slug(&skill.slug) {
                "statesmith_authored"
            } else {
                skill.ownership_class.as_str()
            },
        }),
    )
    .map_err(|err| format!("write projection metadata {}: {err}", tmp.display()))?;
    let action = if reclaiming_product {
        "reclaimed"
    } else if dst.exists() {
        "updated"
    } else {
        "projected"
    };
    if dst.exists() {
        fs::remove_dir_all(dst)
            .map_err(|err| format!("replace managed projection {}: {err}", dst.display()))?;
    }
    fs::rename(&tmp, dst).map_err(|err| format!("activate projection {}: {err}", dst.display()))?;
    Ok(action)
}

fn write_reference_wrapper(dst: &Path, skill: &DiscoveredSkill) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    let wrapper_name = format!("{}-via-{}", skill.slug, skill.native_harness);
    let reason = if skill.lifecycle == "reference_only" {
        "The implementation is closed/vendor-owned, remains inside the source harness, and is never copied."
            .to_string()
    } else {
        let details = skill
            .compatibility
            .get("reasons")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "requirements are unavailable on this host".into());
        format!("The package is portable but external-only on this host: {details}.")
    };
    let body = format!(
        "---\nname: {}\ndescription: \"Delegate an explicit microtask to the {} native capability {} through StateRoot.\"\n---\n\n# {} via {}\n\nThis is a **{} capability**. {}\n\n- Source harness: `{}`\n- Capability: `{}`\n- Source availability: `{}`\n- Digest: `{}`\n\n## Invocation\n\nOnly when the user explicitly asks to use another harness or this delegated capability, run:\n\n```text\n{}\n```\n\nPass a bounded objective, working directory, expected outputs, constraints, and tests. Inspect outputs rather than replaying the foreign transcript. If the source harness is GUI-only, StateRoot returns a structured handoff instead of pretending it launched an agent.\n",
        wrapper_name,
        display_name(&skill.native_harness),
        skill.slug,
        skill.name,
        display_name(&skill.native_harness),
        skill.lifecycle,
        reason,
        skill.harness,
        skill.slug,
        skill.source_path,
        skill.package_digest,
        skill.native_invocation,
    );
    fs::write(dst.join("SKILL.md"), body)?;
    write_json(
        &dst.join(PROJECTION_META),
        &serde_json::json!({
            "schema_version": "stateroot.skill_projection.v1",
            "managed_by": "stateroot",
            "projection_kind": "delegation_wrapper",
            "identity_key": skill.identity_key,
            "slug": skill.slug,
            "package_digest": skill.package_digest,
            "source_harness": skill.harness,
            "native_harness": skill.native_harness,
            "native_invocation": skill.native_invocation,
        }),
    )
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

fn existing_package_digest(path: &Path, scope: &str) -> Option<String> {
    scan_skill_dir(path, "skillsagent", scope, "portable").map(|skill| skill.package_digest)
}

fn canonical_destination(root: &Path, skill: &DiscoveredSkill) -> PathBuf {
    let preferred = root.join(&skill.slug);
    if !preferred.exists()
        || existing_package_digest(&preferred, &skill.scope).as_deref()
            == Some(skill.package_digest.as_str())
    {
        return preferred;
    }
    root.join(format!(
        "{}__{}",
        skill.slug,
        &skill.package_digest[..8.min(skill.package_digest.len())]
    ))
}

fn supersede_prior_versions(root: &Path, skill: &DiscoveredSkill) -> Result<(), String> {
    let Ok(entries) = fs::read_dir(root) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let meta_path = dir.join(PACKAGE_META);
        let Some(mut meta) = read_json_file(&meta_path) else {
            continue;
        };
        if meta.get("identity_key").and_then(Value::as_str) != Some(skill.identity_key.as_str())
            || meta.get("package_digest").and_then(Value::as_str)
                == Some(skill.package_digest.as_str())
        {
            continue;
        }
        meta["lifecycle"] = Value::String("superseded".into());
        write_json(&meta_path, &meta)
            .map_err(|err| format!("mark superseded {}: {err}", meta_path.display()))?;
    }
    Ok(())
}

fn materialize_canonical_package(
    root: &Path,
    skill: &DiscoveredSkill,
    dry_run: bool,
) -> Result<(PathBuf, &'static str), String> {
    let src = Path::new(&skill.source_path);
    if src
        .parent()
        .map(|parent| same_path(parent, root))
        .unwrap_or(false)
    {
        return Ok((src.to_path_buf(), "canonical"));
    }
    let dest = canonical_destination(root, skill);
    if dest.exists()
        && existing_package_digest(&dest, &skill.scope).as_deref()
            == Some(skill.package_digest.as_str())
    {
        if !dry_run {
            // Preserve a quarantined lifecycle across refreshes — the foreign
            // scan record always says "active" and must not overwrite it.
            let mut skill = skill.clone();
            let stored = read_json_file(&dest.join(PACKAGE_META)).and_then(|meta| {
                meta.get("lifecycle")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
            if stored.as_deref() == Some("candidate") {
                skill.lifecycle = "candidate".into();
            }
            write_package_meta(&dest, &skill)
                .map_err(|err| format!("update package metadata {}: {err}", dest.display()))?;
        }
        return Ok((dest, "unchanged"));
    }
    // M4 quarantine: brand-new foreign packages arrive as `candidate` —
    // they surface nowhere until a proposal activates them (never direct).
    let is_new = !dest.exists();
    let mut skill = skill.clone();
    if is_new
        && matches!(
            skill.source_kind.as_str(),
            "package" | "managed" | "workspace" | "plugin" | "extra"
        )
        && skill.ownership_class != "statesmith_authored"
        && !is_product_owned_slug(&skill.slug)
    {
        skill.lifecycle = "candidate".into();
    }
    if dry_run {
        return Ok((dest, "would_pull"));
    }
    fs::create_dir_all(root)
        .map_err(|err| format!("create canonical root {}: {err}", root.display()))?;
    supersede_prior_versions(root, &skill)?;
    copy_dir_recursive(src, &dest)
        .map_err(|err| format!("pull {} → {}: {err}", src.display(), dest.display()))?;
    write_package_meta(&dest, &skill)
        .map_err(|err| format!("write package metadata {}: {err}", dest.display()))?;
    Ok((dest, "pulled"))
}

fn materialize_reference_projection(
    dst: &Path,
    skill: &DiscoveredSkill,
    dry_run: bool,
) -> Result<&'static str, String> {
    if dst.exists() {
        match projection_digest(dst) {
            Some(existing_digest) if existing_digest == skill.package_digest => {
                return Ok("unchanged");
            }
            Some(_) => {}
            None if is_product_owned_slug(&skill.slug)
                || skill.ownership_class == "statesmith_authored" => {}
            None => return Ok("unmanaged_conflict"),
        }
    }
    if dry_run {
        return Ok(if dst.exists() {
            "would_update"
        } else {
            "would_project"
        });
    }
    if dst.exists() {
        fs::remove_dir_all(dst)
            .map_err(|err| format!("replace reference wrapper {}: {err}", dst.display()))?;
    }
    write_reference_wrapper(dst, skill)
        .map_err(|err| format!("reference wrapper {}: {err}", dst.display()))?;
    Ok("projected_reference")
}

fn prune_stale_managed_projections(
    root: &Path,
    skill: &DiscoveredSkill,
    dry_run: bool,
) -> Result<Vec<SyncAction>, String> {
    let Ok(entries) = fs::read_dir(root) else {
        return Ok(Vec::new());
    };
    let mut actions = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(meta) = read_json_file(&dir.join(PROJECTION_META)) else {
            continue;
        };
        if meta.get("managed_by").and_then(Value::as_str) != Some("stateroot")
            || meta.get("identity_key").and_then(Value::as_str) != Some(skill.identity_key.as_str())
            || meta.get("package_digest").and_then(Value::as_str)
                == Some(skill.package_digest.as_str())
        {
            continue;
        }
        let action = if dry_run {
            "would_remove_stale"
        } else {
            fs::remove_dir_all(&dir)
                .map_err(|err| format!("remove stale projection {}: {err}", dir.display()))?;
            "removed_stale"
        };
        actions.push(SyncAction {
            action: action.into(),
            slug: skill.slug.clone(),
            detail: dir.display().to_string(),
        });
    }
    Ok(actions)
}

fn materialize_router_skill(
    agents_root: &Path,
    dry_run: bool,
) -> Result<Option<SyncAction>, String> {
    let dest = agents_root.join("stateroot-skill-router");
    let body = r#"---
name: stateroot-skill-router
description: Discover pooled StateRoot skills and explicitly delegate bounded microtasks to the native source harness when a capability is reference-only.
---

# StateRoot Skill Router

Use this skill when the user asks what skills are available across harnesses,
asks to use a capability owned by another harness, or requests a bounded
cross-harness microtask.

1. Run `stateroot skill list` or `stateroot skill status --json`.
2. Use portable packages from `.agents/skills` directly.
3. For a reference-only capability, invoke:
   `stateroot harness run <harness> --skill <skill> --objective "<bounded microtask>"`
4. Include the working directory, expected outputs, constraints, and tests.
5. Inspect produced files/test results; do not replay the foreign transcript.
6. External launch is explicit-request only. GUI-only harnesses return a
   structured handoff rather than a fake launch.
"#;
    let digest = sha256_hex(body.as_bytes());
    let mut reclaiming = false;
    if dest.exists() {
        match projection_digest(&dest) {
            Some(existing) if existing == digest => return Ok(None),
            Some(_) => {}
            None => {
                // Router is product-owned; reclaim any unmarked copy.
                reclaiming = true;
            }
        }
    }
    if dry_run {
        return Ok(Some(SyncAction {
            action: "would_project".into(),
            slug: "stateroot-skill-router".into(),
            detail: dest.display().to_string(),
        }));
    }
    if dest.exists() {
        fs::remove_dir_all(&dest)
            .map_err(|err| format!("replace router skill {}: {err}", dest.display()))?;
    }
    fs::create_dir_all(&dest)
        .map_err(|err| format!("create router skill {}: {err}", dest.display()))?;
    fs::write(dest.join("SKILL.md"), body)
        .map_err(|err| format!("write router skill {}: {err}", dest.display()))?;
    write_json(
        &dest.join(PROJECTION_META),
        &serde_json::json!({
            "schema_version": "stateroot.skill_projection.v1",
            "managed_by": "stateroot",
            "projection_kind": "federation_router",
            "slug": "stateroot-skill-router",
            "package_digest": digest,
            "source_harness": "skillsagent",
            "native_harness": "skillsagent",
            "native_invocation": "stateroot harness run <harness> --skill <skill> --objective <microtask>",
        }),
    )
    .map_err(|err| format!("write router metadata {}: {err}", dest.display()))?;
    Ok(Some(SyncAction {
        action: if reclaiming {
            "reclaimed".into()
        } else {
            "projected".into()
        },
        slug: "stateroot-skill-router".into(),
        detail: dest.display().to_string(),
    }))
}

/// Seed/update the global portable product skill package from authoritative
/// embedded bytes. Adapter copies must project from this package — they are
/// not a competing source of truth.
pub fn ensure_product_skill_package(
    home: &Path,
    files: &[(String, Vec<u8>)],
) -> Result<SyncAction, String> {
    let dest = home.join(".stateroot/skills/stateroot");
    let mut file_digests = BTreeMap::new();
    for (rel, bytes) in files {
        let rel = rel.replace('\\', "/");
        if rel == PROJECTION_META || rel == PACKAGE_META || rel.starts_with("assets/") {
            // Convenience assets under assets/ are harness stubs, not package body.
            continue;
        }
        file_digests.insert(rel, sha256_hex(bytes));
    }
    if file_digests.is_empty() {
        return Err("product skill package has no files to seed".into());
    }
    let digest = package_digest(&file_digests);
    let identity = product_identity_key("stateroot");
    if dest.exists() && existing_package_digest(&dest, "global").as_deref() == Some(digest.as_str())
    {
        // Refresh ownership meta even when bytes match.
        let skill = DiscoveredSkill {
            identity_key: identity,
            slug: "stateroot".into(),
            name: "stateroot".into(),
            description: String::new(),
            harness: "skillsagent".into(),
            source_path: dest.display().to_string(),
            scope: "global".into(),
            ownership_class: "statesmith_authored".into(),
            lifecycle: "active".into(),
            visibility: String::new(),
            package_digest: digest,
            files: file_digests,
            source_kind: "product".into(),
            license: None,
            native_harness: "skillsagent".into(),
            native_invocation: "stateroot resume --harness <id>".into(),
            compatibility: serde_json::json!({"compatible": true, "reasons": []}),
            hash_exclusions: Vec::new(),
        };
        write_package_meta(&dest, &skill)
            .map_err(|err| format!("update product package meta {}: {err}", dest.display()))?;
        return Ok(SyncAction {
            action: "unchanged".into(),
            slug: "stateroot".into(),
            detail: dest.display().to_string(),
        });
    }

    fs::create_dir_all(home.join(".stateroot/skills"))
        .map_err(|err| format!("create product skill root: {err}"))?;
    let tmp = home.join(format!(
        ".stateroot/skills/.stateroot-product-tmp-{}",
        std::process::id()
    ));
    if tmp.exists() {
        fs::remove_dir_all(&tmp)
            .map_err(|err| format!("remove product seed temp {}: {err}", tmp.display()))?;
    }
    fs::create_dir_all(&tmp).map_err(|err| format!("create product seed temp: {err}"))?;
    for (rel, bytes) in files {
        let rel = rel.replace('\\', "/");
        if rel == PROJECTION_META || rel == PACKAGE_META || rel.starts_with("assets/") {
            continue;
        }
        let target = tmp.join(&rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("create {}: {err}", parent.display()))?;
        }
        fs::write(&target, bytes).map_err(|err| format!("write {}: {err}", target.display()))?;
    }
    let skill = DiscoveredSkill {
        identity_key: identity,
        slug: "stateroot".into(),
        name: "stateroot".into(),
        description: String::new(),
        harness: "skillsagent".into(),
        source_path: dest.display().to_string(),
        scope: "global".into(),
        ownership_class: "statesmith_authored".into(),
        lifecycle: "active".into(),
        visibility: String::new(),
        package_digest: digest.clone(),
        files: file_digests,
        source_kind: "product".into(),
        license: None,
        native_harness: "skillsagent".into(),
        native_invocation: "stateroot resume --harness <id>".into(),
        compatibility: serde_json::json!({"compatible": true, "reasons": []}),
        hash_exclusions: Vec::new(),
    };
    write_package_meta(&tmp, &skill).map_err(|err| format!("write product package meta: {err}"))?;
    let action = if dest.exists() {
        fs::remove_dir_all(&dest)
            .map_err(|err| format!("replace product package {}: {err}", dest.display()))?;
        "updated"
    } else {
        "seeded"
    };
    fs::rename(&tmp, &dest).map_err(|err| format!("activate product package: {err}"))?;
    Ok(SyncAction {
        action: action.into(),
        slug: "stateroot".into(),
        detail: dest.display().to_string(),
    })
}

fn project_product_adapters(
    home: &Path,
    project_dir: Option<&Path>,
    skill: &DiscoveredSkill,
    dry_run: bool,
    probe: &dyn Fn(&str) -> bool,
) -> Result<Vec<SyncAction>, String> {
    let mut actions = Vec::new();
    let src = PathBuf::from(&skill.source_path);
    let agents_root = if skill.scope == "project" {
        project_dir
            .ok_or_else(|| "project-scoped product skill requires project directory".to_string())?
            .join(".agents/skills")
    } else {
        home.join(".agents/skills")
    };
    actions.extend(prune_stale_managed_projections(
        &agents_root,
        skill,
        dry_run,
    )?);
    let agents_dest = agents_root.join(&skill.slug);
    let action =
        materialize_managed_projection(&src, &agents_dest, skill, "portable_package", dry_run)?;
    actions.push(SyncAction {
        action: action.into(),
        slug: skill.slug.clone(),
        detail: agents_dest.display().to_string(),
    });

    let reg = load_registry()?;
    for entry in &reg.harnesses {
        if entry.id == "skillsagent" || entry.id == "planner" {
            continue;
        }
        let roots = if skill.scope == "global" {
            &entry.projection_roots.global
        } else {
            &entry.projection_roots.project
        };
        for rel in roots {
            if is_shared_federation_root(rel) {
                continue;
            }
            let root = if skill.scope == "global" {
                expand_home_relative(rel, home)
            } else {
                project_dir
                    .ok_or_else(|| {
                        "project-scoped product skill requires project directory".to_string()
                    })?
                    .join(rel)
            };
            // Detection-gating (R2.1): never create configs/dirs for
            // harnesses absent from the machine.
            if !harness_detected_with(entry, home, Some(&root), probe) {
                continue;
            }
            actions.extend(prune_stale_managed_projections(&root, skill, dry_run)?);
            let dest = root.join(&skill.slug);
            let action =
                materialize_managed_projection(&src, &dest, skill, "adapter_bridge", dry_run)?;
            actions.push(SyncAction {
                action: action.into(),
                slug: skill.slug.clone(),
                detail: format!("{} adapter → {}", entry.id, dest.display()),
            });
        }
    }
    Ok(actions)
}

/// Always reclaim/update product-owned projections (portable + adapters +
/// router), independent of foreign `--push`.
pub fn refresh_product_projections(
    home: &Path,
    project_dir: Option<&Path>,
) -> Result<Vec<SyncAction>, String> {
    refresh_product_projections_inner(home, project_dir, false, &binary_probe(None))
}

fn refresh_product_projections_inner(
    home: &Path,
    project_dir: Option<&Path>,
    dry_run: bool,
    probe: &dyn Fn(&str) -> bool,
) -> Result<Vec<SyncAction>, String> {
    let mut actions = Vec::new();
    let mut local = list_portable(&home.join(".stateroot/skills"), "global");
    if let Some(project_dir) = project_dir {
        local.extend(list_portable(
            &project_dir.join(".stateroot/skills"),
            "project",
        ));
    }
    for skill in local {
        if !is_product_owned_slug(&skill.slug) && skill.ownership_class != "statesmith_authored" {
            continue;
        }
        if skill.slug == "stateroot-skill-router" {
            continue;
        }
        actions.extend(project_product_adapters(
            home,
            project_dir,
            &skill,
            dry_run,
            probe,
        )?);
    }
    let router_root = project_dir
        .map(|p| p.join(".agents/skills"))
        .unwrap_or_else(|| home.join(".agents/skills"));
    if let Some(action) = materialize_router_skill(&router_root, dry_run)? {
        actions.push(action);
    }
    Ok(actions)
}

/// Bidirectional sync into `.stateroot/skills` + `.agents/skills` projections.
pub fn sync_project(
    project_dir: &Path,
    options: &SyncOptions,
    home: Option<&Path>,
) -> Result<Vec<SyncAction>, String> {
    sync_scoped(Some(project_dir), options, home)
}

/// Synchronize only user-global skill roots.
///
/// Intended for `stateroot setup` when it runs outside a project. It creates
/// user-global canonical/projection roots under the home directory and never
/// writes `.stateroot/` or `.agents/` into the setup invocation directory.
pub fn sync_global(home: &Path, options: &SyncOptions) -> Result<Vec<SyncAction>, String> {
    sync_scoped(None, options, Some(home))
}

fn sync_scoped(
    project_dir: Option<&Path>,
    options: &SyncOptions,
    home: Option<&Path>,
) -> Result<Vec<SyncAction>, String> {
    let home = match home {
        Some(path) => path.to_path_buf(),
        None => home_dir()?,
    };
    let discovered = match project_dir {
        Some(project_dir) => discover_all(project_dir, Some(&home))?,
        None => discover_global(&home)?,
    };
    let live_identities: BTreeSet<String> = discovered
        .iter()
        .filter(|skill| skill.source_kind != "portable")
        .map(|skill| skill.identity_key.clone())
        .collect();
    let mut actions = Vec::new();

    if options.pull || !options.push {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for skill in &discovered {
            if matches!(skill.lifecycle.as_str(), "superseded" | "deactivated") {
                continue;
            }
            // Product packages are seeded from embedded assets — never pull
            // harness adapter copies over the product source of truth.
            if is_product_owned_slug(&skill.slug)
                && skill.source_kind != "portable"
                && skill.source_kind != "product"
            {
                continue;
            }
            if skill.source_kind == "portable" && live_identities.contains(&skill.identity_key) {
                continue;
            }
            let scope = if skill.scope == "global" {
                "global"
            } else {
                "project"
            };
            let portable_root = if scope == "global" {
                home.join(".stateroot/skills")
            } else {
                project_dir
                    .expect("project-scoped discovery requires project directory")
                    .join(".stateroot/skills")
            };
            let agents_root = if scope == "global" {
                home.join(".agents/skills")
            } else {
                project_dir
                    .expect("project-scoped discovery requires project directory")
                    .join(".agents/skills")
            };
            actions.extend(prune_stale_managed_projections(
                &agents_root,
                skill,
                options.dry_run,
            )?);
            // M4 quarantine: resolve the effective lifecycle for THIS record.
            // The foreign scan always reports "active"; the canonical
            // sidecar may already quarantine it, and brand-new foreign
            // packages are quarantined on arrival.
            let mut skill = skill.clone();
            let canonical_dest = canonical_destination(&portable_root, &skill);
            let stored_lifecycle =
                read_json_file(&canonical_dest.join(PACKAGE_META)).and_then(|meta| {
                    meta.get("lifecycle")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
            if stored_lifecycle.as_deref() == Some("candidate")
                || (stored_lifecycle.is_none()
                    && matches!(
                        skill.source_kind.as_str(),
                        "package" | "managed" | "workspace" | "plugin" | "extra"
                    )
                    && skill.ownership_class != "statesmith_authored"
                    && !is_product_owned_slug(&skill.slug))
            {
                skill.lifecycle = "candidate".into();
            }
            if matches!(skill.lifecycle.as_str(), "reference_only" | "external_only") {
                let key = format!("reference:{scope}:{}:{}", skill.native_harness, skill.slug);
                if !seen.insert(key) {
                    continue;
                }
                let suffix = if skill.lifecycle == "reference_only" {
                    "ref"
                } else {
                    "external"
                };
                let dest =
                    agents_root.join(format!("{}--{}-{suffix}", skill.slug, skill.native_harness));
                let action = materialize_reference_projection(&dest, &skill, options.dry_run)?;
                actions.push(SyncAction {
                    action: action.into(),
                    slug: skill.slug.clone(),
                    detail: format!(
                        "{} capability via {} → {}",
                        skill.lifecycle,
                        skill.native_harness,
                        dest.display()
                    ),
                });
                continue;
            }
            let key = format!("package:{scope}:{}:{}", skill.slug, skill.package_digest);
            if !seen.insert(key) {
                continue;
            }
            let (canonical, canonical_action) =
                materialize_canonical_package(&portable_root, &skill, options.dry_run)?;
            if canonical_action != "canonical" {
                actions.push(SyncAction {
                    action: canonical_action.into(),
                    slug: skill.slug.clone(),
                    detail: format!("{} → {}", skill.source_path, canonical.display()),
                });
            }
            if skill.lifecycle == "candidate" {
                // M4 quarantine: canonical package exists, but candidates
                // surface nowhere until approved.
                actions.push(SyncAction {
                    action: "candidate_quarantined".into(),
                    slug: skill.slug.clone(),
                    detail: format!(
                        "{} (activate via `stateroot skill promote {}`)",
                        skill.slug, skill.slug
                    ),
                });
                continue;
            }
            let projection_name = canonical
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&skill.slug);
            let agents_dest = agents_root.join(projection_name);
            let action = materialize_managed_projection(
                &canonical,
                &agents_dest,
                &skill,
                "portable_package",
                options.dry_run,
            )?;
            actions.push(SyncAction {
                action: action.into(),
                slug: skill.slug.clone(),
                detail: agents_dest.display().to_string(),
            });
        }
        let router_root = project_dir
            .map(|project_dir| project_dir.join(".agents/skills"))
            .unwrap_or_else(|| home.join(".agents/skills"));
        if let Some(action) = materialize_router_skill(&router_root, options.dry_run)? {
            actions.push(action);
        }
    }

    if options.push {
        // Push portable packages out to adapter bridges that need harness-specific roots.
        let probe = binary_probe(options.cmd_probe.as_deref());
        let reg = load_registry()?;
        let mut local = list_portable(&home.join(".stateroot/skills"), "global");
        if let Some(project_dir) = project_dir {
            local.extend(list_portable(
                &project_dir.join(".stateroot/skills"),
                "project",
            ));
        }
        for skill in local {
            let src = PathBuf::from(&skill.source_path);
            for entry in &reg.harnesses {
                if entry.id == "skillsagent" || entry.id == "planner" {
                    continue;
                }
                // Prefer .agents/skills which most harnesses already scan; only
                // bridge when the harness lists an extra native projection root.
                let roots = if skill.scope == "global" {
                    &entry.projection_roots.global
                } else {
                    &entry.projection_roots.project
                };
                for rel in roots {
                    if is_shared_federation_root(rel) {
                        continue;
                    }
                    let root = if skill.scope == "global" {
                        expand_home_relative(rel, &home)
                    } else {
                        project_dir
                            .expect("project-scoped portable skill requires project directory")
                            .join(rel)
                    };
                    // Detection-gating (R2.1): never create dirs for absent harnesses.
                    if !harness_detected_with(entry, &home, Some(&root), &probe) {
                        continue;
                    }
                    actions.extend(prune_stale_managed_projections(
                        &root,
                        &skill,
                        options.dry_run,
                    )?);
                    let dest = root.join(&skill.slug);
                    let action = materialize_managed_projection(
                        &src,
                        &dest,
                        &skill,
                        "adapter_bridge",
                        options.dry_run,
                    )?;
                    actions.push(SyncAction {
                        action: action.into(),
                        slug: skill.slug.clone(),
                        detail: format!("{} adapter → {}", entry.id, dest.display()),
                    });
                }
            }
        }
    }

    // Product reclaim always runs — even when foreign --push is off.
    actions.extend(refresh_product_projections_inner(
        &home,
        project_dir,
        options.dry_run,
        &binary_probe(options.cmd_probe.as_deref()),
    )?);

    Ok(actions)
}

fn list_portable(root: &Path, scope: &str) -> Vec<DiscoveredSkill> {
    let mut out = Vec::new();
    scan_tree(root, "skillsagent", scope, "portable", &mut out);
    out
}

pub fn status_report(project_dir: &Path, home: Option<&Path>) -> Result<Value, String> {
    let home = match home {
        Some(path) => path.to_path_buf(),
        None => home_dir()?,
    };
    let discovered = discover_all(project_dir, Some(&home))?;
    let global_portable = list_portable(&home.join(".stateroot/skills"), "global");
    let project_portable = list_portable(&project_dir.join(".stateroot/skills"), "project");
    let reference = discovered
        .iter()
        .filter(|s| s.lifecycle == "reference_only")
        .count();
    let external_only = discovered
        .iter()
        .filter(|s| s.lifecycle == "external_only")
        .count();
    Ok(serde_json::json!({
        "discovered": discovered.len(),
        "portable": global_portable.len() + project_portable.len(),
        "portable_global": global_portable.len(),
        "portable_project": project_portable.len(),
        "reference_only": reference,
        "external_only": external_only,
        "home": home,
        "global_projection_root": home.join(".agents/skills"),
        "project_projection_root": project_dir.join(".agents/skills"),
        "skills": discovered,
    }))
}

pub fn doctor(project_dir: &Path, home: Option<&Path>) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    let home = match home {
        Some(path) => path.to_path_buf(),
        None => home_dir()?,
    };
    let reg = load_registry()?;
    notes.push(format!(
        "registry {} with {} harnesses",
        reg.schema_version,
        reg.harnesses.len()
    ));
    notes.push(format!("resolved home: {}", home.display()));
    let discovered = discover_all(project_dir, Some(&home))?;
    notes.push(format!("discovered {} skill packages", discovered.len()));
    let mut by_harness: BTreeMap<String, usize> = BTreeMap::new();
    for skill in &discovered {
        *by_harness.entry(skill.harness.clone()).or_default() += 1;
    }
    for (harness, count) in by_harness {
        notes.push(format!("  {harness}: {count}"));
    }
    if !project_dir.join(".stateroot").is_dir() {
        notes.push("warning: not a stateroot project (missing .stateroot/)".into());
    }
    let state = crate::openclaw_identity::openclaw_state_dir(&home);
    if state.is_dir() {
        notes.push(format!("openclaw state: {}", state.display()));
        let plugin_skills = state.join("plugin-skills");
        if plugin_skills.is_dir() {
            notes.push(format!(
                "openclaw active plugin skills: {}",
                plugin_skills.display()
            ));
        }
        let bundled = openclaw_bundled_skill_roots(&home);
        if bundled.is_empty() {
            notes.push(
                "warning: OpenClaw detected but bundled skills root was not resolved from PATH/env"
                    .into(),
            );
        } else {
            for root in bundled {
                notes.push(format!("openclaw bundled skills: {}", root.display()));
            }
        }
    }
    if !home.join(".agents/skills").is_dir() {
        notes.push("note: global .agents/skills missing — sync will create it".into());
    }
    if !project_dir.join(".agents/skills").is_dir() {
        notes.push("note: .agents/skills missing — run `stateroot skill sync` to project".into());
    }
    Ok(notes)
}

/// Build a lossless packages payload suitable for the federation sync API.
///
/// Closed-source/reference-only implementations are intentionally metadata
/// only. Shareable packages include every file as base64 exactly once per
/// digest; duplicate origins still get their own metadata record.
pub fn packages_for_report(skills: &[DiscoveredSkill]) -> Result<Value, String> {
    let mut content_emitted = BTreeSet::new();
    let live_identities: BTreeSet<String> = skills
        .iter()
        .filter(|skill| skill.source_kind != "portable")
        .map(|skill| skill.identity_key.clone())
        .collect();
    let mut rows = Vec::new();
    for skill in skills {
        if matches!(skill.lifecycle.as_str(), "superseded" | "deactivated")
            || (skill.source_kind == "portable" && live_identities.contains(&skill.identity_key))
        {
            continue;
        }
        let projection_name = if skill.lifecycle == "active" {
            skill.slug.clone()
        } else {
            let suffix = if skill.lifecycle == "reference_only" {
                "ref"
            } else {
                "external"
            };
            format!("{}--{}-{suffix}", skill.slug, skill.native_harness)
        };
        let mut row = serde_json::json!({
            "slug": skill.slug,
            "identity_key": skill.identity_key,
            "name": skill.name,
            "description": skill.description,
            "harness": skill.harness,
            "source_path": skill.source_path,
            "scope": skill.scope,
            "ownership_class": skill.ownership_class,
            "lifecycle": skill.lifecycle,
            "package_digest": skill.package_digest,
            "files": skill.files,
            "source_kind": skill.source_kind,
            "license": skill.license,
            "native_harness": skill.native_harness,
            "native_invocation": skill.native_invocation,
            "compatibility": skill.compatibility,
            "package_path": format!("skills/{}", skill.slug),
            "projections": [{
                "harness": "shared_agents",
                "target_path": format!(".agents/skills/{projection_name}"),
                "projection_kind": if skill.lifecycle == "active" {
                    "portable_package"
                } else {
                    "delegation_wrapper"
                },
                "status": "active",
            }],
        });
        if skill.lifecycle != "reference_only"
            && content_emitted.insert(skill.package_digest.clone())
        {
            let root = Path::new(&skill.source_path);
            let mut contents = serde_json::Map::new();
            for relative in skill.files.keys() {
                if relative == PROJECTION_META || relative == PACKAGE_META {
                    continue;
                }
                let bytes = fs::read(root.join(relative)).map_err(|err| {
                    format!(
                        "read package file {}/{} for report: {err}",
                        root.display(),
                        relative
                    )
                })?;
                contents.insert(
                    relative.clone(),
                    Value::String(base64::engine::general_purpose::STANDARD.encode(bytes)),
                );
            }
            row["package_files_base64"] = Value::Object(contents);
        }
        rows.push(row);
    }
    Ok(Value::Array(rows))
}

/// Activate a quarantined skill package (M4): set `lifecycle: active` in
/// its canonical sidecar. `scope`: `user` (home store) or `project`.
pub fn activate_skill(
    project_dir: &Path,
    home: &Path,
    scope: &str,
    slug: &str,
) -> Result<bool, String> {
    let root = if scope == "user" {
        home.join(".stateroot/skills")
    } else {
        crate::local_store::root(project_dir).join("skills")
    };
    let meta_path = root.join(slug).join(PACKAGE_META);
    let Some(mut meta) = read_json_file(&meta_path) else {
        return Ok(false);
    };
    meta["lifecycle"] = serde_json::json!("active");
    write_json(&meta_path, &meta).map_err(|err| format!("write {}: {err}", meta_path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn registry_loads_and_normalizes_claude() {
        let reg = load_registry().expect("registry");
        assert!(reg.harnesses.len() >= 16);
        assert_eq!(normalize_harness("claude-code"), "claude");
        assert_eq!(display_name("skillsagent"), "StateSmith");
    }

    #[test]
    fn discovers_cursor_and_skips_projection_loop() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let project = tmp.path().join("proj");
        fs::create_dir_all(home.join(".cursor/skills/demo")).unwrap();
        let mut f = fs::File::create(home.join(".cursor/skills/demo/SKILL.md")).unwrap();
        writeln!(f, "---\nname: demo\ndescription: d\n---\n# Demo\n").unwrap();

        fs::create_dir_all(project.join(".stateroot/skills/loop")).unwrap();
        let mut g = fs::File::create(project.join(".stateroot/skills/loop/SKILL.md")).unwrap();
        writeln!(
            g,
            "{MANAGED_MARKER} origin_harness=cursor -->\n---\nname: loop\n---\n"
        )
        .unwrap();

        let found = discover_all(&project, Some(&home)).unwrap();
        assert!(found
            .iter()
            .any(|s| s.slug == "demo" && s.harness == "cursor"));
        assert!(!found.iter().any(|s| s.slug == "loop"));
    }

    fn write_skill(root: &Path, name: &str, description: &str) {
        fs::create_dir_all(root).unwrap();
        fs::write(
            root.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: \"{description}\"\n---\n# {name}\n"),
        )
        .unwrap();
    }

    fn write_skill_with_metadata(root: &Path, name: &str, metadata: serde_json::Value) {
        fs::create_dir_all(root).unwrap();
        fs::write(
            root.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: \"d\"\nmetadata: {}\n---\n# {name}\n",
                serde_json::to_string(&metadata).unwrap()
            ),
        )
        .unwrap();
    }

    #[test]
    fn package_meta_sidecar_emits_d2_union_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let project = tmp.path().join("project");
        fs::create_dir_all(project.join(".stateroot")).unwrap();
        write_skill(&home.join(".cursor/skills/union-demo"), "union-demo", "d");

        let options = SyncOptions {
            dry_run: false,
            push: false,
            pull: true,
            cmd_probe: Some(vec![]),
        };
        sync_project(&project, &options, Some(&home)).unwrap();

        let meta = read_json_file(&home.join(".stateroot/skills/union-demo/skill.federation.json"))
            .expect("federation sidecar");
        for key in [
            "schema_version",
            "identity_key",
            "slug",
            "scope",
            "package_digest",
            "origin",
            "ownership_class",
            "license",
            "lifecycle",
            "compatibility",
            "native_harness",
            "native_invocation",
        ] {
            assert!(meta.get(key).is_some(), "missing union field {key}: {meta}");
        }
        assert_eq!(
            meta["schema_version"],
            serde_json::json!("stateroot.skill_package.v1")
        );
        assert_eq!(meta["origin"]["harness"], serde_json::json!("cursor"));
        assert!(meta["origin"]["source_path"]
            .as_str()
            .unwrap()
            .contains("union-demo"));
        assert_eq!(meta["origin"]["source_kind"], serde_json::json!("package"));
        assert!(meta["compatibility"].is_object());
    }

    #[test]
    fn openclaw_compat_primary_env_api_key_and_install_bins() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        write_skill_with_metadata(
            &home.join(".openclaw/skills/image-lab"),
            "image-lab",
            serde_json::json!({"openclaw": {
                "requires": {
                    "bins": ["r2-missing-bin-xyz"],
                    "env": ["R2_IMAGE_LAB_KEY"]
                },
                "primaryEnv": "R2_IMAGE_LAB_KEY",
                "install": [{"id": "brew", "kind": "brew", "bins": ["r2-missing-bin-xyz"]}],
            }}),
        );
        // Config supplies the apiKey for the declared primaryEnv.
        fs::write(
            home.join(".openclaw/openclaw.json"),
            serde_json::to_vec(&serde_json::json!({
                "skills": {"entries": {"image-lab": {"apiKey": "sk-test"}}},
            }))
            .unwrap(),
        )
        .unwrap();

        let mut found = Vec::new();
        discover_openclaw_skills(&home, &mut found);
        let skill = found
            .iter()
            .find(|s| s.slug == "image-lab")
            .expect("discovered");
        let compat = &skill.compatibility;
        let reasons = compat["reasons"].as_array().unwrap();
        // primaryEnv satisfied via skills.entries.<slug>.apiKey → no env reason.
        assert!(
            !reasons
                .iter()
                .any(|r| r.as_str().unwrap().contains("R2_IMAGE_LAB_KEY")),
            "apiKey must satisfy primaryEnv: {reasons:#?}"
        );
        // Missing bin stays incompatible (load-time gate) but honestly notes
        // the installer that provides it.
        assert!(
            reasons.iter().any(|r| r.as_str().unwrap()
                == "missing binary `r2-missing-bin-xyz` (installable via `brew`)"),
            "{reasons:#?}"
        );
        assert_eq!(compat["compatible"], serde_json::json!(false));
        assert_eq!(skill.lifecycle, "external_only");
    }

    #[test]
    fn openclaw_compat_env_satisfied_from_config_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        write_skill_with_metadata(
            &home.join(".openclaw/skills/cfg-skill"),
            "cfg-skill",
            serde_json::json!({"openclaw": {
                "requires": {"env": ["R2_CFG_ONLY_VAR"]},
            }}),
        );
        fs::write(
            home.join(".openclaw/openclaw.json"),
            serde_json::to_vec(&serde_json::json!({
                "skills": {"entries": {"cfg-skill": {"env": {"R2_CFG_ONLY_VAR": "from-config"}}}},
            }))
            .unwrap(),
        )
        .unwrap();

        let mut found = Vec::new();
        discover_openclaw_skills(&home, &mut found);
        let skill = found
            .iter()
            .find(|s| s.slug == "cfg-skill")
            .expect("discovered");
        assert_eq!(
            skill.compatibility["compatible"],
            serde_json::json!(true),
            "config-provided env must satisfy requires.env: {:#?}",
            skill.compatibility
        );
        assert_eq!(skill.lifecycle, "active");
    }

    #[test]
    fn openclaw_compat_primary_env_missing_without_api_key() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        write_skill_with_metadata(
            &home.join(".openclaw/skills/nokey-skill"),
            "nokey-skill",
            serde_json::json!({"openclaw": {
                "primaryEnv": "R2_DEFINITELY_UNSET_VAR",
            }}),
        );
        fs::create_dir_all(home.join(".openclaw")).unwrap();

        let mut found = Vec::new();
        discover_openclaw_skills(&home, &mut found);
        let skill = found
            .iter()
            .find(|s| s.slug == "nokey-skill")
            .expect("discovered");
        let reasons = skill.compatibility["reasons"].as_array().unwrap();
        assert!(
            reasons.iter().any(|r| r
                .as_str()
                .unwrap()
                .contains("missing environment variable `R2_DEFINITELY_UNSET_VAR`")),
            "{reasons:#?}"
        );
        assert_eq!(skill.lifecycle, "external_only");
    }

    #[test]
    fn discovers_real_openclaw_managed_plugin_and_configured_workspace_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let project = tmp.path().join("project");
        let workspace = tmp.path().join("openclaw-workspace");
        write_skill(
            &home.join(".openclaw/skills/managed-skill"),
            "managed-skill",
            "managed",
        );
        write_skill(
            &home.join(".openclaw/plugin-skills/plugin-skill"),
            "plugin-skill",
            "plugin",
        );
        write_skill(
            &workspace.join("skills/workspace-skill"),
            "workspace-skill",
            "workspace",
        );
        fs::create_dir_all(home.join(".openclaw")).unwrap();
        fs::write(
            home.join(".openclaw/openclaw.json"),
            serde_json::to_vec(&serde_json::json!({
                "agents": {"defaults": {"workspace": workspace}},
            }))
            .unwrap(),
        )
        .unwrap();

        let found = discover_all(&project, Some(&home)).unwrap();
        for expected in ["managed-skill", "plugin-skill", "workspace-skill"] {
            assert!(
                found
                    .iter()
                    .any(|skill| skill.slug == expected && skill.harness == "openclaw"),
                "missing {expected}: {found:#?}"
            );
        }
    }

    #[test]
    fn sync_preserves_scope_content_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let project = tmp.path().join("project");
        fs::create_dir_all(project.join(".stateroot")).unwrap();
        write_skill(
            &home.join(".cursor/skills/global-demo"),
            "global-demo",
            "global",
        );
        write_skill(
            &project.join(".cursor/skills/project-demo"),
            "project-demo",
            "project",
        );

        let options = SyncOptions {
            dry_run: false,
            push: false,
            pull: true,
            cmd_probe: None,
        };
        let first = sync_project(&project, &options, Some(&home)).unwrap();
        assert!(first.iter().any(|action| action.action == "pulled"));
        // M4 quarantine: foreign skills land as candidates — canonical
        // packages exist, projections do NOT, until activated.
        assert!(home
            .join(".stateroot/skills/global-demo/SKILL.md")
            .is_file());
        assert!(!home.join(".agents/skills/global-demo/SKILL.md").is_file());
        assert!(project
            .join(".stateroot/skills/project-demo/SKILL.md")
            .is_file());
        assert!(!project
            .join(".agents/skills/project-demo/SKILL.md")
            .is_file());
        assert!(first
            .iter()
            .any(|action| action.action == "candidate_quarantined"));
        assert!(!project.join(".stateroot/skills/global-demo").exists());
        assert!(!home.join(".stateroot/skills/project-demo").exists());
        let raw = fs::read_to_string(home.join(".stateroot/skills/global-demo/SKILL.md")).unwrap();
        assert!(!raw.contains(MANAGED_MARKER), "raw source was mutated");

        // Activate (the proposals approve path) → projections materialize.
        assert!(activate_skill(&project, &home, "user", "global-demo").unwrap());
        assert!(activate_skill(&project, &home, "project", "project-demo").unwrap());
        let _activated = sync_project(&project, &options, Some(&home)).unwrap();
        assert!(home.join(".agents/skills/global-demo/SKILL.md").is_file());
        assert!(project
            .join(".agents/skills/project-demo/SKILL.md")
            .is_file());
        assert!(home
            .join(".agents/skills/global-demo")
            .join(PROJECTION_META)
            .is_file());

        let second = sync_project(&project, &options, Some(&home)).unwrap();
        assert!(
            second
                .iter()
                .all(|action| !matches!(action.action.as_str(), "pulled" | "projected")),
            "second sync must be idempotent: {second:#?}"
        );

        write_skill(
            &home.join(".cursor/skills/global-demo"),
            "global-demo",
            "global v2",
        );
        let third = sync_project(&project, &options, Some(&home)).unwrap();
        // M4: the new version ALSO arrives quarantined; the old canonical is
        // superseded regardless.
        assert!(third.iter().any(|action| action.action == "removed_stale"));
        assert!(third
            .iter()
            .any(|action| action.action == "candidate_quarantined"));
        let old_meta = read_json_file(
            &home
                .join(".stateroot/skills/global-demo")
                .join(PACKAGE_META),
        )
        .unwrap();
        assert_eq!(old_meta["lifecycle"], "superseded");
        // Activate the versioned v2 candidate, then re-sync: the stale
        // projection is removed and exactly one stays active.
        let v2_dir = fs::read_dir(home.join(".stateroot/skills"))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .find(|name| name.starts_with("global-demo__"))
            .expect("versioned v2 canonical dir");
        assert!(activate_skill(&project, &home, "user", &v2_dir).unwrap());
        let fourth = sync_project(&project, &options, Some(&home)).unwrap();
        assert!(fourth.iter().any(|action| action.action == "projected"));
        let active_projections = fs::read_dir(home.join(".agents/skills"))
            .unwrap()
            .flatten()
            .filter(|entry| {
                read_json_file(&entry.path().join(PROJECTION_META))
                    .and_then(|meta| {
                        (meta.get("identity_key")?.as_str()?
                            == old_meta["identity_key"].as_str()?)
                        .then_some(())
                    })
                    .is_some()
            })
            .count();
        assert_eq!(
            active_projections, 1,
            "only the latest projection stays active"
        );
    }

    #[test]
    fn sync_never_clobbers_unmanaged_projection() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let project = tmp.path().join("project");
        fs::create_dir_all(project.join(".stateroot")).unwrap();
        write_skill(
            &project.join(".cursor/skills/project-demo"),
            "project-demo",
            "source",
        );
        write_skill(
            &project.join(".agents/skills/project-demo"),
            "project-demo",
            "unmanaged",
        );
        let options = SyncOptions {
            dry_run: false,
            push: false,
            pull: true,
            cmd_probe: None,
        };
        let actions = sync_project(&project, &options, Some(&home)).unwrap();
        // M4 quarantine: the foreign pull lands as a candidate — no
        // projection is even attempted, so unmanaged content is untouched.
        assert!(actions.iter().any(|action| {
            action.slug == "project-demo" && action.action == "candidate_quarantined"
        }));
        let content =
            fs::read_to_string(project.join(".agents/skills/project-demo/SKILL.md")).unwrap();
        assert!(content.contains("unmanaged"));

        // After activation the projection path still never clobbers
        // unmanaged content (conflict, not overwrite).
        assert!(activate_skill(&project, &home, "project", "project-demo").unwrap());
        let actions = sync_project(&project, &options, Some(&home)).unwrap();
        assert!(actions.iter().any(|action| {
            action.slug == "project-demo" && action.action == "unmanaged_conflict"
        }));
        let content =
            fs::read_to_string(project.join(".agents/skills/project-demo/SKILL.md")).unwrap();
        assert!(content.contains("unmanaged"));
    }

    #[test]
    fn sync_reclaims_product_stateroot_adapter_without_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        // Seed product package (authoritative).
        let files = vec![(
            "SKILL.md".to_string(),
            b"---\nname: stateroot\ndescription: Persistent project state via the `stateroot` CLI.\n---\n\n# StateRoot\n"
                .to_vec(),
        ), (
            "skill.manifest.json".to_string(),
            br#"{"install_strategy":"first_party_bootstrap","required_binaries":["stateroot"]}"#
                .to_vec(),
        )];
        let seeded = ensure_product_skill_package(&home, &files).unwrap();
        assert!(matches!(
            seeded.action.as_str(),
            "seeded" | "updated" | "unchanged"
        ));

        // Legacy Claude installer copy: no projection marker.
        let claude = home.join(".claude/skills/stateroot");
        fs::create_dir_all(&claude).unwrap();
        fs::write(
            claude.join("SKILL.md"),
            "---\nname: stateroot\ndescription: stale installer copy\n---\n\n# old\n",
        )
        .unwrap();

        // Pull-only sync must still reclaim product adapters.
        let actions = sync_global(
            &home,
            &SyncOptions {
                dry_run: false,
                push: false,
                pull: true,
                cmd_probe: None,
            },
        )
        .unwrap();
        assert!(
            actions.iter().any(|action| {
                action.slug == "stateroot"
                    && matches!(
                        action.action.as_str(),
                        "reclaimed" | "projected" | "updated"
                    )
                    && action.detail.contains("claude adapter")
            }),
            "expected product reclaim on Claude adapter without --push: {actions:#?}"
        );
        assert!(claude.join(PROJECTION_META).is_file());
        let content = fs::read_to_string(claude.join("SKILL.md")).unwrap();
        assert!(content.contains("`stateroot` CLI"));
        assert!(!content.contains("stale installer copy"));
    }

    #[test]
    fn registry_exposes_product_skills() {
        let reg = load_registry().unwrap();
        assert!(reg.product_skills.contains(&"stateroot".to_string()));
        assert!(is_product_owned_slug("stateroot"));
    }

    #[test]
    fn registry_exposes_mcp_config_targets() {
        let reg = load_registry().unwrap();
        let cursor = reg.harnesses.iter().find(|h| h.id == "cursor").unwrap();
        assert!(cursor
            .mcp_config
            .global
            .iter()
            .any(|t| t.path == ".cursor/mcp.json" && t.shape == "mcpServers"));
        let hermes = reg.harnesses.iter().find(|h| h.id == "hermes").unwrap();
        assert!(hermes
            .mcp_config
            .global
            .iter()
            .any(|t| t.shape == "mcp_servers"));
        let skillsagent = reg
            .harnesses
            .iter()
            .find(|h| h.id == "skillsagent")
            .unwrap();
        assert!(skillsagent
            .mcp_config
            .global
            .iter()
            .any(|t| t.path == ".stateroot/tools/mcp.cloud.json" && t.shape == "mcpServers"));
    }

    #[test]
    fn global_sync_never_writes_into_setup_working_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let setup_cwd = tmp.path().join("outside-project");
        fs::create_dir_all(&setup_cwd).unwrap();
        write_skill(
            &home.join(".cursor/skills/global-demo"),
            "global-demo",
            "global",
        );

        let actions = sync_global(
            &home,
            &SyncOptions {
                dry_run: false,
                push: true,
                pull: true,
                cmd_probe: None,
            },
        )
        .unwrap();
        assert!(actions.iter().any(|action| action.slug == "global-demo"));
        assert!(home
            .join(".stateroot/skills/global-demo/SKILL.md")
            .is_file());
        // M4 quarantine: candidate — no projection until activated.
        assert!(!home.join(".agents/skills/global-demo/SKILL.md").is_file());
        assert!(activate_skill(&std::path::PathBuf::new(), &home, "user", "global-demo").unwrap());
        let _ = sync_global(
            &home,
            &SyncOptions {
                dry_run: false,
                push: true,
                pull: true,
                cmd_probe: None,
            },
        )
        .unwrap();
        assert!(home.join(".agents/skills/global-demo/SKILL.md").is_file());
        assert!(home
            .join(".agents/skills/stateroot-skill-router/SKILL.md")
            .is_file());
        assert!(
            !setup_cwd.join(".stateroot").exists() && !setup_cwd.join(".agents").exists(),
            "global setup must not create project projections in its cwd"
        );
    }

    #[test]
    fn closed_builtin_report_is_metadata_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("builtin");
        write_skill(&root, "closed-demo", "closed");
        let skill = scan_skill_dir(&root, "codex", "global", "builtin_reference").unwrap();
        let report = packages_for_report(&[skill]).unwrap();
        assert!(report[0].get("package_files_base64").is_none());
        assert_eq!(report[0]["lifecycle"], "reference_only");
        assert_eq!(report[0]["native_harness"], "codex");
    }

    #[test]
    fn package_digest_matches_cross_language_contract() {
        let files = BTreeMap::from([
            ("SKILL.md".to_string(), "abc".to_string()),
            ("refs/a.md".to_string(), "def".to_string()),
        ]);
        assert_eq!(
            package_digest(&files),
            "3b698576bc7fca509d64458f714f500e9d4055c6d0024efa3b09f40453d809e8"
        );
    }
}
