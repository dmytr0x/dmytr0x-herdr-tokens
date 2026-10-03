use herdr_tokens::config::{CommandOutput, Config, Snapshot, Worktrees};
use std::{fs, path::PathBuf};
fn directory() -> (tempfile::TempDir, PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let p = t.path().canonicalize().unwrap();
    (t, p)
}
const COLLECTOR: &str = "[[collectors]]\nname='ci'\nprovider='command'\ncommand=['echo','{}']\n[collectors.tokens]\nci='status'\n";
#[test]
fn defaults_empty_fragments_and_semantic_hash() {
    let (_t, p) = directory();
    assert!(Config::load(&p).is_err());
    fs::write(p.join("tokens.toml"), "schema_version=1\n").unwrap();
    assert!(Config::load(&p).unwrap().collectors().is_empty());
    fs::create_dir(p.join("tokens.d")).unwrap();
    fs::write(p.join("tokens.d/b.toml"), COLLECTOR).unwrap();
    let c = Config::load(&p).unwrap();
    assert_eq!(c.collectors()[0].ttl_ms(), 30000);
    assert_eq!(c.collectors()[0].timeout_ms(), 1000);
    let before = Snapshot::read(&p).unwrap();
    fs::write(
        p.join("tokens.toml"),
        "# only formatting changed\nschema_version = 1\n",
    )
    .unwrap();
    assert_ne!(before, Snapshot::read(&p).unwrap());
    assert_eq!(c.hash(), Config::load(&p).unwrap().hash());
    fs::write(p.join("tokens.d/a.toml"), COLLECTOR).unwrap();
    assert!(Config::load(&p).is_err());
    fs::remove_file(p.join("tokens.d/a.toml")).unwrap();
    assert!(Config::load(&p).is_ok());
}
#[test]
fn strict_validation_matrix() {
    let (_t, p) = directory();
    let valid = format!("schema_version=1\n{COLLECTOR}");
    let bad = vec![
        "".into(),
        "schema_version=2".into(),
        "schema_version=1\nunknown=true".into(),
        valid.replace("provider='command'", "provider='unknown'"),
        valid.replace("command=['echo','{}']", "command=[]"),
        valid.replace("command=['echo','{}']", "command=['']"),
        valid.replace("command=['echo','{}']", "command=['echo']\ninterval_ms=249"),
        valid.replace(
            "command=['echo','{}']",
            "command=['echo']\ntimeout_ms=10001",
        ),
        valid.replace("command=['echo','{}']", "command=['echo']\nttl_ms=29999"),
        valid.replace("command=['echo','{}']", "command=['echo']\ninterval_ms=-1"),
        valid.replace("command=['echo','{}']", "command=['echo']\ninterval_ms=1.5"),
        valid.replace(
            "command=['echo','{}']",
            "command=['echo']\nttl_ms=18446744073709551616",
        ),
        valid.replace("name='ci'", "name='has space'"),
        valid.replace("ci='status'", "'$ci'='status'"),
        valid.replace("ci='status'", "ci=''"),
        valid.replace("ci='status'", ""),
        valid.replace(
            "command=['echo','{}']",
            "command=['echo']\nenv_allow=['KEY','KEY']",
        ),
        valid.replace(
            "command=['echo','{}']",
            "command=['echo']\nenv_allow=['HERDR_TOKENS_WORKSPACE_ID']",
        ),
        valid.replace(
            "command=['echo','{}']",
            "command=['echo']\nenv={'1BAD'='x'}",
        ),
        valid.replace("provider='command'", "provider='git'"),
        valid.replace(
            "provider='command'\ncommand=['echo','{}']",
            "provider='git'",
        ),
        format!("schema_version=1\n[workspace_dirs]\nw1='relative'\n{COLLECTOR}"),
        format!("schema_version=1\n[runtime]\nmax_concurrency=65\n{COLLECTOR}"),
        format!("schema_version=1\n[runtime]\ndiscovery_interval_ms=999\n{COLLECTOR}"),
        valid.replace(
            "ci='status'",
            &(0..17)
                .map(|i| format!("t{i}='status'\n"))
                .collect::<String>(),
        ),
        format!(
            "{valid}\n{}",
            COLLECTOR.replace("name='ci'", "name='other'")
        ),
    ];
    for (i, s) in bad.into_iter().enumerate() {
        fs::write(p.join("tokens.toml"), s).unwrap();
        assert!(Config::load(&p).is_err(), "case {i}");
    }
    fs::write(
        p.join("tokens.toml"),
        "schema_version=1\nsecret_password='DO_NOT_PRINT_THIS'",
    )
    .unwrap();
    assert!(
        !Config::load(&p)
            .unwrap_err()
            .to_string()
            .contains("DO_NOT_PRINT_THIS")
    );
}
#[test]
fn main_config_symlink_is_supported() {
    let (_t, p) = directory();
    let source = tempfile::tempdir().unwrap();
    let target = source.path().join("tokens.toml");
    fs::write(&target, "schema_version=1\n").unwrap();
    std::os::unix::fs::symlink(&target, p.join("tokens.toml")).unwrap();

    assert!(Config::load(&p).is_ok());
}

