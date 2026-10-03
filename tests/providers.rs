mod support;
use herdr_tokens::{
    config::{CommandOutput, Config, Provider, TokenMapping},
    providers::{self, Token},
};
use std::{collections::BTreeMap, fs, path::Path, process::Command};
use support::git;
use tokio_util::sync::CancellationToken;
// Mutable wire fixture; each invocation crosses the real validation boundary.
#[derive(serde::Serialize)]
struct Collector {
    name: String,
    provider: Provider,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<CommandOutput>,
    interval_ms: u64,
    timeout_ms: u64,
    ttl_ms: u64,
    tokens: BTreeMap<String, TokenMapping>,
}
async fn collect(
    c: &Collector,
    workspace: &str,
    cwd: &Path,
    cancel: CancellationToken,
) -> Result<providers::Collected, providers::Error> {
    #[derive(serde::Serialize)]
    struct Wire<'a> {
        schema_version: u32,
        collectors: Vec<&'a Collector>,
    }
    let text = toml::to_string(&Wire {
        schema_version: 1,
        collectors: vec![c],
    })
    .unwrap();
    let config = Config::from_toml(&text).unwrap();
    providers::collect(&config.collectors()[0], workspace, cwd, cancel).await
}
fn collector(script: String) -> Collector {
    Collector {
        name: "test".into(),
        provider: Provider::Command,
        command: vec!["/bin/sh".into(), "-c".into(), script],
        output: Some(CommandOutput::Json),
        interval_ms: 1000,
        timeout_ms: 1000,
        ttl_ms: 3000,
        tokens: BTreeMap::from([("token".into(), "status".into())]),
    }
}
async fn command(output: &str) -> Result<providers::Collected, providers::Error> {
    collect(
        &collector(format!("printf '%s' '{}'", output.replace('\'', "'\"'\"'"))),
        "w1",
        Path::new("/"),
        CancellationToken::new(),
    )
    .await
}
#[tokio::test]
async fn command_text_output_publishes_date_stdout() {
    let mut c = collector(String::new());
    c.command = vec!["date".into(), "+date-marker".into()];
    c.output = Some(CommandOutput::Text);
    c.tokens = BTreeMap::from([("current_date".into(), "stdout".into())]);

    let out = collect(&c, "w1", Path::new("/"), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(out.patch["current_date"], Token::Set("date-marker".into()));
    assert!(!out.truncated);
}

#[tokio::test]
async fn command_text_output_strips_ansi_sequences() {
    let mut c = collector("printf '\\033[31mred\\033[0m\\n'".into());
    c.output = Some(CommandOutput::Text);
    c.tokens = BTreeMap::from([("color".into(), "stdout".into())]);

    let out = collect(&c, "w1", Path::new("/"), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(out.patch["color"], Token::Set("red".into()));
}

#[tokio::test]
async fn decorated_command_values_preserve_clears_and_unicode_limits() {
    for (value, expected) in [
        (serde_json::json!(0), Token::Set("[!0]".into())),
        (serde_json::json!(true), Token::Set("[!true]".into())),
        (serde_json::json!("  done  "), Token::Set("[!done]".into())),
        (serde_json::Value::Null, Token::Clear),
        (serde_json::json!(" \n\u{0007} "), Token::Clear),
    ] {
        let output = serde_json::json!({"status":value}).to_string();
        let mut c = collector(format!("printf '%s' '{output}'"));
        let mapping = c.tokens.get_mut("token").unwrap();
        mapping.prefix = "[!".into();
        mapping.suffix = "]".into();
        let out = collect(&c, "w1", Path::new("/"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.patch["token"], expected);
        assert!(!out.truncated);
    }
    let mut c = collector(format!(
        "printf '%s' '{}'",
        serde_json::json!({"status":"é".repeat(79)})
    ));
    let mapping = c.tokens.get_mut("token").unwrap();
    mapping.prefix = "✓".into();
    mapping.suffix = "!".into();
    let out = collect(&c, "w1", Path::new("/"), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        out.patch["token"],
        Token::Set(format!("✓{}", "é".repeat(79)))
    );
    assert!(out.truncated);
    // Mappings of the same field can have independent decoration.
    c.command = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf '{\"status\":0}'".into(),
    ];
    c.tokens.insert("plain".into(), "status".into());
    let out = collect(&c, "w1", Path::new("/"), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(out.patch["plain"], Token::Set("0".into()));
    assert_eq!(out.patch["token"], Token::Set("✓0!".into()));
}
#[tokio::test]
async fn hidden_command_values_clear_the_entire_decoration() {
    for (value, expected) in [
        (serde_json::Value::Null, Token::Clear),
        (serde_json::json!(""), Token::Clear),
        (serde_json::json!(" \n\u{0007} "), Token::Clear),
        (serde_json::json!(0), Token::Clear),
        (serde_json::json!(0.0), Token::Clear),
        (serde_json::json!("0"), Token::Clear),
        (serde_json::json!(" 0.0 "), Token::Clear),
        (serde_json::json!(1), Token::Set("[!1]".into())),
        (serde_json::json!(0.01), Token::Set("[!0.01]".into())),
        (serde_json::json!(false), Token::Set("[!false]".into())),
        (serde_json::json!("done"), Token::Set("[!done]".into())),
    ] {
        let output = serde_json::json!({"status":value}).to_string();
        let mut c = collector(format!("printf '%s' '{output}'"));
        let mapping = c.tokens.get_mut("token").unwrap();
        mapping.prefix = "[!".into();
        mapping.suffix = "]".into();
        mapping.show_zero = false;
        let out = collect(&c, "w1", Path::new("/"), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(out.patch["token"], expected, "{value}");
        assert!(!out.truncated);
    }
}
#[tokio::test]
async fn decorated_git_counts_include_zero() {
    let t = tempfile::tempdir().unwrap();
    git(t.path(), &["init", "-q"]);
    let mut c = git_collector();
    let mapping = c.tokens.get_mut("staged_files").unwrap();
    mapping.prefix = "✓".into();
    mapping.suffix = " staged".into();
    let out = collect(&c, "w1", t.path(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(out.patch["staged_files"], Token::Set("✓0 staged".into()));
    assert_eq!(out.patch["modified_files"], Token::Set("0".into()));
    c.tokens.get_mut("staged_files").unwrap().show_zero = false;
    let out = collect(&c, "w1", t.path(), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(out.patch["staged_files"], Token::Clear);
    assert_eq!(out.patch["modified_files"], Token::Set("0".into()));
}
#[tokio::test]
async fn strict_json_and_atomic_mapping() {
    for (output, expected) in [
        (r#"{"status":true}"#, Token::Set("true".into())),
        (r#"{"status":0}"#, Token::Set("0".into())),
        (r#"{"status":null}"#, Token::Clear),
        (r#"{"status":" \n\t "}"#, Token::Clear),
        (r#"{"status":"  a=b -x  "}"#, Token::Set("a=b -x".into())),
    ] {
        assert_eq!(command(output).await.unwrap().patch["token"], expected);
    }
    for invalid in [
        "",
        "[]",
        "null",
        "{}",
        r#"{"status":1,"status":2}"#,
        r#"{"other":1,"other":2,"status":1}"#,
        r#"{"status":[]}"#,
        r#"{"status":{}}"#,
        r#"{"status":1} {}"#,
    ] {
        assert!(command(invalid).await.is_err(), "{invalid}");
    }
    let unicode = format!("  {}\u{0007}  ", "é".repeat(81));
    let out = command(&serde_json::json!({"status":unicode}).to_string())
        .await
        .unwrap();
    assert_eq!(out.patch["token"], Token::Set("é".repeat(80)));
    assert!(out.truncated);
    let c = collector("printf '{\"status\":1}'; exit 1".into());
    assert!(
        collect(&c, "w1", Path::new("/"), CancellationToken::new())
            .await
            .is_err()
    );
    let mut c = collector("printf '{\"status\":1}'".into());
    c.tokens.insert("missing".into(), "missing".into());
    assert!(
        collect(&c, "w1", Path::new("/"), CancellationToken::new())
            .await
            .is_err()
    );
}

fn git_collector() -> Collector {
    let mut c = collector(String::new());
    c.provider = Provider::Git;
    c.command.clear();
    c.output = None;
    c.tokens = [
        "modified_files",
        "staged_files",
        "untracked_files",
        "conflict_files",
    ]
    .into_iter()
    .map(|s| (s.into(), s.into()))
    .collect();
    c
}
async fn counts(p: &Path) -> BTreeMap<String, Token> {
    collect(&git_collector(), "w1", p, CancellationToken::new())
        .await
        .unwrap()
        .patch
}
#[tokio::test]
async fn real_git_status_rename_weird_names_worktrees_and_conflicts() {
    use std::os::unix::ffi::OsStringExt;
    let t = tempfile::tempdir().unwrap();
    let p = t.path();
    git(p, &["init", "-q", "-b", "main"]);
    let zero = counts(p).await;
    assert!(zero.values().all(|v| *v == Token::Set("0".into())));
    fs::write(p.join("tracked"), "base\n").unwrap();
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "initial"]);
    fs::write(p.join("tracked"), "staged\n").unwrap();
    git(p, &["add", "."]);
    fs::write(p.join("tracked"), "unstaged\n").unwrap();
    let weird = if cfg!(target_os = "macos") {
        b"bad\nname".to_vec()
    } else {
        b"bad\xff\nname".to_vec()
    };
    fs::write(p.join(std::ffi::OsString::from_vec(weird)), "x").unwrap();
    let c = counts(p).await;
    assert_eq!(c["modified_files"], Token::Set("1".into()));
    assert_eq!(c["staged_files"], Token::Set("1".into()));
    assert_eq!(c["untracked_files"], Token::Set("1".into()));
    git(p, &["add", "."]);
    git(p, &["commit", "-qm", "changes"]);
    git(p, &["mv", "tracked", "renamed\nfile"]);
    assert_eq!(counts(p).await["staged_files"], Token::Set("1".into()));
    git(p, &["commit", "-qm", "rename"]);
    let linked = t.path().join("linked");
    git(
        p,
        &["worktree", "add", "--detach", linked.to_str().unwrap()],
    );
    assert!(
        counts(&linked)
            .await
            .values()
            .all(|v| *v == Token::Set("0".into()))
    );
    git(p, &["checkout", "-qb", "other"]);
    fs::write(p.join("renamed\nfile"), "other\n").unwrap();
    git(p, &["commit", "-qam", "other"]);
    git(p, &["checkout", "-q", "main"]);
    fs::write(p.join("renamed\nfile"), "main\n").unwrap();
    git(p, &["commit", "-qam", "main"]);
    let merge = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "merge",
            "other",
        ])
        .current_dir(p)
        .output()
        .unwrap();
    assert!(!merge.status.success());
    let c = counts(p).await;
    assert_eq!(c["conflict_files"], Token::Set("1".into()));
    assert_eq!(c["modified_files"], Token::Set("0".into()));
    assert_eq!(c["staged_files"], Token::Set("0".into()));
    let nonrepo = tempfile::tempdir().unwrap();
    assert!(
        counts(nonrepo.path())
            .await
            .values()
            .all(|v| *v == Token::Clear)
    );
    let bare = tempfile::tempdir().unwrap();
    git(bare.path(), &["init", "-q", "--bare"]);
    assert!(
        collect(
            &git_collector(),
            "w1",
            bare.path(),
            CancellationToken::new()
        )
        .await
        .is_err()
    );
}
