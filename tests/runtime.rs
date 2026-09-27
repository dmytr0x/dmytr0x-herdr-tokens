use herdr_tokens::runtime::{Endpoint, Sequences};
use std::{fs, os::unix::fs::PermissionsExt};
#[test]
fn singleton_sequences_restart_corruption_and_endpoint_scope() {
    let t = tempfile::tempdir_in("/tmp").unwrap();
    let p = t.path().canonicalize().unwrap();
    let endpoint = Endpoint::new(&p.join("api.sock"), Some(&p.join("runtime"))).unwrap();
    let lock = endpoint.acquire().unwrap().unwrap();
    assert!(endpoint.acquire().unwrap().is_none());
    let state = p.join("state");
    fs::create_dir(&state).unwrap();
    let mut seq = Sequences::open(state.clone(), &lock).unwrap();
    assert_eq!(seq.allocate().unwrap(), 1);
    assert_eq!(seq.allocate().unwrap(), 2);
    drop(seq);
    let mut seq = Sequences::open(state.clone(), &lock).unwrap();
    assert_eq!(seq.allocate().unwrap(), 1025);
    for _ in 0..1023 {
        seq.allocate().unwrap();
    }
    assert_eq!(seq.allocate().unwrap(), 2049);
    drop(seq);
    drop(lock);
    assert!(endpoint.acquire().unwrap().is_some());
    let lock = endpoint.acquire().unwrap().unwrap();
    fs::write(state.join("sequence.toml"), "corrupt").unwrap();
    assert!(Sequences::open(state.clone(), &lock).is_err());
    fs::write(
        state.join("sequence.toml"),
        format!("schema_version=1\nreserved_through={}\n", u64::MAX),
    )
    .unwrap();
    assert!(Sequences::open(state, &lock).is_err());
    let other = Endpoint::new(&p.join("other.sock"), Some(&p.join("runtime"))).unwrap();
    assert_ne!(endpoint.hash, other.hash);
    assert!(other.acquire().unwrap().is_some());
}
#[test]
fn herdr_created_state_root_may_be_readable_but_managed_state_is_private() {
    let t = tempfile::tempdir_in("/tmp").unwrap();
    let p = t.path().canonicalize().unwrap();
    let e = Endpoint::new(&p.join("api"), Some(&p.join("runtime"))).unwrap();
    let root = p.join("state");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    let identity = e.identity(p.join("config"), root.clone());
    let managed = herdr_tokens::runtime::state_dir(&identity).unwrap();
    assert_eq!(
        fs::metadata(&managed).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o755
    );
    fs::set_permissions(&root, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(herdr_tokens::runtime::state_dir(&identity).is_err());
}
#[test]
fn private_runtime_and_symlink_rejection() {
    let t = tempfile::tempdir_in("/tmp").unwrap();
    let p = t.path().canonicalize().unwrap();
    let r = p.join("runtime");
    fs::create_dir(&r).unwrap();
    fs::set_permissions(&r, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Endpoint::new(&p.join("api"), Some(&r)).is_err());
    fs::set_permissions(&r, fs::Permissions::from_mode(0o700)).unwrap();
    let link = p.join("link");
    std::os::unix::fs::symlink(&r, &link).unwrap();
    assert!(Endpoint::new(&p.join("api"), Some(&link)).is_err());
    let e = Endpoint::new(&p.join("api"), Some(&r)).unwrap();
    let target = p.join("target");
    fs::write(&target, "").unwrap();
    std::os::unix::fs::symlink(&target, r.join(format!("{}.lock", &e.hash[..32]))).unwrap();
    assert!(e.acquire().is_err());
}
