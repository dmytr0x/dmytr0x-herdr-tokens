//! One fair, bounded value queue and per-workspace clear barriers.
use crate::providers::{Patch, Token};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use tokio::time::Instant;
pub type Key = (String, String);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Generation {
    pub config: u64,
    pub directory: u64,
    pub connection: u64,
}
pub struct Pending {
    pub key: Key,
    pub generation: Generation,
    pub patch: Patch,
    pub completed: Instant,
    pub interval_ms: u64,
    pub ttl_ms: u64,
}
pub struct Publication {
    pub workspace: String,
    pub patch: Patch,
    pub ttl_ms: u64,
    pub job: Option<(Key, Generation)>,
    pub completed: Instant,
}
struct Barrier {
    keys: BTreeSet<String>,
    until: Instant,
    retry: Instant,
}
#[derive(Default)]
pub struct Publisher {
    pending: BTreeMap<Key, (u64, Pending)>,
    barriers: BTreeMap<String, Barrier>,
    // Upper bounds include ambiguous delivery, not just acknowledged reports.
    expiry: BTreeMap<String, Instant>,
    ticket: u64,
}
impl Publisher {
    pub fn put(&mut self, pending: Pending) {
        self.ticket += 1;
        let ticket = self
            .pending
            .get(&pending.key)
            .map_or(self.ticket, |(t, _)| *t);
        self.pending.insert(pending.key.clone(), (ticket, pending));
    }
    pub fn discard_values(&mut self) {
        self.pending.clear();
    }
    pub fn remove_workspace(&mut self, workspace: &str) {
        self.pending.retain(|(w, _), _| w != workspace);
        self.barriers.remove(workspace);
        self.expiry.remove(workspace);
    }
    pub fn clear(&mut self, workspace: &str, keys: BTreeSet<String>) {
        if keys.is_empty() {
            return;
        }
        self.pending.retain(|(w, _), _| w != workspace);
        let now = Instant::now();
        let until = self.expiry.get(workspace).copied().unwrap_or(now);
        let barrier = self.barriers.entry(workspace.into()).or_insert(Barrier {
            keys: BTreeSet::new(),
            until,
            retry: now,
        });
        barrier.keys.extend(keys);
        barrier.until = barrier.until.max(until);
        barrier.retry = now;
    }
    pub fn pending_clears(&self, workspace: &str) -> Vec<String> {
        self.barriers
            .get(workspace)
            .map_or_else(Vec::new, |b| b.keys.iter().cloned().collect())
    }
    pub fn has_pending(&self, key: &Key) -> bool {
        self.pending.contains_key(key)
    }
    pub fn has_clears(&self) -> bool {
        !self.barriers.is_empty()
    }
    /// Invalid generations are discarded; expired values are returned for recollection.
    pub fn next(
        &mut self,
        valid: impl Fn(&Key, Generation) -> bool,
    ) -> (Option<Publication>, Vec<Key>) {
        let now = Instant::now();
        // Try each required clear at least once, even if no known value remains.
        self.barriers
            .retain(|_, b| !(b.until <= now && b.retry > now));
        if let Some((workspace, b)) = self
            .barriers
            .iter_mut()
            .filter(|(_, b)| b.retry <= now)
            .min_by_key(|(_, b)| b.retry)
        {
            let patch = b
                .keys
                .iter()
                .take(16)
                .map(|k| (k.clone(), Token::Clear))
                .collect();
            b.retry = now + Duration::from_secs(1);
            return (
                Some(Publication {
                    workspace: workspace.clone(),
                    patch,
                    ttl_ms: 1,
                    job: None,
                    completed: now,
                }),
                vec![],
            );
        }
        let mut stale = Vec::new();
        self.pending.retain(|key, (_, p)| {
            if !valid(key, p.generation) {
                return false;
            }
            let elapsed = now.duration_since(p.completed);
            if elapsed > Duration::from_millis(p.interval_ms)
                || elapsed.as_millis() >= u128::from(p.ttl_ms)
            {
                stale.push(key.clone());
                return false;
            }
            true
        });
        let key = self
            .pending
            .iter()
            .filter(|((w, _), _)| !self.barriers.contains_key(w))
            .min_by_key(|(_, (ticket, _))| ticket)
            .map(|(k, _)| k.clone());
        let send = key.map(|key| {
            let (_, p) = self.pending.remove(&key).expect("selected pending patch");
            // floor(ttl - elapsed), not ttl - floor(elapsed).
            let remaining = Duration::from_millis(p.ttl_ms)
                .saturating_sub(now.duration_since(p.completed))
                .as_millis() as u64;
            Publication {
                workspace: key.0.clone(),
                patch: p.patch,
                ttl_ms: remaining,
                job: Some((key, p.generation)),
                completed: p.completed,
            }
        });
        (send, stale)
    }
    pub fn sending(&mut self, send: &Publication) {
        if send.job.is_some() {
            // Request execution can delay server acceptance by the full adapter deadline.
            let until =
                Instant::now() + Duration::from_millis(send.ttl_ms) + Duration::from_millis(1100);
            self.expiry
                .entry(send.workspace.clone())
                .and_modify(|t| *t = (*t).max(until))
                .or_insert(until);
        }
    }
    pub fn acknowledged(&mut self, send: &Publication) {
        if send.job.is_none()
            && let Some(b) = self.barriers.get_mut(&send.workspace)
        {
            for key in send.patch.keys() {
                b.keys.remove(key);
            }
            if b.keys.is_empty() {
                self.barriers.remove(&send.workspace);
            } else {
                b.retry = Instant::now();
            }
        }
    }
}