#[test]
fn files_limits_symlinks_and_fragment_globals() {
    let (_t, p) = directory();
    fs::write(p.join("tokens.toml"), "schema_version=1").unwrap();
    fs::create_dir(p.join("tokens.d")).unwrap();
    fs::write(p.join("tokens.d/x.toml"), "schema_version=1").unwrap();
    assert!(Config::load(&p).is_err());
    fs::remove_file(p.join("tokens.d/x.toml")).unwrap();
    std::os::unix::fs::symlink(p.join("tokens.toml"), p.join("tokens.d/x.toml")).unwrap();
    assert!(Config::load(&p).is_err());
    fs::remove_file(p.join("tokens.d/x.toml")).unwrap();
    fs::write(p.join("tokens.toml"), vec![b' '; 1_048_577]).unwrap();
    assert!(Config::load(&p).is_err());
    fs::write(p.join("tokens.toml"), [0xff]).unwrap();
    assert!(Config::load(&p).is_err());
    fs::write(p.join("tokens.toml"), "schema_version=1").unwrap();
    for i in 0..64 {
        fs::write(p.join(format!("tokens.d/{i}.toml")), "").unwrap();
    }
    assert!(Config::load(&p).is_err());
}
#[test]
fn zero_visibility_defaults_alias_and_hash() {
    let (_t, p) = directory();
    let text = format!("schema_version=1\n{COLLECTOR}");
    fs::write(p.join("tokens.toml"), &text).unwrap();
    let default = Config::load(&p).unwrap();
    assert!(default.collectors()[0].tokens()["ci"].show_zero);
    for flag in ["show_zero", "show_always"] {
        for enabled in [true, false] {
            fs::write(
                p.join("tokens.toml"),
                text.replace(
                    "ci='status'",
                    &format!("ci={{field='status',{flag}={enabled}}}"),
                ),
            )
            .unwrap();
            let config = Config::load(&p).unwrap();
            assert_eq!(config.collectors()[0].tokens()["ci"].show_zero, enabled);
            assert_eq!(default.hash() == config.hash(), enabled);
        }
    }
}
#[test]
fn decorated_mappings_are_strict_and_normalized() {
    let (_t, p) = directory();
    let text = format!("schema_version=1\n{COLLECTOR}");
    fs::write(p.join("tokens.toml"), &text).unwrap();
    let shorthand = Config::load(&p).unwrap();
    fs::write(
        p.join("tokens.toml"),
        text.replace("ci='status'", "ci={field='status',prefix='',suffix=''}"),
    )
    .unwrap();
    assert_eq!(shorthand, Config::load(&p).unwrap());
    assert_eq!(shorthand.hash(), Config::load(&p).unwrap().hash());
    fs::write(
        p.join("tokens.toml"),
        text.replace("ci='status'", "ci={field='status',prefix='[!',suffix=']'}"),
    )
    .unwrap();
    let decorated = Config::load(&p).unwrap();
    assert_eq!(decorated.collectors()[0].tokens()["ci"].prefix, "[!");
    assert_ne!(decorated, shorthand);
    for value in [
        "{prefix='!'}",
        "{field=''}",
        "{field=42}",
        "{field='status',prefix=1}",
        "{field='status',show_zero='false'}",
        "{field='status',show_always=0}",
        "{field='status',show_zero=false,show_always=false}",
        "{field='status',unknown='x'}",
        "{field='status',fg='yellow'}",
        "{field='status',bg='base'}",
        "{field='status',prefix=\"\\n\"}",
        "{field='status',suffix=\"\\u0000\"}",
    ] {
        fs::write(
            p.join("tokens.toml"),
            text.replace("ci='status'", &format!("ci={value}")),
        )
        .unwrap();
        assert!(Config::load(&p).is_err(), "accepted {value}");
    }
    for (length, valid) in [(80, true), (81, false)] {
        let value = format!("ci={{field='status',prefix='{}'}}", "✓".repeat(length));
        fs::write(p.join("tokens.toml"), text.replace("ci='status'", &value)).unwrap();
        assert_eq!(Config::load(&p).is_ok(), valid);
    }
    let bad_git = text
        .replace(
            "provider='command'\ncommand=['echo','{}']",
            "provider='git'",
        )
        .replace("ci='status'", "ci={field='unknown',prefix='!'}");
    fs::write(p.join("tokens.toml"), bad_git).unwrap();
    assert!(Config::load(&p).is_err());
}
#[test]
fn command_text_output_accepts_stdout_mapping() {
    let (_t, p) = directory();
    fs::write(
        p.join("tokens.toml"),
        "schema_version=1\n[[collectors]]\nname='date'\nprovider='command'\ncommand=['date']\noutput='text'\n[collectors.tokens]\ncurrent_date='stdout'\n",
    )
    .unwrap();

    let config = Config::load(&p).unwrap();
    assert_eq!(config.collectors()[0].output(), Some(&CommandOutput::Text));
}

