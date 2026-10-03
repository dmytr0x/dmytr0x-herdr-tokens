use herdr_tokens::{
    providers::Token,
    publisher::{Generation, Key, Pending, Publisher},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use tokio::time::Instant;
fn pending(workspace: &str, name: &str, value: &str) -> Pending {
    Pending {
        key: Key::new(workspace, name),
        generation: Generation {
            config: 1,
            directory: 1,
            connection: 1,
        },
        patch: BTreeMap::from([("token".into(), Token::Set(value.into()))]),
        completed: Instant::now(),
        interval_ms: 1000,
        ttl_ms: 3000,
    }
}
#[tokio::test(start_paused = true)]
async fn freshness_accepts_exact_interval_and_floors_fractional_ttl() {
    let mut publisher = Publisher::default();
    publisher.put(pending("w1", "a", "value"));
    tokio::time::advance(Duration::from_millis(1000)).await;
    assert_eq!(publisher.next(|_, _| true).0.unwrap().ttl_ms, 2000);
    publisher.put(pending("w1", "a", "value"));
    tokio::time::advance(Duration::from_micros(1500)).await;
    assert_eq!(publisher.next(|_, _| true).0.unwrap().ttl_ms, 2998);
}

#[tokio::test(start_paused = true)]
async fn unacknowledged_clear_expires_after_possible_delivery_deadline() {
    let mut publisher = Publisher::default();
    publisher.put(pending("w1", "a", "old"));
    let publication = publisher.next(|_, _| true).0.unwrap();
    publisher.sending(&publication);
    publisher.clear("w1", BTreeSet::from(["token".into()]));
    assert!(publisher.next(|_, _| true).0.unwrap().job.is_none());
    tokio::time::advance(
        Duration::from_millis(3000) + herdr_tokens::process::DELIVERY_BOUND
            - Duration::from_millis(100),
    )
    .await;
    assert!(publisher.next(|_, _| true).0.unwrap().job.is_none());
    publisher.put(pending("w1", "a", "new"));
    tokio::time::advance(Duration::from_millis(101)).await;
    assert!(publisher.next(|_, _| true).0.unwrap().job.is_some());
    assert!(!publisher.has_clears());
}

#[tokio::test(start_paused = true)]
async fn clear_without_known_values_is_attempted_and_removal_is_workspace_local() {
    let mut publisher = Publisher::default();
    publisher.clear("w1", BTreeSet::from(["old".into()]));
    publisher.clear("w1", BTreeSet::from(["another".into()]));
    publisher.put(pending("w1", "a", "new"));
    publisher.put(pending("w2", "a", "other"));
    let clear = publisher.next(|_, _| true).0.unwrap();
    assert_eq!(clear.patch.len(), 2);
    assert!(clear.patch.values().all(|token| *token == Token::Clear));
    publisher.remove_workspace("w1");
    assert!(publisher.pending_clears("w1").is_empty());
    assert!(!publisher.has_pending(&Key::new("w1", "a")));
    assert_eq!(publisher.next(|_, _| true).0.unwrap().workspace, "w2");
}

#[tokio::test(start_paused = true)]
async fn disconnect_discards_values_but_preserves_clear_obligations() {
    let mut publisher = Publisher::default();
    publisher.clear("w1", BTreeSet::from(["old".into()]));
    publisher.put(pending("w1", "a", "new"));
    publisher.discard_values();
    assert!(publisher.has_clears());
    assert!(!publisher.has_pending(&Key::new("w1", "a")));
    assert!(publisher.next(|_, _| true).0.unwrap().job.is_none());
}

#[tokio::test(start_paused = true)]
async fn latest_patch_fairness_freshness_and_remaining_ttl() {
    let mut p = Publisher::default();
    p.put(pending("w1", "a", "old"));
    p.put(pending("w1", "b", "b"));
    p.put(pending("w1", "a", "new"));
    tokio::time::advance(Duration::from_millis(200)).await;
    let (s, stale) = p.next(|_, _| true);
    assert!(stale.is_empty());
    let s = s.unwrap();
    assert_eq!(s.patch["token"], Token::Set("new".into()));
    assert_eq!(s.ttl_ms, 2800);
    assert_eq!(p.next(|_, _| true).0.unwrap().job.unwrap().0.collector, "b");
    p.put(pending("w1", "a", "stale"));
    tokio::time::advance(Duration::from_millis(1001)).await;
    let (s, stale) = p.next(|_, _| true);
    assert!(s.is_none());
    assert_eq!(stale, vec![Key::from(("w1".into(), "a".into()))]);
    p.put(pending("w1", "a", "fenced"));
    assert!(p.next(|_, _| false).0.is_none());
}
#[tokio::test(start_paused = true)]
async fn clear_barrier_chunks_retries_expiry_and_generation_fencing() {
    let mut p = Publisher::default();
    p.put(pending("w1", "a", "old"));
    let send = p.next(|_, _| true).0.unwrap();
    p.sending(&send);
    let keys: BTreeSet<String> = (0..32).map(|i| format!("t{i:02}")).collect();
    p.clear("w1", keys);
    p.put(pending("w1", "a", "new"));
    p.put(pending("w2", "a", "other"));
    let first = p.next(|_, _| true).0.unwrap();
    assert!(first.job.is_none());
    assert_eq!(first.patch.len(), 16);
    assert_eq!(p.next(|_, _| true).0.unwrap().workspace, "w2");
    assert!(p.next(|_, _| true).0.is_none());
    tokio::time::advance(Duration::from_secs(1)).await;
    let retry = p.next(|_, _| true).0.unwrap();
    assert_eq!(retry.patch, first.patch);
    p.acknowledged(&retry);
    let second = p.next(|_, _| true).0.unwrap();
    assert_eq!(second.patch.len(), 16);
    assert_ne!(second.patch, first.patch);
    p.acknowledged(&second);
    assert!(!p.has_clears());
    assert!(p.next(|_, _| true).0.unwrap().job.is_some());
}
