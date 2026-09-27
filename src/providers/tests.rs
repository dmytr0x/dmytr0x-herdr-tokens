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
