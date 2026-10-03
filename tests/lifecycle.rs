mod support;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use support::git;
struct Harness {
    _temp: tempfile::TempDir,
    root: PathBuf,
    runner: RefCell<Option<Child>>,
}
impl Harness {
    fn new() -> Self {
        let t = tempfile::tempdir_in("/tmp").unwrap();
        let root = t.path().canonicalize().unwrap();
        for dir in ["config", "w1", "w2"] {
            fs::create_dir(root.join(dir)).unwrap();
        }
        let h = Self {
            _temp: t,
            root,
            runner: RefCell::new(None),
        };
        h.settings(json!({"workspaces":{"w1":h.root.join("w1"),"w2":h.root.join("w2")}}));
        h.config("old");
        h
    }
    fn settings(&self, v: Value) {
        self.atomic("fake.json", &v.to_string());
    }
    fn atomic(&self, path: &str, text: &str) {
        let p = self.root.join(path);
        let tmp = p.with_extension("tmp");
        fs::write(&tmp, text).unwrap();
        fs::rename(tmp, p).unwrap();
    }
    fn config(&self, token: &str) {
        let argv = serde_json::to_string(&[
            "/bin/sh",
            "-c",
            "printf '{\"status\":\"%s\"}' \"$HERDR_TOKENS_WORKSPACE_ID\"",
        ])
        .unwrap();
        self.atomic("config/tokens.toml",&format!("schema_version=1\n[runtime]\nmax_concurrency=2\ndiscovery_interval_ms=1000\n[[collectors]]\nname='command'\nprovider='command'\ncommand={argv}\ninterval_ms=250\ntimeout_ms=200\nttl_ms=750\n[collectors.tokens]\n{token}='status'\n"));
    }
    fn cmd(&self, command: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_herdr-tokens"));
        c.env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &self.root)
            .arg(command)
            .arg("--socket")
            .arg(self.root.join("api.sock"))
            .arg("--runtime-dir")
            .arg(self.root.join("runtime"))
            .arg("--config-dir")
            .arg(self.root.join("config"))
            .arg("--state-dir")
            .arg(self.root.join("state"))
            .arg("--herdr-bin")
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-herdr"));
        // Instrumented child processes must write into cargo-llvm-cov's profile directory.
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            c.env("LLVM_PROFILE_FILE", profile);
        }
        c
    }
    fn spawn(&mut self) {
        let log = fs::File::create(self.root.join("runner.log")).unwrap();
        *self.runner.get_mut() = Some(
            self.cmd("run")
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        self.wait(|h| h.status(false).is_some());
    }
    fn status(&self, values: bool) -> Option<Value> {
        let mut cmd = self.cmd("status");
        cmd.arg("--json");
        if values {
            cmd.arg("--include-values");
        }
        let o = cmd.output().unwrap();
        if !o.status.success() {
            return None;
        }
        serde_json::from_slice::<Value>(&o.stdout)
            .ok()
            .map(|v| v["result"].clone())
    }
    fn wait(&self, condition: impl Fn(&Self) -> bool) {
        let until = Instant::now() + Duration::from_secs(10);
        while !condition(self) {
            if let Some(runner) = self.runner.borrow_mut().as_mut()
                && let Some(status) = runner.try_wait().expect("runner status")
            {
                panic!(
                    "runner exited {status}; redacted log: {}",
                    fs::read_to_string(self.root.join("runner.log")).unwrap_or_default()
                );
            }
            assert!(
                Instant::now() < until,
                "timed out; log: {}",
                fs::read_to_string(self.root.join("runner.log")).unwrap_or_default()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn reports(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("reports.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }
    fn stop(&mut self) {
        let o = self.cmd("stop").output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        if let Some(mut c) = self.runner.get_mut().take() {
            assert!(c.wait().unwrap().success());
        }
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.cmd("stop").output();
        if let Some(mut c) = self.runner.get_mut().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
fn token_report(v: &Value, token: &str) -> bool {
    v["args"].as_array().unwrap().iter().any(|v| {
        v.as_str()
            .is_some_and(|s| s.starts_with(&format!("{token}=")))
    })
}
fn sequence(v: &Value) -> u64 {
    let args = v["args"].as_array().unwrap();
    let i = args.iter().position(|v| v == "--seq").unwrap();
    args[i + 1].as_str().unwrap().parse().unwrap()
}
#[test]
fn global_command_runs_once_in_config_directory_and_fans_out_to_six_workspaces() {
    let mut h = Harness::new();
    let mut workspaces = serde_json::Map::new();
    for i in 1..=6 {
        let name = format!("w{i}");
        fs::create_dir_all(h.root.join(&name)).unwrap();
        workspaces.insert(name, json!(h.root.join(format!("w{i}"))));
    }
    h.settings(json!({"workspaces": workspaces}));
    let argv = serde_json::to_string(&[
        "/bin/sh",
        "-c",
        "test -z \"${HERDR_TOKENS_WORKSPACE_ID+x}\"; test -z \"${HERDR_TOKENS_WORKSPACE_DIR+x}\"; printf 'attempt\\n' >> attempts; printf 'session-value\\n'",
    ])
    .unwrap();
    h.atomic(
        "config/tokens.toml",
        &format!(
            "schema_version=1\n[runtime]\nmax_concurrency=2\ndiscovery_interval_ms=1000\n[[collectors]]\nname='global'\nprovider='command'\nglobal=true\ncommand={argv}\noutput='text'\ninterval_ms=30000\ntimeout_ms=1000\nttl_ms=90000\n[collectors.tokens]\nsession_value='stdout'\n"
        ),
    );

    h.spawn();
    h.wait(|h| {
        let reports = h.reports();
        (1..=6).all(|i| {
            reports.iter().any(|r| {
                r["args"][2] == format!("w{i}")
                    && r["args"]
                        .as_array()
                        .unwrap()
                        .contains(&json!("session_value=session-value"))
            })
        })
    });

    assert_eq!(
        fs::read_to_string(h.root.join("config/attempts"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(
        h.status(false).unwrap()["jobs"].as_array().unwrap().len(),
        1
    );
    h.stop();
}

#[test]
fn newly_discovered_workspace_receives_cached_global_value_without_execution() {
    let mut h = Harness::new();
    h.settings(json!({"workspaces":{"w1":h.root.join("w1")}}));
    let argv = serde_json::to_string(&[
        "/bin/sh",
        "-c",
        "printf 'attempt\\n' >> attempts; printf 'cached-value\\n'",
    ])
    .unwrap();
    h.atomic(
        "config/tokens.toml",
        &format!(
            "schema_version=1\n[runtime]\ndiscovery_interval_ms=1000\n[[collectors]]\nname='global'\nprovider='command'\nglobal=true\ncommand={argv}\noutput='text'\ninterval_ms=30000\ntimeout_ms=1000\nttl_ms=90000\n[collectors.tokens]\nsession_value='stdout'\n"
        ),
    );
    h.spawn();
    h.wait(|h| {
        h.reports().iter().any(|report| {
            report["args"][2] == "w1"
                && report["args"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("session_value=cached-value"))
        })
    });
    assert_eq!(
        fs::read_to_string(h.root.join("config/attempts"))
            .unwrap()
            .lines()
            .count(),
        1
    );

    h.settings(json!({"workspaces":{"w1":h.root.join("w1"),"w2":h.root.join("w2")}}));
    h.wait(|h| {
        h.reports().iter().any(|report| {
            report["args"][2] == "w2"
                && report["args"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("session_value=cached-value"))
        })
    });
    assert_eq!(
        fs::read_to_string(h.root.join("config/attempts"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    h.stop();
}

#[test]
fn formatting_only_reload_clears_then_publishes_decorated_values() {
    let mut h = Harness::new();
    h.spawn();
    h.wait(|h| h.reports().len() >= 2);
    let text = fs::read_to_string(h.root.join("config/tokens.toml")).unwrap();
    h.atomic(
        "config/tokens.toml",
        &text.replace("old='status'", "old={field='status'}"),
    );
    assert!(h.cmd("reload").output().unwrap().status.success());
    assert_eq!(h.status(false).unwrap()["config_generation"], 1);
    h.atomic(
        "config/tokens.toml",
        &text.replace("old='status'", "old={field='status',prefix='[',suffix=']'}"),
    );
    assert!(h.cmd("reload").output().unwrap().status.success());
    h.wait(|h| {
        ["w1", "w2"].iter().all(|w| {
            h.reports().iter().any(|r| {
                r["args"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(format!("old=[{w}]")))
            })
        })
    });
    let reports = h.reports();
    for w in ["w1", "w2"] {
        let first = reports
            .iter()
            .position(|r| {
                r["args"]
                    .as_array()
                    .unwrap()
                    .contains(&json!(format!("old=[{w}]")))
            })
            .unwrap();
        assert!(reports[..first].iter().any(|r| {
            r["args"][2] == w
                && r["args"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("--clear-token"))
        }));
        assert!(!reports[first..].iter().any(|r| {
            r["args"]
                .as_array()
                .unwrap()
                .contains(&json!(format!("old={w}")))
        }));
    }
    h.stop();
}
#[test]
fn polling_reload_and_directory_invalidation() {
    let mut h = Harness::new();
    h.spawn();
    h.wait(|h| h.reports().len() >= 2);
    h.atomic("config/tokens.toml", "schema_version=1\nnot valid TOML");
    h.wait(|h| {
        h.status(false)
            .is_some_and(|s| !s["last_rejected_error"].is_null())
    });
    assert_eq!(h.status(false).unwrap()["config_generation"], 1);
    h.config("polled");
    h.wait(|h| h.reports().iter().any(|v| token_report(v, "polled")));
    h.settings(json!({"workspaces":{"w1":h.root.join("missing"),"w2":h.root.join("w2")}}));
    h.wait(|h| {
        h.status(false)
            .is_some_and(|s| s["jobs"].as_array().unwrap().len() == 1)
    });
    let n = h.reports().len();
    std::thread::sleep(Duration::from_millis(700));
    assert!(
        !h.reports()[n..]
            .iter()
            .any(|v| v["args"][2] == "w1" && token_report(v, "polled"))
    );
    h.settings(json!({"workspaces":{"w1":h.root.join("w2"),"w2":h.root.join("w2")}}));
    h.wait(|h| {
        h.status(false)
            .is_some_and(|s| s["jobs"].as_array().unwrap().len() == 2)
    });
    h.stop();
}
#[test]
fn cancelling_inflight_collection_before_reload_and_concurrency_reduction() {
    let mut h = Harness::new();
    let initial = fs::read_to_string(h.root.join("config/tokens.toml"))
        .unwrap()
        .replace("printf", "sleep 0.5; printf")
        .replace("interval_ms=250", "interval_ms=1000")
        .replace("timeout_ms=200", "timeout_ms=900")
        .replace("ttl_ms=750", "ttl_ms=3000");
    h.atomic("config/tokens.toml", &initial);
    h.spawn();
    h.wait(|h| {
        h.status(false).is_some_and(|s| {
            s["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|j| j["running"] == true)
        })
    });
    h.config("new");
    let text = fs::read_to_string(h.root.join("config/tokens.toml"))
        .unwrap()
        .replace("max_concurrency=2", "max_concurrency=1");
    h.atomic("config/tokens.toml", &text);
    assert!(h.cmd("reload").output().unwrap().status.success());
    h.wait(|h| h.reports().iter().any(|v| token_report(v, "new")));
    assert!(!h.reports().iter().any(|v| token_report(v, "old")));
    for _ in 0..10 {
        let s = h.status(false).unwrap();
        assert!(
            s["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|j| j["running"] == true)
                .count()
                <= 1
        );
    }
    h.stop();
}
#[test]
fn refresh_action_context_is_explicit_and_running_refreshes_coalesce() {
    let mut h = Harness::new();
    let argv=serde_json::to_string(&["/bin/sh", "-c", "printf 'attempt\\n' >> attempts; while [ ! -e released ]; do sleep 0.02; done; printf '{\"status\":1}'"]).unwrap();
    let text = format!(
        "schema_version=1\n[runtime]\nmax_concurrency=2\n[[collectors]]\nname='command'\nprovider='command'\ncommand={argv}\ninterval_ms=30000\ntimeout_ms=5000\nttl_ms=90000\n[collectors.tokens]\nold='status'\n"
    );
    let attempts = |h: &Harness, w: &str| {
        fs::read_to_string(h.root.join(w).join("attempts"))
            .unwrap_or_default()
            .lines()
            .count()
    };
    h.atomic("config/tokens.toml", &text);
    h.spawn();
    h.wait(|h| {
        h.status(false).is_some_and(|s| {
            s["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|j| j["running"] == true)
                .count()
                == 2
        })
    });
    for _ in 0..5 {
        assert!(
            h.cmd("refresh")
                .env("HERDR_WORKSPACE_ID", "w1")
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    for w in ["w1", "w2"] {
        fs::write(h.root.join(w).join("released"), "").unwrap();
    }
    h.wait(|h| attempts(h, "w1") == 2 && attempts(h, "w2") == 2);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(attempts(&h, "w1"), 2);
    assert_eq!(attempts(&h, "w2"), 2);
    assert!(
        h.cmd("refresh")
            .env("HERDR_PLUGIN_ID", "dmytr0x-herdr-tokens")
            .env("HERDR_PLUGIN_ACTION_ID", "refresh")
            .env("HERDR_WORKSPACE_ID", "w1")
            .output()
            .unwrap()
            .status
            .success()
    );
    h.wait(|h| attempts(h, "w1") == 3);
    assert_eq!(attempts(&h, "w2"), 2);
    h.stop();
}
#[test]
fn two_workspaces_reload_fences_restart_sequences_and_redaction() {
    let mut h = Harness::new();
    h.spawn();
    h.wait(|h| {
        h.reports()
            .iter()
            .filter(|v| token_report(v, "old"))
            .count()
            >= 6
    });
    let status = h.status(false).unwrap();
    assert_eq!(status["connection"], "Connected");
    assert_eq!(status["jobs"].as_array().unwrap().len(), 2);
    assert!(!status.to_string().contains("last_collected"));
    let status = h.status(true).unwrap();
    assert!(status.to_string().contains("last_collected"));
    assert!(h.cmd("run").output().unwrap().status.success());
    let reports = h.reports();
    assert!(
        reports
            .iter()
            .any(|v| v["args"].as_array().unwrap().contains(&json!("old=w1")))
    );
    assert!(
        reports
            .iter()
            .any(|v| v["args"].as_array().unwrap().contains(&json!("old=w2")))
    );
    h.atomic("config/tokens.toml", "schema_version=1\nSECRET_INVALID");
    let r = h.cmd("reload").output().unwrap();
    assert_eq!(r.status.code(), Some(2));
    let generation = h.status(false).unwrap()["config_generation"].clone();
    assert_eq!(generation, 1);
    assert!(h.cmd("start").output().unwrap().status.success());
    assert!(h.cmd("run").output().unwrap().status.success());
    h.config("new");
    assert!(h.cmd("reload").output().unwrap().status.success());
    h.wait(|h| {
        h.reports()
            .iter()
            .filter(|v| token_report(v, "new"))
            .count()
            >= 2
    });
    let all = h.reports();
    let first_new = all.iter().position(|v| token_report(v, "new")).unwrap();
    assert!(!all[first_new..].iter().any(|v| token_report(v, "old")));
    for w in ["w1", "w2"] {
        let first = all
            .iter()
            .position(|v| v["args"][2] == w && token_report(v, "new"))
            .unwrap();
        assert!(all[..first].iter().any(|v| {
            v["args"][2] == w
                && v["args"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("--clear-token"))
        }));
    }
    let last = sequence(all.last().unwrap());
    h.stop();
    h.spawn();
    h.wait(|h| h.reports().iter().any(|v| sequence(v) > last + 100));
    h.stop();
    let seq: Vec<_> = h.reports().iter().map(sequence).collect();
    assert!(seq.windows(2).all(|w| w[0] < w[1]));
}
#[test]
fn invalid_discovery_preserves_snapshot_disconnect_recovers_and_empty_reload_clears() {
    let mut h = Harness::new();
    h.spawn();
    h.wait(|h| h.reports().len() >= 2);
    h.settings(json!({"bad_discovery":true,"workspaces":{}}));
    h.wait(|h| {
        h.status(false)
            .is_some_and(|s| !s["discovery_error"].is_null())
    });
    assert_eq!(
        h.status(false).unwrap()["jobs"].as_array().unwrap().len(),
        2
    );
    h.settings(json!({"offline":true,"workspaces":{}}));
    h.wait(|h| {
        h.status(false)
            .is_some_and(|s| s["connection"] == "Disconnected")
    });
    let n = h.reports().len();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(h.reports().len(), n);
    h.settings(json!({"workspaces":{"w1":h.root.join("w1")}}));
    assert!(h.cmd("refresh").output().unwrap().status.success());
    h.wait(|h| {
        h.status(false).is_some_and(|s| {
            s["connection"] == "Connected" && s["jobs"].as_array().unwrap().len() == 1
        })
    });
    h.atomic("config/tokens.toml", "schema_version=1\n");
    assert!(h.cmd("reload").output().unwrap().status.success());
    h.wait(|h| {
        h.status(false)
            .is_some_and(|s| s["jobs"].as_array().unwrap().is_empty())
    });
    h.stop();
}
#[test]
fn detached_start_races_and_identity_conflicts() {
    let h = Harness::new();
    let mut a = h
        .cmd("start")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut b = h
        .cmd("start")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(a.wait().unwrap().success());
    assert!(b.wait().unwrap().success());
    let conflict = Command::new(env!("CARGO_BIN_EXE_herdr-tokens"))
        .arg("start")
        .arg("--socket")
        .arg(h.root.join("api.sock"))
        .arg("--runtime-dir")
        .arg(h.root.join("runtime"))
        .arg("--config-dir")
        .arg(h.root.join("config"))
        .arg("--state-dir")
        .arg(h.root.join("other-state"))
        .output()
        .unwrap();
    assert_eq!(conflict.status.code(), Some(1));
    assert!(h.cmd("stop").output().unwrap().status.success());
    h.wait(|h| h.status(false).is_none());
}
#[test]
fn newly_enabled_git_prerequisite_rejects_reload_without_mutation() {
    use std::os::unix::fs::PermissionsExt;
    let mut h = Harness::new();
    let bin = h.root.join("bin");
    fs::create_dir(&bin).unwrap();
    let git = bin.join("git");
    fs::write(&git, "#!/bin/sh\nprintf 'git version 2.19.0\\n'\n").unwrap();
    fs::set_permissions(&git, fs::Permissions::from_mode(0o700)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let log = fs::File::create(h.root.join("runner.log")).unwrap();
    *h.runner.get_mut() = Some(
        h.cmd("run")
            .env("PATH", path)
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    h.wait(|h| h.status(false).is_some());
    let mut text = fs::read_to_string(h.root.join("config/tokens.toml")).unwrap();
    text.push_str("\n[[collectors]]\nname='git'\nprovider='git'\n[collectors.tokens]\ngit_untracked='untracked_files'\n");
    h.atomic("config/tokens.toml", &text);
    assert!(h.cmd("validate").output().unwrap().status.success());
    assert_eq!(h.cmd("reload").output().unwrap().status.code(), Some(2));
    assert_eq!(h.status(false).unwrap()["config_generation"], 1);
    h.wait(|h| h.reports().iter().any(|v| token_report(v, "old")));
    h.stop();
}
#[test]
fn semantic_publication_errors_are_redacted_and_do_not_disconnect() {
    let mut h = Harness::new();
    h.settings(
        json!({"workspaces":{"w1":h.root.join("w1")},"report_error":"metadata_token_limit"}),
    );
    h.spawn();
    h.wait(|h| {
        h.status(false).is_some_and(|s| {
            s["jobs"].as_array().is_some_and(|jobs| {
                jobs.iter()
                    .any(|j| !j["diagnostics"]["publication_error"].is_null())
            })
        })
    });
    assert_eq!(h.status(false).unwrap()["connection"], "Connected");
    assert!(
        !fs::read_to_string(h.root.join("runner.log"))
            .unwrap()
            .contains("DO_NOT_LOG_RAW_SECRET")
    );
    h.settings(json!({"workspaces":{"w1":h.root.join("w1")}}));
    h.wait(|h| {
        h.status(false)
            .is_some_and(|s| s["jobs"][0]["diagnostics"]["publication_error"].is_null())
    });
    h.stop();
}
#[test]
fn bounded_control_connections_and_oversized_request_recover() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    let mut h = Harness::new();
    h.spawn();
    let socket = fs::read_dir(h.root.join("runtime"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "sock"))
        .unwrap();
    let clients: Vec<_> = (0..32)
        .map(|_| UnixStream::connect(&socket).unwrap())
        .collect();
    std::thread::sleep(Duration::from_millis(100));
    let busy = h.cmd("status").output().unwrap();
    assert!(!busy.status.success());
    assert!(String::from_utf8_lossy(&busy.stderr).contains("busy"));
    drop(clients);
    h.wait(|h| h.status(false).is_some());
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream.write_all(&vec![b'x'; 8193]).unwrap();
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    drop(stream);
    assert!(h.status(false).is_some());
    h.stop();
}
#[test]
fn cli_exit_codes_validate_never_executes_and_no_implicit_endpoint() {
    let h = Harness::new();
    assert!(h.cmd("validate").output().unwrap().status.success());
    let o = Command::new(env!("CARGO_BIN_EXE_herdr-tokens"))
        .env_clear()
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(h.cmd("status").output().unwrap().status.code(), Some(1));
}

impl Harness {
    /// `repo` (main worktree, never opened) with `linked` open as w1; w2 stays a plain directory.
    fn worktrees(&self) {
        let repo = self.root.join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        git(&repo, &["worktree", "add", "-q", "../linked"]);
        self.settings(
            json!({"workspaces":{"w1":self.root.join("linked"),"w2":self.root.join("w2")}}),
        );
    }
    /// Appends `jobs` to the default collector configuration.
    fn jobs(&self, jobs: &str) {
        self.config("old");
        let text = fs::read_to_string(self.root.join("config/tokens.toml")).unwrap();
        self.atomic("config/tokens.toml", &format!("{text}{jobs}"));
    }
    fn background(&self, name: &str) -> Value {
        self.status(false).unwrap()["background_jobs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|j| j["name"] == name)
            .cloned()
            .unwrap()
    }
    fn lines(&self, path: &str) -> Vec<String> {
        fs::read_to_string(self.root.join(path))
            .unwrap_or_default()
            .lines()
            .map(Into::into)
            .collect()
    }
}
/// A job that appends `$PWD $HERDR_TOKENS_WORKSPACE_IDS` to `root/log`.
fn logging_job(h: &Harness, name: &str, extra: &str, log: &str) -> String {
    let argv = serde_json::to_string(&[
        "/bin/sh",
        "-c",
        &format!(
            "echo \"$PWD $HERDR_TOKENS_WORKSPACE_IDS\" >> {}",
            h.root.join(log).display()
        ),
    ])
    .unwrap();
    format!("[[jobs]]\nname='{name}'\ncommand={argv}\ninterval_ms=10000\n{extra}\n")
}
fn alive(pid_file: &std::path::Path) -> bool {
    let pid: i32 = fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
}
#[test]
fn background_main_and_all_modes_resolve_worktrees_and_skip_plain_directories() {
    let mut h = Harness::new();
    h.worktrees();
    h.jobs(&format!(
        "{}{}",
        logging_job(&h, "main", "", "main.log"),
        logging_job(&h, "all", "worktrees='all'", "all.log")
    ));
    h.spawn();
    h.wait(|h| {
        ["main", "all"]
            .iter()
            .all(|j| !h.background(j)["last_run"].is_null())
    });
    assert_eq!(
        h.lines("main.log"),
        [format!("{} w1", h.root.join("repo").display())]
    );
    assert_eq!(
        h.lines("all.log"),
        [format!("{} w1", h.root.join("linked").display())]
    );
    for name in ["main", "all"] {
        let job = h.background(name);
        assert_eq!(job["phase"], "idle");
        assert_eq!(job["last_run"]["targets"], 1);
        assert_eq!(job["last_run"]["succeeded"], 1);
        assert_eq!(job["last_run"]["skipped"], 1);
        assert_eq!(job["targets"][0]["last_exit"], 0);
        assert_eq!(job["targets"][0]["workspaces"], json!(["w1"]));
    }
    h.stop();
}
#[test]
fn background_main_mode_runs_in_bare_repository() {
    let mut h = Harness::new();
    let source = h.root.join("source");
    fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q"]);
    git(&source, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git(&h.root, &["clone", "-q", "--bare", "source", "bare.git"]);
    git(
        &h.root.join("bare.git"),
        &["worktree", "add", "-q", "../wt"],
    );
    h.settings(json!({"workspaces":{"w1":h.root.join("wt")}}));
    h.jobs(&logging_job(&h, "main", "", "main.log"));
    h.spawn();
    h.wait(|h| !h.lines("main.log").is_empty());
    assert_eq!(
        h.lines("main.log"),
        [format!("{} w1", h.root.join("bare.git").display())]
    );
    h.stop();
}
#[test]
fn background_chunks_bound_concurrency() {
    let mut h = Harness::new();
    let mut workspaces = serde_json::Map::new();
    for i in 1..=5 {
        let dir = h.root.join(format!("r{i}"));
        fs::create_dir(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        workspaces.insert(format!("w{i}"), json!(dir));
    }
    h.settings(json!({"workspaces": workspaces}));
    let log = h.root.join("chunks.log");
    let argv = serde_json::to_string(&[
        "/bin/sh",
        "-c",
        &format!(
            "echo start >> {0}; sleep 0.3; echo end >> {0}",
            log.display()
        ),
    ])
    .unwrap();
    h.jobs(&format!(
        "[[jobs]]\nname='chunked'\ncommand={argv}\ninterval_ms=10000\nchunk_size=2\n"
    ));
    h.spawn();
    h.wait(|h| h.lines("chunks.log").len() == 10);
    let mut running = 0;
    let mut peak = 0;
    for line in h.lines("chunks.log") {
        running += if line == "start" { 1 } else { -1 };
        peak = peak.max(running);
    }
    assert_eq!(peak, 2);
    h.wait(|h| h.background("chunked")["last_run"]["succeeded"] == 5);
    h.stop();
}
#[test]
fn run_job_triggers_idle_jobs_and_rejects_unknown_names() {
    let mut h = Harness::new();
    h.worktrees();
    let slow = "interval_ms=86400000";
    h.jobs(
        &format!(
            "{}{}",
            logging_job(&h, "a", "", "a.log").replace("interval_ms=10000\n", ""),
            logging_job(&h, "b", "", "b.log").replace("interval_ms=10000\n", "")
        )
        .replace("\n\n", &format!("\n{slow}\n")),
    );
    h.spawn();
    let o = h
        .cmd("run-job")
        .args(["--job", "unknown"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(
        h.cmd("run-job")
            .args(["--job", "a"])
            .output()
            .unwrap()
            .status
            .success()
    );
    h.wait(|h| h.lines("a.log").len() == 1);
    assert!(h.lines("b.log").is_empty());
    // The manifest action runs `run-job` without arguments: every job is triggered.
    assert!(h.cmd("run-job").output().unwrap().status.success());
    h.wait(|h| h.lines("a.log").len() == 2 && h.lines("b.log").len() == 1);
    h.wait(|h| h.background("a")["phase"] == "idle");
    assert!(h.background("a")["next_due_in_ms"].as_u64().unwrap() > 80_000_000);
    h.stop();
}
#[test]
fn job_only_reload_keeps_collectors_and_publications() {
    let mut h = Harness::new();
    h.worktrees();
    h.jobs(&logging_job(&h, "a", "", "a.log"));
    h.spawn();
    h.wait(|h| h.reports().len() >= 2);
    let before = h.status(false).unwrap();
    let n = h.reports().len();
    h.jobs(&format!(
        "{}{}",
        logging_job(&h, "a", "", "a.log"),
        logging_job(&h, "b", "", "b.log")
    ));
    let o = h.cmd("reload").output().unwrap();
    assert!(o.status.success());
    let after = h.status(false).unwrap();
    assert_eq!(after["config_generation"], before["config_generation"]);
    assert_ne!(after["config_hash"], before["config_hash"]);
    assert_eq!(after["background_jobs"].as_array().unwrap().len(), 2);
    h.wait(|h| h.reports().len() >= n + 4);
    assert!(!h.reports()[n..].iter().any(|r| {
        r["args"]
            .as_array()
            .unwrap()
            .contains(&json!("--clear-token"))
    }));
    h.stop();
}
#[test]
fn changed_job_reload_and_stop_kill_running_process_groups() {
    let mut h = Harness::new();
    h.worktrees();
    let root = h.root.clone();
    let job = move |interval: u64, file: &str| {
        let argv = serde_json::to_string(&[
            "/bin/sh",
            "-c",
            &format!("echo $$ > {}; exec sleep 30", root.join(file).display()),
        ])
        .unwrap();
        format!("[[jobs]]\nname='slow'\ncommand={argv}\ninterval_ms={interval}\ntimeout_ms=60000\n")
    };
    h.jobs(&job(86_400_000, "first.pid"));
    h.spawn();
    assert!(h.cmd("run-job").output().unwrap().status.success());
    h.wait(|h| {
        h.root.join("first.pid").exists() && h.background("slow")["targets"][0]["running"] == true
    });
    let first = h.root.join("first.pid");
    // The collector keeps publishing while the job sleeps.
    let n = h.reports().len();
    h.wait(|h| h.reports().len() >= n + 4);
    assert!(alive(&first));
    h.jobs(&job(86_300_000, "second.pid"));
    assert!(h.cmd("reload").output().unwrap().status.success());
    h.wait(|_| !alive(&first));
    assert!(h.cmd("run-job").output().unwrap().status.success());
    let second = h.root.join("second.pid");
    h.wait(|h| h.root.join("second.pid").exists());
    h.wait(|_| alive(&second));
    let started = Instant::now();
    h.stop();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!alive(&second));
}

#[test]
fn workspace_changes_preserve_unrelated_inflight_collectors() {
    let mut h = Harness::new();
    let script = "echo $$ > started; while [ ! -f release ]; do sleep 0.02; done; printf ready";
    let argv = serde_json::to_string(&["/bin/sh", "-c", script]).unwrap();
    h.atomic("config/tokens.toml", &format!("schema_version=1\n[runtime]\ndiscovery_interval_ms=1000\n[[collectors]]\nname='slow'\nprovider='command'\ncommand={argv}\noutput='text'\ninterval_ms=30000\ntimeout_ms=10000\n[collectors.tokens]\nvalue='stdout'\n"));
    h.settings(json!({"workspaces":{"w1":h.root.join("w1")}}));
    h.spawn();
    h.wait(|h| h.root.join("w1/started").exists());
    let first_pid = fs::read_to_string(h.root.join("w1/started")).unwrap();
    h.settings(json!({"workspaces":{"w1":h.root.join("w1"),"w2":h.root.join("w2")}}));
    h.wait(|h| h.root.join("w2/started").exists());
    assert_eq!(
        fs::read_to_string(h.root.join("w1/started")).unwrap(),
        first_pid
    );
    assert!(alive(&h.root.join("w1/started")));
    fs::create_dir(h.root.join("changed")).unwrap();
    h.settings(json!({"workspaces":{"w1":h.root.join("w1"),"w2":h.root.join("changed")}}));
    h.wait(|h| h.root.join("changed/started").exists());
    assert_eq!(
        fs::read_to_string(h.root.join("w1/started")).unwrap(),
        first_pid
    );
    assert!(alive(&h.root.join("w1/started")));
    fs::write(h.root.join("w1/release"), "").unwrap();
    h.wait(|h| h.reports().iter().any(|r| token_report(r, "value")));
    let stopped = Instant::now();
    h.stop();
    assert!(stopped.elapsed() < Duration::from_secs(6));
    assert!(!alive(&h.root.join("changed/started")));
}

#[test]
#[should_panic(expected = "runner exited")]
fn startup_failure_is_reported_without_waiting_for_the_deadline() {
    let mut h = Harness::new();
    h.atomic("config/tokens.toml", "schema_version=99");
    h.spawn();
}