#[test]
fn global_command_collector_is_accepted() {
    let (_t, p) = directory();
    fs::write(
        p.join("tokens.toml"),
        "schema_version=1\n[[collectors]]\nname='date'\nprovider='command'\nglobal=true\ncommand=['date']\noutput='text'\n[collectors.tokens]\ncurrent_date='stdout'\n",
    )
    .unwrap();

    let config = Config::load(&p).unwrap();
    assert!(config.collectors()[0].global());
}

#[test]
fn global_git_collector_is_rejected() {
    let (_t, p) = directory();
    fs::write(
        p.join("tokens.toml"),
        "schema_version=1\n[[collectors]]\nname='git'\nprovider='git'\nglobal=true\n[collectors.tokens]\nuntracked='untracked_files'\n",
    )
    .unwrap();

    assert!(Config::load(&p).is_err());
}

#[test]
fn command_text_output_rejects_non_stdout_mapping() {
    let (_t, p) = directory();
    fs::write(
        p.join("tokens.toml"),
        "schema_version=1\n[[collectors]]\nname='date'\nprovider='command'\ncommand=['date']\noutput='text'\n[collectors.tokens]\ncurrent_date='value'\n",
    )
    .unwrap();

    assert!(Config::load(&p).is_err());
}

#[test]
fn shipped_examples_validate() {
    Config::load(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples")).unwrap();
}

const JOB: &str = "[[jobs]]\nname='fetch'\ncommand=['git','fetch']\n";
#[test]
fn job_defaults_and_bounds() {
    let (_t, p) = directory();
    fs::write(p.join("tokens.toml"), format!("schema_version=1\n{JOB}")).unwrap();
    let job = Config::load(&p).unwrap().jobs()[0].clone();
    assert_eq!(
        (
            job.worktrees(),
            job.interval_ms(),
            job.timeout_ms(),
            job.chunk_size(),
            job.chunk_delay_ms()
        ),
        (Worktrees::Main, 300_000, 60_000, 4, 0)
    );
    let set = |extra: &str| {
        fs::write(
            p.join("tokens.toml"),
            format!("schema_version=1\n{JOB}{extra}\n"),
        )
        .unwrap();
        Config::load(&p)
    };
    let job = set("interval_ms=10000").unwrap().jobs()[0].clone();
    assert_eq!(job.timeout_ms(), 10_000);
    let job = set("worktrees='all'\ninterval_ms=86400000\ntimeout_ms=600000\nchunk_size=16\nchunk_delay_ms=86400000")
        .unwrap()
        .jobs()
        [0].clone();
    assert_eq!(job.worktrees(), Worktrees::All);
    for (extra, valid) in [
        ("interval_ms=9999", false),
        ("interval_ms=86400001", false),
        ("timeout_ms=0", false),
        ("timeout_ms=600001\ninterval_ms=900000", false),
        ("interval_ms=10000\ntimeout_ms=10001", false),
        ("chunk_size=0", false),
        ("chunk_size=17", false),
        ("interval_ms=10000\nchunk_delay_ms=10001", false),
        ("interval_ms=10000\nchunk_delay_ms=10000", true),
        ("worktrees='linked'", false),
        ("unknown=1", false),
        ("tokens={x='y'}", false),
        ("env_allow=['HERDR_TOKENS_JOB']", false),
        ("env={HERDR_TOKENS_WORKTREE_DIR='x'}", false),
        ("env={HERDR_TOKENS_WORKSPACE_IDS='x'}", false),
        ("env={NAME='x'}\nenv_allow=['HOME']", true),
    ] {
        assert_eq!(set(extra).is_ok(), valid, "{extra}");
    }
}
#[test]
fn job_names_commands_limits_and_fragments() {
    let (_t, p) = directory();
    let bad = [
        format!("schema_version=1\n{JOB}{JOB}"),
        format!(
            "schema_version=1\n{COLLECTOR}{}",
            JOB.replace("'fetch'", "'ci'")
        ),
        format!(
            "schema_version=1\n{}",
            JOB.replace("'fetch'", "'has space'")
        ),
        format!("schema_version=1\n{}", JOB.replace("['git','fetch']", "[]")),
        format!(
            "schema_version=1\n{}",
            JOB.replace("['git','fetch']", "['']")
        ),
        (0..17).fold("schema_version=1\n".to_owned(), |s, i| {
            s + &JOB.replace("'fetch'", &format!("'j{i}'"))
        }),
    ];
    for (i, text) in bad.iter().enumerate() {
        fs::write(p.join("tokens.toml"), text).unwrap();
        assert!(Config::load(&p).is_err(), "case {i}");
    }
    fs::write(
        p.join("tokens.toml"),
        format!("schema_version=1\n{COLLECTOR}"),
    )
    .unwrap();
    fs::create_dir(p.join("tokens.d")).unwrap();
    fs::write(p.join("tokens.d/b.toml"), JOB.replace("'fetch'", "'b'")).unwrap();
    fs::write(p.join("tokens.d/a.toml"), JOB.replace("'fetch'", "'a'")).unwrap();
    let config = Config::load(&p).unwrap();
    let names: Vec<_> = config.jobs().iter().map(|j| j.name()).collect();
    assert_eq!(names, ["a", "b"]);
    assert!(config.same_collection_config(&config));
}

#[test]
fn fragment_errors_identify_source_entry_field_and_span_without_values() {
    let (_t, p) = directory();
    fs::write(p.join("tokens.toml"), "schema_version=1").unwrap();
    fs::create_dir(p.join("tokens.d")).unwrap();
    fs::write(
        p.join("tokens.d/a.toml"),
        COLLECTOR.replace(
            "command=['echo','{}']",
            "command=['SECRET_ARGUMENT']\nenv={KEY='SECRET_VALUE'}\ninterval_ms=1",
        ),
    )
    .unwrap();
    let error = Config::load(&p).unwrap_err().to_string();
    assert!(error.contains("fragment 1 collectors[0] bytes"), "{error}");
    assert!(error.contains("interval_ms"));
    assert!(!error.contains("SECRET"));
    fs::write(
        p.join("tokens.d/a.toml"),
        "secret='SECRET_VALUE'\nunknown=[",
    )
    .unwrap();
    let error = Config::load(&p).unwrap_err().to_string();
    assert!(error.contains("fragment 1: invalid TOML/schema at byte"));
    assert!(!error.contains("SECRET"));
}
