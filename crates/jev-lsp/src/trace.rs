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

/// `<root>/.git/jev/session.jsonl`.
pub fn path_for(root: &str) -> PathBuf {
    Path::new(root).join(".git").join("jev").join("session.jsonl")
}

/// How large the record is allowed to get before the oldest lines are dropped.
///
/// A log with no bound is a disk filling up slowly. Found the hard way: a day of testing
/// polled `jev.status` in a loop and left ninety-five megabytes of it behind. One megabyte is
/// a few thousand entries — more than anyone reads back — and staying small is what keeps the
/// session buffer instant.
const MAX_BYTES: u64 = 1024 * 1024;

/// What to keep when the record is trimmed: the newest lines, this many bytes' worth.
const KEEP_BYTES: usize = 256 * 1024;

/// Append one entry, trimming the record if it has grown past its bound.
///
/// A single `O_APPEND` write of a line this size is atomic on Linux, so two processes sharing
/// a root interleave lines rather than characters. A line can still be torn if the process is
/// killed mid-write, which is why `tail` skips what it cannot parse.
pub fn append(root: &str, entry: &Value) -> std::io::Result<()> {
    let path = path_for(root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path.clone())?;
    writeln!(file, "{entry}")?;
    drop(file);

    // Checked after the write rather than before: the size is only interesting once it has
    // grown, and a `stat` per append is cheaper than a trim per append.
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > MAX_BYTES {
        trim(&path);
    }
    Ok(())
}

/// Keep the newest complete lines, in place.
fn trim(path: &std::path::Path) {
    let Ok(raw) = std::fs::read(path) else {
        return;
    };
    let keep_from = raw.len().saturating_sub(KEEP_BYTES);
    // Start at a line boundary: a tail that begins mid-line is not an entry.
    let start = raw[keep_from..]
        .iter()
        .position(|b| *b == b'\n')
        .map(|offset| keep_from + offset + 1)
        .unwrap_or(keep_from);
    if start >= raw.len() {
        return;
    }
    let _ = std::fs::write(path, &raw[start..]);
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
        let dir = std::env::temp_dir().join(format!("jev-trace-{name}-{}", std::process::id()));
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
    fn the_record_is_bounded_and_keeps_the_newest() {
        let root = root("bounded");
        // Enough to pass the bound, each entry big enough that this stays quick.
        let filler = "x".repeat(4096);
        for i in 0..400 {
            append(&root, &json!({"n": i, "pad": filler})).expect("append");
        }
        let path = path_for(&root);
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        assert!(
            size <= MAX_BYTES + 8192,
            "the record does not grow without bound: {size} bytes"
        );
        let kept = tail(&root, 10_000);
        assert!(!kept.is_empty(), "and it still reads back");
        assert_eq!(
            kept.last().and_then(|e| e["n"].as_u64()),
            Some(399),
            "the newest entry is the one still there"
        );
        assert!(
            kept.iter().all(|e| e.get("n").is_some()),
            "every line that survives is a whole entry"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_root_is_empty_rather_than_an_error() {
        let root = root("empty");
        assert!(tail(&root, 10).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
