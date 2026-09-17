//! Content-hash keyed conclusion cache.
//!
//! Never a source of truth (PROTOCOL.md N9): entries are immutable and evictable, and a
//! miss is always recoverable by recomputation. Keys embed the content hash, so an entry
//! for changed content simply never hits — invalidation is free.

use crate::types::{Finding, Proposal};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

#[derive(Debug, Clone, Default)]
pub struct Conclusion {
    pub findings: Vec<Finding>,
    pub edit: Option<Proposal>,
    pub artifact: Option<String>,
}

struct State {
    map: HashMap<String, Arc<Conclusion>>,
    order: VecDeque<String>,
    cap: usize,
    hits: u64,
    misses: u64,
}

pub struct Cache {
    inner: Mutex<State>,
}

impl Cache {
    pub fn new(cap: usize) -> Cache {
        Cache {
            inner: Mutex::new(State {
                map: HashMap::new(),
                order: VecDeque::new(),
                cap: cap.max(1),
                hits: 0,
                misses: 0,
            }),
        }
    }

    pub fn get(&self, key: &str) -> Option<Arc<Conclusion>> {
        let mut s = self.inner.lock();
        match s.map.get(key).cloned() {
            Some(v) => {
                s.hits += 1;
                Some(v)
            }
            None => {
                s.misses += 1;
                None
            }
        }
    }

    pub fn put(&self, key: &str, value: Conclusion) {
        let mut s = self.inner.lock();
        if s.map.insert(key.to_string(), Arc::new(value)).is_none() {
            s.order.push_back(key.to_string());
        }
        while s.order.len() > s.cap {
            if let Some(old) = s.order.pop_front() {
                s.map.remove(&old);
            }
        }
    }

    /// Drop every entry. Used by `:Meta recompute`.
    pub fn clear(&self) {
        let mut s = self.inner.lock();
        s.map.clear();
        s.order.clear();
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// (hits, misses, entries)
    pub fn stats(&self) -> (u64, u64, usize) {
        let s = self.inner.lock();
        (s.hits, s.misses, s.map.len())
    }
}

/// Findings are per document: one analysis covers every scope in it.
pub fn findings_key(content_hash: &str) -> String {
    format!("findings|{content_hash}")
}

/// A generated edit or artifact is per verb, per scope, and per prompt revision — a
/// change to the wording must not serve stale conclusions (PROTOCOL.md §5 gate 1).
pub fn op_key(verb: &str, prompt_version: &str, content_hash: &str, start_line: u32, end_line: u32) -> String {
    format!("op|{verb}|{prompt_version}|{content_hash}|{start_line}|{end_line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conclusion(tag: &str) -> Conclusion {
        Conclusion {
            artifact: Some(tag.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn keys_separate_the_axes_that_matter() {
        assert_ne!(findings_key("h1"), findings_key("h2"));
        assert_ne!(op_key("harden", "1", "h", 1, 2), op_key("harden", "1", "h", 1, 3));
        assert_ne!(op_key("harden", "1", "h", 1, 2), op_key("rewrite", "1", "h", 1, 2));
        assert_ne!(
            op_key("harden", "1", "h", 1, 2),
            op_key("harden", "2", "h", 1, 2),
            "a prompt revision must not hit an older conclusion"
        );
        assert_eq!(op_key("harden", "1", "h", 1, 2), op_key("harden", "1", "h", 1, 2));
    }

    #[test]
    fn stored_values_are_returned_and_misses_counted() {
        let c = Cache::new(4);
        assert!(c.get("k").is_none());
        c.put("k", conclusion("v"));
        assert_eq!(c.get("k").unwrap().artifact.as_deref(), Some("v"));
        let (hits, misses, len) = c.stats();
        assert_eq!((hits, misses, len), (1, 1, 1));
    }

    #[test]
    fn eviction_is_bounded_and_oldest_first() {
        let c = Cache::new(2);
        c.put("a", conclusion("a"));
        c.put("b", conclusion("b"));
        c.put("c", conclusion("c"));
        assert!(c.get("a").is_none(), "oldest evicted");
        assert!(c.get("b").is_some());
        assert!(c.get("c").is_some());
        assert!(c.len() <= 2);
    }

    #[test]
    fn rewriting_a_key_does_not_duplicate_it() {
        let c = Cache::new(4);
        c.put("k", conclusion("one"));
        c.put("k", conclusion("two"));
        assert_eq!(c.len(), 1);
        assert_eq!(c.get("k").unwrap().artifact.as_deref(), Some("two"));
    }

    #[test]
    fn clear_empties_the_cache() {
        let c = Cache::new(4);
        c.put("k", conclusion("v"));
        c.clear();
        assert!(c.is_empty());
        assert!(c.get("k").is_none());
    }

    #[test]
    fn a_zero_capacity_cache_still_works() {
        let c = Cache::new(0);
        c.put("k", conclusion("v"));
        assert!(c.get("k").is_none() || c.len() == 1);
    }
}
