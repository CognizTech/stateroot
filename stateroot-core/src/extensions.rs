//! Git-style extension discovery: any executable named `stateroot-<name>` on
//! PATH becomes `stateroot <name> [args…]`.
//!
//! Extensions are processes that read/write the same `.stateroot/` stores and
//! speak text on stdout — string-native, per doctrine. There is no registry
//! or manifest: discovery is a PATH scan, the user manages the files.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

/// One discovered extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    /// Command name — the suffix after `stateroot-`, lowercased, hyphens
    /// preserved (a Windows PATHEXT extension is not part of the name).
    pub name: String,
    /// Executable path (first PATH hit wins on duplicate names).
    pub path: PathBuf,
    /// Set by the caller comparing against the builtin subcommand names — a
    /// shadowed builtin is never dispatched to the extension.
    pub shadowed_builtin: bool,
}

/// Parse the extension name from a candidate file name. `pathext` carries the
/// executable extensions (lowercased, dot-prefixed) on Windows; pass an empty
/// slice where the exec bit rules (unix). The bare `stateroot` binary never
/// parses — it lacks the `-<name>` suffix by construction.
fn extension_name(file_name: &str, pathext: &[String]) -> Option<String> {
    let lower = file_name.to_ascii_lowercase();
    let mut name = lower.strip_prefix("stateroot-")?;
    if let Some((stem, ext)) = name.rsplit_once('.') {
        if pathext.iter().any(|p| p.trim_start_matches('.') == ext) {
            name = stem;
        }
    }
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Executable test: any exec bit on unix, a PATHEXT extension on Windows.
#[cfg(unix)]
fn is_executable(path: &std::path::Path, _pathext: &[String]) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Executable test: any exec bit on unix, a PATHEXT extension on Windows.
#[cfg(windows)]
fn is_executable(path: &std::path::Path, pathext: &[String]) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| {
                pathext
                    .iter()
                    .any(|p| p.trim_start_matches('.').eq_ignore_ascii_case(e))
            })
            .unwrap_or(false)
}

/// Executable test: any exec bit on unix, a PATHEXT extension on Windows.
#[cfg(not(any(unix, windows)))]
fn is_executable(path: &std::path::Path, _pathext: &[String]) -> bool {
    path.is_file()
}

/// PATHEXT entries (lowercased, dot-prefixed) where they exist; empty on
/// unix, where executable-ness is the mode bit and names keep their suffix.
fn pathext() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .map(|s| format!(".{}", s.trim_start_matches('.')))
        .collect()
}

/// Scan `dirs` (in PATH order) for extension executables. Duplicate names
/// dedup first-PATH-hit-wins; output is sorted by name for stable display.
fn discover_in(dirs: &[PathBuf]) -> Vec<Extension> {
    let pathext = pathext();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = entry
                .file_name()
                .to_str()
                .and_then(|f| extension_name(f, &pathext))
            else {
                continue;
            };
            if seen.contains(&name) || !is_executable(&path, &pathext) {
                continue;
            }
            seen.insert(name.clone());
            out.push(Extension {
                name,
                path,
                shadowed_builtin: false,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Discover every `stateroot-<name>` executable on PATH.
pub fn discover() -> Vec<Extension> {
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    discover_in(&dirs)
}

/// Resolve one extension by command name.
pub fn resolve(name: &str) -> Option<PathBuf> {
    let name = name.to_ascii_lowercase();
    discover()
        .into_iter()
        .find(|e| e.name == name)
        .map(|e| e.path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_pathext() -> Vec<String> {
        Vec::new()
    }

    #[test]
    fn name_parsing_takes_the_suffix_after_stateroot_dash() {
        let pe = no_pathext();
        assert_eq!(
            extension_name("stateroot-hello", &pe).as_deref(),
            Some("hello")
        );
        assert_eq!(
            extension_name("stateroot-my-cmd", &pe).as_deref(),
            Some("my-cmd"),
            "hyphens are preserved"
        );
        assert_eq!(
            extension_name("STATEROOT-HELLO", &pe).as_deref(),
            Some("hello"),
            "names lowercase"
        );
        // The bare binary and an empty suffix never parse.
        assert_eq!(extension_name("stateroot", &pe), None);
        assert_eq!(extension_name("stateroot.exe", &pe), None);
        assert_eq!(extension_name("stateroot-", &pe), None);
        assert_eq!(extension_name("unrelated", &pe), None);
        // Without PATHEXT the suffix is kept whole (git-style `git-foo.sh`).
        assert_eq!(
            extension_name("stateroot-foo.sh", &pe).as_deref(),
            Some("foo.sh")
        );
    }

    #[test]
    fn name_parsing_strips_a_pathext_extension() {
        let pe = vec![".exe".to_string(), ".bat".to_string()];
        assert_eq!(
            extension_name("stateroot-foo.EXE", &pe).as_deref(),
            Some("foo")
        );
        assert_eq!(
            extension_name("stateroot-foo.txt", &pe).as_deref(),
            Some("foo.txt"),
            "non-executable extensions stay part of the name"
        );
        assert_eq!(extension_name("stateroot-.exe", &pe), None);
    }

    #[cfg(unix)]
    #[test]
    fn discover_in_finds_executables_first_path_hit_wins() {
        use std::os::unix::fs::PermissionsExt;
        let dir_a = tempfile::tempdir().expect("dir a");
        let dir_b = tempfile::tempdir().expect("dir b");
        let write = |dir: &std::path::Path, name: &str, mode: u32| {
            let path = dir.join(name);
            fs::write(&path, "#!/bin/sh\n").expect("write");
            fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
            path
        };
        let a_first = write(dir_a.path(), "stateroot-alpha", 0o755);
        let shadowed = write(dir_b.path(), "stateroot-alpha", 0o755);
        write(dir_a.path(), "stateroot-not-exec", 0o644);
        let self_bin = write(dir_a.path(), "stateroot", 0o755);
        let subdir = dir_a.path().join("stateroot-dir");
        fs::create_dir(&subdir).expect("subdir");

        let found = discover_in(&[dir_a.path().to_path_buf(), dir_b.path().to_path_buf()]);
        assert_eq!(found.len(), 1, "only alpha survives: {found:?}");
        assert_eq!(found[0].name, "alpha");
        assert_eq!(
            found[0].path, a_first,
            "first PATH hit wins over {shadowed:?}"
        );
        assert!(!found[0].shadowed_builtin);
        assert_ne!(found[0].path, self_bin, "the bare binary is excluded");

        // An empty PATH discovers nothing.
        assert!(discover_in(&[dir_b.path().join("missing").to_path_buf()]).is_empty());
    }
}
