//! Universal harness detection: binary probing (platform-aware) AND
//! config-dir markers, reported as separate evidence.
//!
//! Motivation: config-dir-only detection misreads leftovers — a `~/.gemini`
//! folder outlives the gemini binary, and "folder exists" alone used to flag
//! the harness as present. [`detect_harnesses`] therefore reports
//! `binary_found` and `config_dir` independently; a config-only row reads as
//! "config leftover, binary not found" in any surfaced output.
//!
//! Binary probing runs `where.exe <cmd>` on Windows and `which <cmd>`
//! elsewhere (argument arrays only — no shell string). The [`Prober`] trait
//! keeps probing injectable so tests never touch the real PATH.

use std::path::Path;

use super::registry::Tier;

/// Detection outcome for one registry row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    /// Canonical harness id.
    pub id: String,
    /// A harness binary resolves on PATH.
    pub binary_found: bool,
    /// A config marker exists under home (may be a leftover).
    pub config_dir: bool,
    /// Human-readable evidence lines (e.g. "binary `gemini` on PATH",
    /// "config ~/.gemini").
    pub evidence: Vec<String>,
    /// Registry tier (install completeness).
    pub tier: Tier,
}

impl Detection {
    /// Present on the machine in any form — binary or config leftover.
    pub fn installed(&self) -> bool {
        self.binary_found || self.config_dir
    }

    /// Short evidence suffix for pickers, e.g. "binary" vs
    /// "config only, binary not found".
    pub fn evidence_label(&self) -> String {
        if self.binary_found {
            "binary".to_string()
        } else if self.config_dir {
            "config only, binary not found".to_string()
        } else {
            "not detected".to_string()
        }
    }
}

/// Binary probe abstraction (injectable for tests).
pub trait Prober {
    /// True when `cmd` resolves as an executable on this system.
    fn probe(&self, cmd: &str) -> bool;
}

/// Real prober: `where.exe <cmd>` on Windows, `which <cmd>` elsewhere.
/// Spawns with argument arrays only — never a shell string.
pub struct SystemProber;

