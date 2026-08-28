//! Host-path identity so Windows and WSL name the same project tree.
//!
//! Project identity is `.stateroot/manifest.json`'s `project_id` — that file
//! lives on the shared volume. Harness payloads and `projects.toml` still
//! carry host paths (`D:\foo` vs `/mnt/d/foo`, `\\?\` prefixes, slash vs
//! backslash). Those strings must compare equal when they name the same
//! DrvFs tree. Never store OS-absolute paths in project-scoped artifacts.

use std::path::{Path, PathBuf};

/// Stable registry / comparison key for a project directory.
///
/// Canonicalizes when the path exists, then folds Windows ↔ WSL forms into
/// `d:/rest` (forward slashes, lowercase drive letter, no verbatim prefix).
pub fn equivalent_project_key(dir: &Path) -> String {
    let canonical = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    normalize_host_path(&canonical.to_string_lossy())
}

/// Fold a path string into the Windows↔WSL identity form.
pub fn normalize_host_path(raw: &str) -> String {
    let mut s = raw.trim().replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_string();
    } else if let Some(rest) = s.strip_prefix("//./") {
        s = rest.to_string();
    }
    s = fold_wsl_unc(&s);
    s = fold_mnt_drive(&s);
    if s.len() >= 2 && s.as_bytes()[1] == b':' && s.as_bytes()[0].is_ascii_alphabetic() {
        let mut chars: Vec<char> = s.chars().collect();
        chars[0] = chars[0].to_ascii_lowercase();
        s = chars.into_iter().collect();
    }
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    s
}

/// True when two host paths name the same tree after Windows↔WSL folding.
pub fn host_paths_equivalent(a: &str, b: &str) -> bool {
    normalize_host_path(a) == normalize_host_path(b)
}

/// Resolve `raw` to a directory that exists on *this* host, translating
/// Windows drive paths to `/mnt/<letter>/…` (and the reverse) when needed.
pub fn resolve_existing_dir(raw: &Path) -> Option<PathBuf> {
    if raw.is_dir() {
        return Some(raw.to_path_buf());
    }
    let norm = normalize_host_path(&raw.to_string_lossy());
    native_candidates(&norm)
        .into_iter()
        .find(|path| path.is_dir())
}

fn fold_wsl_unc(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    for prefix in ["//wsl$/", "//wsl.localhost/"] {
        if lower.starts_with(prefix) {
            let after_prefix = &s[prefix.len()..];
            if let Some(slash) = after_prefix.find('/') {
                return after_prefix[slash..].to_string();
            }
        }
    }
    s.to_string()
}

/// `/mnt/<letter>/rest` → `<letter>:/rest`. Leaves `/mnt/docker/…` alone.
fn fold_mnt_drive(s: &str) -> String {
    let Some(after) = s.strip_prefix("/mnt/").or_else(|| s.strip_prefix("/MNT/")) else {
        return s.to_string();
    };
    let mut chars = after.chars();
    let Some(letter) = chars.next() else {
        return s.to_string();
    };
    if !letter.is_ascii_alphabetic() {
        return s.to_string();
    }
    let rest: String = chars.collect();
    if rest.is_empty() {
        return format!("{}:", letter.to_ascii_lowercase());
    }
    if !rest.starts_with('/') {
        return s.to_string();
    }
    format!("{}:{}", letter.to_ascii_lowercase(), rest)
}

fn native_candidates(norm: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if norm.len() >= 2 && norm.as_bytes()[1] == b':' && norm.as_bytes()[0].is_ascii_alphabetic() {
        let letter = &norm[..1];
        let rest = &norm[2..];
        out.push(PathBuf::from(format!(
            "{}:{}",
            letter.to_ascii_uppercase(),
            rest.replace('/', "\\")
        )));
        out.push(PathBuf::from(format!("{}:{rest}", letter)));
        out.push(PathBuf::from(format!("/mnt/{letter}{rest}")));
    } else if !norm.is_empty() {
        out.push(PathBuf::from(norm));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_drive_and_wsl_mnt_are_the_same_key() {
        assert!(host_paths_equivalent(
            r"D:\siderai\skillsAgent\stateroot",
            "/mnt/d/siderai/skillsAgent/stateroot",
        ));
        assert_eq!(
            normalize_host_path(r"\\?\D:\siderai\skillsAgent\stateroot"),
            normalize_host_path("/mnt/d/siderai/skillsAgent/stateroot"),
        );
        assert_eq!(
            normalize_host_path(r"\\?\D:\siderai\skillsAgent\stateroot"),
            "d:/siderai/skillsAgent/stateroot",
        );
    }

    #[test]
    fn wsl_unc_and_docker_mnt_are_not_confused() {
        assert!(host_paths_equivalent(
            r"\\wsl$\Ubuntu\mnt\d\work\demo",
            r"D:\work\demo",
        ));
        assert_eq!(
            normalize_host_path("/mnt/docker/volumes/x"),
            "/mnt/docker/volumes/x",
        );
    }

    #[test]
    fn trailing_slash_and_drive_case_fold() {
        assert!(host_paths_equivalent(r"D:\work\demo\", r"d:/work/demo"));
        assert!(host_paths_equivalent("/mnt/D/work/demo/", r"D:\work\demo"));
    }
}
