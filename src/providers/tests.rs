use super::*;

#[test]
fn normalization_clears_controls_and_counts_unicode_scalars() {
    for input in ["", " \n\t ", "\0\u{7f}\u{85}"] {
        assert_eq!(normalize(input), (Token::Clear, false));
    }
    assert_eq!(normalize(" \t ab\0c\n "), (Token::Set("abc".into()), false));
    assert_eq!(
        normalize(&"界".repeat(80)),
        (Token::Set("界".repeat(80)), false)
    );
    assert_eq!(
        normalize(&"界".repeat(81)),
        (Token::Set("界".repeat(80)), true)
    );
}

#[test]
fn decorations_never_resurrect_hidden_values() {
    let mut mapping = TokenMapping {
        field: "value".into(),
        prefix: "[".into(),
        suffix: "]".into(),
        show_zero: false,
    };
    for input in ["", "\n", "0", " 0.0 "] {
        assert_eq!(render(input, &mapping), (Token::Clear, false));
    }
    for input in ["false", "00", "0.00", "-0", "1"] {
        assert_eq!(
            render(input, &mapping),
            (Token::Set(format!("[{input}]")), false)
        );
    }
    mapping.show_zero = true;
    assert_eq!(render("0", &mapping), (Token::Set("[0]".into()), false));
    assert_eq!(
        render(&"界".repeat(80), &mapping),
        (Token::Set(format!("[{}", "界".repeat(79))), true)
    );
}

#[test]
fn json_scalars_literal_selectors_and_unmapped_values() {
    let mappings = ["text", "number", "boolean", "null", "literal.dot"]
        .into_iter()
        .map(|field| (field.to_owned(), field.into()))
        .collect();
    let (patch, truncated) = command::parse_json(br#"{"text":"ok","number":42,"boolean":false,"null":null,"literal.dot":"literal","ignored":{"nested":[]}}"#, &mappings).unwrap();
    assert!(!truncated);
    assert_eq!(
        patch,
        BTreeMap::from([
            ("text".into(), Token::Set("ok".into())),
            ("number".into(), Token::Set("42".into())),
            ("boolean".into(), Token::Set("false".into())),
            ("null".into(), Token::Clear),
            ("literal.dot".into(), Token::Set("literal".into())),
        ])
    );
}

#[test]
fn invalid_json_rejects_the_whole_patch() {
    let mappings = BTreeMap::from([("a".into(), "a".into()), ("b".into(), "b".into())]);
    for bytes in [
        br#"{"a":"ok"}"#.as_slice(),
        br#"{"a":"ok","b":[]}"#,
        br#"{"a":"ok","b":{}}"#,
        br#"{"a":"ok","b":1,"b":2}"#,
        br#"{"a":"ok","b":1,"ignored":1,"ignored":2}"#,
        br#"{"a":"ok","b":1} {}"#,
        br#"[]"#,
        br#"null"#,
        b"",
        b"\xff",
    ] {
        assert!(
            matches!(
                command::parse_json(bytes, &mappings),
                Err(Error::InvalidOutput)
            ),
            "{bytes:?}"
        );
    }
}

#[test]
fn text_removes_terminal_sequences_and_rejects_invalid_utf8() {
    let mappings = BTreeMap::from([("status".into(), "stdout".into())]);
    for bytes in [
        b"\x1b[31mready\x1b[0m".as_slice(),
        b"\x1b]0;title\x07ready",
        b"\x1b]0;title\x1b\\ready",
        b"\x1bPignored\x1b\\ready",
        b"\x1bXignored\x07ready",
        b"\x1b^ignored\x07ready",
        b"\x1b_ignored\x07ready",
        b"\x1b7ready",
        b"ready\x1b",
        b"ready\x1b[",
        b"ready\x1b]unfinished",
    ] {
        assert_eq!(
            command::parse_text(bytes, &mappings).unwrap(),
            (
                BTreeMap::from([("status".into(), Token::Set("ready".into()))]),
                false
            )
        );
    }
    assert!(command::parse_text(b"\xff", &mappings).is_err());
    assert!(
        command::parse_text(
            b"ready",
            &BTreeMap::from([("status".into(), "other".into())])
        )
        .is_err()
    );
}

#[tokio::test]
async fn resolution_distinguishes_absence_cancellation_and_unavailable_paths() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        resolve_worktree(dir.path(), CancellationToken::new())
            .await
            .unwrap(),
        WorktreeResolution::NotRepository
    );
    assert!(matches!(
        resolve_worktree(&dir.path().join("missing"), CancellationToken::new()).await,
        Err(ResolutionError::UnavailablePath)
    ));
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        resolve_worktree(dir.path(), cancel).await,
        Err(ResolutionError::Process(process::Error::Cancelled))
    ));
    assert!(matches!(
        line_path(b"", dir.path()),
        Err(ResolutionError::Malformed)
    ));
    assert!(matches!(
        line_path(b"missing", dir.path()),
        Err(ResolutionError::UnavailablePath)
    ));
    for error in [
        process::Error::Io,
        process::Error::Timeout,
        process::Error::Cancelled,
    ] {
        let message = error.to_string();
        assert_eq!(ResolutionError::from(error).to_string(), message);
    }
}

#[test]
fn text_requires_utf8_even_inside_removed_escapes() {
    let mappings = BTreeMap::from([("status".into(), "stdout".into())]);
    for bytes in [b"\x1b]0;\xff\x07ok".as_slice(), b"\x1b[\xffmok"] {
        assert!(command::parse_text(bytes, &mappings).is_err());
    }
    assert_eq!(
        command::parse_text("\x1b[31m界\x1b[0m".as_bytes(), &mappings)
            .unwrap()
            .0["status"],
        Token::Set("界".into())
    );
}

#[test]
fn nul_worktree_records_preserve_unusual_path_bytes_and_bare_repositories() {
    for path in [
        b"/ordinary".as_slice(),
        b"/space name",
        b"/new\nline\tquote\"\\\xff",
    ] {
        for kind in ["bare".to_owned(), format!("HEAD {}", "a".repeat(40))] {
            let mut bytes = b"worktree ".to_vec();
            bytes.extend_from_slice(path);
            bytes.extend_from_slice(format!("\0{kind}\0\0").as_bytes());
            assert_eq!(main_worktree_record(&bytes).unwrap(), path);
        }
    }
    for bytes in [
        b"worktree /repo\nbare\n\n".as_slice(),
        b"worktree /repo\0HEAD bad\0\0",
        b"worktree /repo\0bare",
        b"worktree \0bare\0\0",
    ] {
        assert!(main_worktree_record(bytes).is_err());
    }
}
