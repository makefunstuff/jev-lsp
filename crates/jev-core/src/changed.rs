//! Which files git has seen change, for the one pass that will not re-inspect what nobody
//! touched.
//!
//! A rules pass on every save would re-inspect the whole document every time; the inspections are
//! cheap, but the *decision* is not free and answering the same question twice is waste. So the
//! document is looked at only when git reports it changed — `git diff --name-only HEAD` union
//! `git status --porcelain`, which covers both an edit to a tracked file and a file that is new.
//!
//! It lives in `jev-core` because both front ends need the same answer: the server for its ambient
//! pass, and `jev inspect` for its `--force` flag. Two implementations would be two different
//! definitions of "changed", and the CLI and the server would disagree about what they are for.
//!
//! Nothing here is fatal. A directory that is not a repository, a repository with no commits yet,
//! or a machine with no git is an `Err`, and the caller treats that as *every* document being
//! changed: a pass that never runs outside a repository is a feature that silently does nothing,
//! and one that runs is merely a little more expensive than it had to be.

use std::collections::HashSet;

/// The paths git reports as changed, absolute, or the reason that question could not be
/// answered.
pub fn changed_paths(root: &str) -> Result<HashSet<String>, String> {
    let base = root.trim_end_matches('/');
    let mut changed = HashSet::new();
    for args in [
        ["diff", "--name-only", "HEAD"].as_slice(),
        ["status", "--porcelain"].as_slice(),
    ] {
        let label = args.join(" ");
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args.iter())
            .output()
            .map_err(|e| format!("git {label} could not run: {e}"))?;
        if !output.status.success() {
            let why = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(format!("git {label} failed: {why}"));
        }
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let rel = if args[0] == "diff" {
                line.trim().to_string()
            } else {
                // `status --porcelain` is `XY path`, and a rename is `XY old -> new`. The new
                // name is the one that exists.
                let rest = line.get(3..).unwrap_or("").trim();
                rest.rsplit(" -> ").next().unwrap_or(rest).trim().to_string()
            };
            if !rel.is_empty() {
                changed.insert(format!("{base}/{rel}"));
            }
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn repo(tag: &str) -> Option<PathBuf> {
        let root = std::env::temp_dir().join(format!("jev-changed-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).ok()?;
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        let ready = run(&["init", "-q"])
            && run(&["config", "user.email", "test@example.invalid"])
            && run(&["config", "user.name", "test"]);
        if !ready {
            return None;
        }
        std::fs::write(root.join("tracked.txt"), "one\n").ok()?;
        if !run(&["add", "-A"]) || !run(&["-c", "commit.gpgsign=false", "commit", "-q", "-m", "x"]) {
            return None;
        }
        Some(root)
    }

    fn path_of(root: &Path, name: &str) -> String {
        format!("{}/{}", root.display().to_string().trim_end_matches('/'), name)
    }

    #[test]
    fn a_clean_checkout_reports_nothing_and_an_edit_reports_its_file() {
        let Some(root) = repo("edit") else {
            return; // no git on this machine
        };
        let root_str = root.display().to_string();
        let clean = changed_paths(&root_str).unwrap();
        assert!(
            !clean.contains(&path_of(&root, "tracked.txt")),
            "a committed, untouched file is not changed: {clean:?}"
        );

        std::fs::write(root.join("tracked.txt"), "two\n").unwrap();
        let edited = changed_paths(&root_str).unwrap();
        assert!(edited.contains(&path_of(&root, "tracked.txt")), "{edited:?}");

        // An untracked file is a change too: it is about to be looked at for the first time.
        std::fs::write(root.join("new.rs"), "fn a() {}\n").unwrap();
        let created = changed_paths(&root_str).unwrap();
        assert!(created.contains(&path_of(&root, "new.rs")), "{created:?}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_directory_that_is_not_a_repository_is_an_error_the_caller_can_act_on() {
        let root = std::env::temp_dir().join(format!("jev-changed-{}-plain", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let err = changed_paths(&root.display().to_string()).unwrap_err();
        assert!(!err.is_empty(), "and the reason travels with the failure");
        std::fs::remove_dir_all(&root).ok();
    }
}
