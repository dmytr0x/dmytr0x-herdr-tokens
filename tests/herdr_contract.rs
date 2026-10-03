use herdr_tokens::{
    config::{Config, Runtime},
    herdr,
};
use serde_json::json;
use std::{collections::BTreeMap, fs};
fn config() -> Config {
    Config {
        runtime: Runtime::default(),
        workspace_dirs: BTreeMap::new(),
        collectors: vec![],
        jobs: vec![],
    }
}
#[test]
fn captured_empty_list_envelopes() {
    let d = herdr::decode(
        include_bytes!("fixtures/workspaces.json"),
        include_bytes!("fixtures/panes.json"),
        &config(),
    )
    .unwrap();
    assert!(d.is_empty());
    assert!(herdr::decode(b"{}", include_bytes!("fixtures/panes.json"), &config()).is_err());
}
#[test]
fn directory_priority_ambiguity_relative_and_partial_snapshots() {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    let a = root.join("a");
    let b = root.join("b");
    fs::create_dir(&a).unwrap();
    fs::create_dir(&b).unwrap();
    let w = json!({"id":"test","result":{"workspaces":[{"workspace_id":"w1","cwd":a}]}});
    let panes = |dirs: Vec<String>| {
        json!({"id":"test","result":{"panes":dirs.iter().map(|d|json!({"workspace_id":"w1","cwd":d,"foreground_cwd":b})).collect::<Vec<_>>()}}).to_string()
    };
    let decode = |w: &serde_json::Value, p: &str, c: &Config| {
        herdr::decode(w.to_string().as_bytes(), p.as_bytes(), c)
    };
    let one = panes(vec![a.display().to_string()]);
    let two = panes(vec![a.display().to_string(), b.display().to_string()]);
    assert_eq!(
        decode(&w, &one, &config()).unwrap()["w1"].canonical,
        Some(a.clone())
    );
    assert!(
        decode(&w, &two, &config()).unwrap()["w1"]
            .canonical
            .is_none()
    );
    assert!(
        decode(&w, &panes(vec!["relative".into()]), &config()).unwrap()["w1"]
            .canonical
            .is_none()
    );
    assert!(decode(&w, "{}", &config()).is_err());
    let mut c = config();
    c.workspace_dirs.insert("w1".into(), a.clone());
    assert_eq!(decode(&w, &two, &c).unwrap()["w1"].canonical, Some(a));
    c.workspace_dirs.insert("w1".into(), root.join("offline"));
    assert!(decode(&w, &one, &c).unwrap()["w1"].canonical.is_none());
    let wt = json!({"id":"test","result":{"workspaces":[{"workspace_id":"w1","worktree":{"checkout_path":b}}]}});
    assert_eq!(
        decode(&wt, &one, &config()).unwrap()["w1"].canonical,
        Some(b)
    );
    let too_many = json!({"id":"test","result":{"workspaces":(0..257).map(|i|json!({"workspace_id":i.to_string()})).collect::<Vec<_>>()}});
    assert!(decode(&too_many, &one, &config()).is_err());
}