impl Prober for SystemProber {
    fn probe(&self, cmd: &str) -> bool {
        let program = if cfg!(windows) { "where.exe" } else { "which" };
        std::process::Command::new(program)
            .arg(cmd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }
}

/// Detect every registry row against `home` with `prober`.
pub fn detect_harnesses(home: &Path, prober: &dyn Prober) -> Vec<Detection> {
    crate::skill_federation::load_registry()
        .map(|contract| {
            contract
                .harnesses
                .into_iter()
                .map(|entry| {
                    detect_one(
                        home,
                        &entry.id,
                        &entry.detect,
                        &entry.detect_cmds,
                        match entry.tier.as_str() {
                            "A" => Tier::A,
                            "B" => Tier::B,
                            _ => Tier::C,
                        },
                        prober,
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn detect_one(
    home: &Path,
    id: &str,
    detect: &[String],
    detect_cmds: &[String],
    tier: Tier,
    prober: &dyn Prober,
) -> Detection {
    let mut evidence = Vec::new();

    let mut binary_found = false;
    for cmd in detect_cmds {
        if prober.probe(cmd) {
            binary_found = true;
            evidence.push(format!("binary `{cmd}` on PATH"));
            break;
        }
    }

    let mut config_dir = detect.iter().any(|marker| {
        let path = home.join(marker);
        path.is_dir() || path.is_file()
    });
    if config_dir {
        let marker = detect
            .iter()
            .find(|marker| {
                let path = home.join(marker);
                path.is_dir() || path.is_file()
            })
            .map(String::as_str)
            .unwrap_or("");
        evidence.push(format!("config ~/{marker}"));
    } else if id == "pi" {
        let relocated = super::paths::pi_agent_root(home);
        if relocated != home.join(".pi/agent") && (relocated.is_dir() || relocated.is_file()) {
            config_dir = true;
            evidence.push(format!("config {}", relocated.display()));
        }
    }

    Detection {
        id: id.to_string(),
        binary_found,
        config_dir,
        evidence,
        tier,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Scripted prober: only the listed commands "resolve".
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

    fn detection_for(home: &Path, id: &str, prober: &dyn Prober) -> Detection {
        detect_harnesses(home, prober)
            .into_iter()
            .find(|d| d.id == id)
            .expect("registry row")
    }

    #[test]
    fn binary_and_config_both_present() {
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".gemini")).expect("marker");
        let prober = StubProber::with(&["gemini"]);
        let det = detection_for(home.path(), "gemini", &prober);
        assert!(det.binary_found);
        assert!(det.config_dir);
        assert!(det.installed());
        assert_eq!(det.evidence_label(), "binary");
        assert!(det.evidence.iter().any(|e| e == "binary `gemini` on PATH"));
        assert!(det.evidence.iter().any(|e| e == "config ~/.gemini"));
    }

    #[test]
    fn config_only_is_leftover_not_binary() {
        // The complaint case: ~/.gemini exists without the gemini binary.
        let home = tempfile::tempdir().expect("home");
        std::fs::create_dir_all(home.path().join(".gemini")).expect("marker");
        let prober = StubProber::with(&[]);
        let det = detection_for(home.path(), "gemini", &prober);
        assert!(!det.binary_found, "config leftover must not read as binary");
        assert!(det.config_dir);
        assert!(det.installed(), "leftover still surfaces as installed");
        assert_eq!(det.evidence_label(), "config only, binary not found");
        assert!(det.evidence.iter().all(|e| !e.starts_with("binary")));
    }

    #[test]
    fn binary_only_without_config() {
        let home = tempfile::tempdir().expect("home");
        let prober = StubProber::with(&["claude"]);
        let det = detection_for(home.path(), "claude", &prober);
        assert!(det.binary_found);
        assert!(!det.config_dir);
        assert!(det.installed());
        assert_eq!(det.evidence_label(), "binary");
    }

    #[test]
    fn neither_present_is_not_installed() {
        let home = tempfile::tempdir().expect("home");
        let prober = StubProber::with(&[]);
        let det = detection_for(home.path(), "gemini", &prober);
        assert!(!det.binary_found);
        assert!(!det.config_dir);
        assert!(!det.installed());
        assert_eq!(det.evidence_label(), "not detected");
        assert!(det.evidence.is_empty());
    }

    #[test]
    fn any_of_multiple_cmds_counts() {
        // antigravity accepts either `antigravity` or `agy`.
        let home = tempfile::tempdir().expect("home");
        let prober = StubProber::with(&["agy"]);
        let det = detection_for(home.path(), "antigravity", &prober);
        assert!(det.binary_found);
        assert!(det.evidence.iter().any(|e| e == "binary `agy` on PATH"));
    }

    #[test]
    fn detection_covers_all_registry_rows() {
        let home = tempfile::tempdir().expect("home");
        let prober = StubProber::with(&[]);
        let dets = detect_harnesses(home.path(), &prober);
        let contract = crate::skill_federation::load_registry().expect("contract");
        assert_eq!(dets.len(), contract.harnesses.len());
        for entry in contract.harnesses {
            let det = dets.iter().find(|d| d.id == entry.id).expect("row");
            let expected = match entry.tier.as_str() {
                "A" => Tier::A,
                "B" => Tier::B,
                _ => Tier::C,
            };
            assert_eq!(det.tier, expected);
        }
    }

    #[test]
    fn system_prober_finds_sh_but_not_nonsense() {
        // Smoke test for the real prober: `sh` exists on every unix CI host;
        // a deliberately absurd name must not resolve.
        if cfg!(windows) {
            return;
        }
        let prober = SystemProber;
        assert!(prober.probe("sh"));
        assert!(!prober.probe("stateroot-definitely-not-a-real-binary-xyz"));
    }
}
