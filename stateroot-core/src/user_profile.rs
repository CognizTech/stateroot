//! User-global profile store at `~/.stateroot/user/USER.md`.

use std::path::{Path, PathBuf};

use crate::local_store::now_rfc3339;

pub const USER_DIR: &str = ".stateroot/user";
pub const USER_FILE: &str = "USER.md";
pub const HISTORY_DIR: &str = "history";

#[derive(Debug, thiserror::Error)]
pub enum UserProfileError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn path(home: &Path) -> PathBuf {
    home.join(USER_DIR).join(USER_FILE)
}

pub fn read(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path(home)).ok()?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Return the payload after a single known StateRoot provenance header.
/// Unknown comments remain content so comparisons stay conservative.
pub fn known_header_payload(content: &str) -> &str {
    let trimmed = content.trim();
    let Some((first, rest)) = trimmed.split_once('\n') else {
        return trimmed;
    };
    let first = first.trim();
    if (first.starts_with("<!-- stateroot:user ") || first.starts_with("<!-- imported from "))
        && first.ends_with("-->")
    {
        rest.trim()
    } else {
        trimmed
    }
}

/// Compare profiles while ignoring one recognized StateRoot provenance line.
pub fn payloads_equal(left: &str, right: &str) -> bool {
    known_header_payload(left) == known_header_payload(right)
}

/// Write a profile and snapshot a prior non-empty version first.
pub fn write(home: &Path, content: &str, origin: Option<&str>) -> Result<String, UserProfileError> {
    let dir = home.join(USER_DIR);
    std::fs::create_dir_all(&dir)?;
    let path = path(home);
    let mut snapshot = String::new();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if payloads_equal(&existing, content) {
            return Ok(format!("user profile unchanged at {}", path.display()));
        }
        if !existing.trim().is_empty() {
            let history = dir.join(HISTORY_DIR);
            std::fs::create_dir_all(&history)?;
            let stamp = now_rfc3339().replace([':', '-'], "");
            let snap = unique_path(&history, &format!("{stamp}.md"));
            std::fs::write(&snap, existing)?;
            snapshot = format!(" (previous version → {})", snap.display());
        }
    }
    let mut body = content.trim().to_string();
    if let Some(origin) = origin {
        if !body.contains("<!-- stateroot:user") && !body.contains("<!-- imported from") {
            body = format!(
                "<!-- stateroot:user origin={}; at={} -->\n{}",
                origin.replace([';', '\n'], " "),
                now_rfc3339(),
                body
            );
        }
    }
    std::fs::write(&path, format!("{body}\n"))?;
    Ok(format!(
        "user profile written to {}{snapshot}",
        path.display()
    ))
}

/// Preserve a conflicting project profile as an import candidate.
pub fn write_import_candidate(
    home: &Path,
    content: &str,
    origin: &str,
) -> Result<PathBuf, UserProfileError> {
    let dir = home.join(USER_DIR).join(HISTORY_DIR);
    std::fs::create_dir_all(&dir)?;
    let stamp = now_rfc3339().replace([':', '-'], "");
    let path = unique_path(&dir, &format!("{stamp}-import-candidate.md"));
    let body = format!(
        "<!-- stateroot:user import-candidate origin={}; at={} -->\n{}\n",
        origin.replace([';', '\n'], " "),
        now_rfc3339(),
        content.trim()
    );
    std::fs::write(&path, body)?;
    Ok(path)
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let base = dir.join(name);
    if !base.exists() {
        return base;
    }
    let stem = name.strip_suffix(".md").unwrap_or(name);
    for n in 1.. {
        let candidate = dir.join(format!("{stem}-{n}.md"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_provenance_is_ignored_conservatively() {
        let stored = "<!-- stateroot:user origin=openclaw; at=now -->\n# User\n\nLin\n";
        assert!(payloads_equal(stored, "# User\n\nLin"));
        assert!(!payloads_equal(
            "<!-- foreign header -->\n# User\n\nLin",
            "# User\n\nLin"
        ));
    }

    #[test]
    fn identical_payload_write_does_not_rewrite_or_snapshot() {
        let home = tempfile::tempdir().unwrap();
        write(home.path(), "Human Lin", Some("first")).unwrap();
        let path = path(home.path());
        let before = std::fs::read_to_string(&path).unwrap();
        let note = write(home.path(), "Human Lin", Some("second")).unwrap();
        assert!(note.contains("unchanged"));
        assert_eq!(std::fs::read_to_string(path).unwrap(), before);
        assert!(!home.path().join(USER_DIR).join(HISTORY_DIR).exists());
    }
}
