//! Pure reload and workspace decisions; the coordinator applies their effects.
use super::Workspace;
use crate::{config::Config, herdr::Discovery};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use tokio::time::Instant;

pub(super) struct WorkspaceDiff {
    pub added: BTreeSet<String>,
    pub removed: BTreeSet<String>,
    pub changed: BTreeSet<String>,
}
impl WorkspaceDiff {
    pub fn between(old: &BTreeMap<String, Workspace>, new: &Discovery) -> Self {
        Self {
            added: new
                .keys()
                .filter(|id| !old.contains_key(*id))
                .cloned()
                .collect(),
            removed: old
                .keys()
                .filter(|id| !new.contains_key(*id))
                .cloned()
                .collect(),
            changed: new
                .iter()
                .filter(|(id, dir)| {
                    old.get(*id)
                        .is_some_and(|w| w.directory.canonical != dir.canonical)
                })
                .map(|(id, _)| id.clone())
                .collect(),
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ReloadPlan {
    Unchanged,
    JobsOnly,
    Collections,
}
impl ReloadPlan {
    pub fn between(old: &Config, new: &Config) -> Self {
        if old == new {
            Self::Unchanged
        } else if old.same_collection_config(new) {
            Self::JobsOnly
        } else {
            Self::Collections
        }
    }
}
pub(super) struct Candidate {
    pub observed: String,
    pending: Option<(String, Instant)>,
}
impl Candidate {
    pub fn new(observed: String) -> Self {
        Self {
            observed,
            pending: None,
        }
    }
    pub fn committed(&mut self, observed: String) {
        self.observed = observed;
        self.pending = None;
    }
    /// True only after the same candidate has remained observable for the debounce window.
    pub fn ready(&mut self, fingerprint: String, now: Instant) -> bool {
        if fingerprint == self.observed {
            self.pending = None;
            return false;
        }
        if self.pending.as_ref().is_some_and(|(old, since)| {
            old == &fingerprint && now.duration_since(*since) >= Duration::from_millis(200)
        }) {
            self.committed(fingerprint);
            true
        } else {
            if self
                .pending
                .as_ref()
                .is_none_or(|(old, _)| old != &fingerprint)
            {
                self.pending = Some((fingerprint, now));
            }
            false
        }
    }
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn candidates_must_stabilize_and_reverts_clear_pending() {
        let mut c = Candidate::new("a".into());
        assert!(!c.ready("b".into(), Instant::now()));
        tokio::time::advance(Duration::from_millis(199)).await;
        assert!(!c.ready("b".into(), Instant::now()));
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(c.ready("b".into(), Instant::now()));
        assert!(!c.ready("c".into(), Instant::now()));
        assert!(!c.ready("b".into(), Instant::now()));
        assert!(!c.pending());
    }
}
