use herdr_tokens::runtime::{Endpoint, Sequences};
use std::{fs, os::unix::fs::PermissionsExt};
#[test]
fn singleton_sequences_restart_corruption_and_endpoint_scope() {
    let t = tempfile::tempdir_in("/tmp").unwrap();
    let p = t.path().canonicalize().unwrap();
    let endpoint = Endpoint::new(&p.join("api.sock"), Some(&p.join("runtime"))).unwrap();
    let mut lock = endpoint.acquire().unwrap().unwrap();
    assert!(endpoint.acquire().unwrap().is_none());
    let state = p.join("state");
    fs::create_dir(&state).unwrap();
    let mut seq = Sequences::open(state.clone(), &mut lock).unwrap();
    assert_eq!(seq.allocate().unwrap(), 1);
    assert_eq!(seq.allocate().unwrap(), 2);
    drop(seq);
    let mut seq = Sequences::open(state.clone(), &mut lock).unwrap();
    assert_eq!(seq.allocate().unwrap(), 1025);
    for _ in 0..1023 {
        seq.allocate().unwrap();
    }
    assert_eq!(seq.allocate().unwrap(), 2049);
    drop(seq);
    drop(lock);
    assert!(endpoint.acquire().unwrap().is_some());
    let mut lock = endpoint.acquire().unwrap().unwrap();
    fs::write(state.join("sequence.toml"), "corrupt").unwrap();
    assert!(Sequences::open(state.clone(), &mut lock).is_err());
    fs::write(
        state.join("sequence.toml"),
        format!("schema_version=1\nreserved_through={}\n", u64::MAX),
    )
    .unwrap();
    assert!(Sequences::open(state, &mut lock).is_err());
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

#[test]
fn sequence_state_bounds_symlinks_and_persistence_failures() {
    let t = tempfile::tempdir_in("/tmp").unwrap();
    let root = t.path();
    let endpoint = Endpoint::new(&root.join("api"), Some(&root.join("r"))).unwrap();
    let mut lock = endpoint.acquire().unwrap().unwrap();
    let state = root.join("state");
    fs::create_dir(&state).unwrap();
    let path = state.join("sequence.toml");
    for text in [
        " ".repeat(4097),
        "schema_version=2\nreserved_through=1".into(),
        "schema_version=1\nreserved_through=-1".into(),
    ] {
        fs::write(&path, text).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Sequences::open(state.clone(), &mut lock).is_err());
    }
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(root.join("missing"), &path).unwrap();
    assert!(Sequences::open(state.clone(), &mut lock).is_err());
    fs::remove_file(&path).unwrap();
    let mut seq = Sequences::open(state.clone(), &mut lock).unwrap();
    for _ in 0..1024 {
        seq.allocate().unwrap();
    }
    // Isolated fixture only: prevent replacement without relying on uid permissions.
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(seq.allocate().is_err());
    assert!(seq.allocate().is_err());
}
