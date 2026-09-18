//! An append-only log of what this server did, for the user to read back.
//!
//! A log, not memory. Nothing here is consulted to decide anything, which is what keeps N9
//! ("the server holds no cross-session memory") true: the cache is still keyed by content
//! hash, and the same request against the same document still gets the same answer. What the
//! trace adds is the thing a harness has and an editor usually does not — a record of what
//! happened, that outlives the buffer it happened in.
//!
//! It lives where dismissals live, under the repository root's `.git/`, so it is greppable,
//! survives a restart, and never appears in `git status`.

use serde_json::Value;
use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};

/// `<root>/.git/meta/session.jsonl`.
pub fn path_for(root: &str) -> PathBuf {
    Path::new(root).join(".git").join("meta").join("session.jsonl")
}

/// Append one entry.
///
/// A single `O_APPEND` write of a line this size is atomic on Linux, so two processes sharing
/// a root interleave lines rather than characters, and a reader never sees half of one.
pub fn append(root: &str, entry: &Value) -> std::io::Result<()> {
    let path = path_for(root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{entry}")?;
    Ok(())
}

/// The last `limit` entries, oldest first.
///
/// A line that is not JSON is skipped rather than treated as an error: a half-written line
/// from a killed process is not a reason to lose the rest of the record.
pub fn tail(root: &str, limit: usize) -> Vec<Value> {
    let Ok(file) = std::fs::File::open(path_for(root)) else {
        return Vec::new();
    };
    let lines: Vec<String> = std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .collect();
    lines
        .iter()
        .rev()
        .take(limit)
        .rev()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn root(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("meta-trace-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.to_string_lossy().to_string()
    }

    #[test]
    fn entries_append_and_read_back_in_order() {
        let root = root("order");
        for i in 0..5 {
            append(&root, &json!({"n": i})).expect("append");
        }
        let all = tail(&root, 10);
        assert_eq!(all.len(), 5);
        assert_eq!(all[0]["n"], 0);
        assert_eq!(all[4]["n"], 4);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tail_keeps_the_newest_when_the_limit_is_smaller() {
        let root = root("tail");
        for i in 0..10 {
            append(&root, &json!({"n": i})).expect("append");
        }
        let last = tail(&root, 3);
        assert_eq!(last.len(), 3);
        // oldest first, still: the newest three, in the order they happened
        assert_eq!(last[0]["n"], 7);
        assert_eq!(last[2]["n"], 9);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_damaged_line_does_not_lose_the_rest() {
        let root = root("damaged");
        append(&root, &json!({"n": 1})).expect("append");
        let path = path_for(&root);
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).expect("open");
        writeln!(file, "{{ this is not json").expect("write");
        drop(file);
        append(&root, &json!({"n": 2})).expect("append");
        let all = tail(&root, 10);
        assert_eq!(all.len(), 2, "the good entries survive a half-written one");
        assert_eq!(all[1]["n"], 2);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_root_is_empty_rather_than_an_error() {
        let root = root("empty");
        assert!(tail(&root, 10).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
