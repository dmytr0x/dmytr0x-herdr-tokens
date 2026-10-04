use herdr_tokens::{diagnostics::JobStatus, providers::Token};
use std::{collections::BTreeMap, time::Duration};
use tokio::time::Instant;

#[tokio::test(start_paused = true)]
async fn values_are_opt_in_and_expiry_uses_collection_not_acknowledgement() {
    let collected = Instant::now();
    tokio::time::advance(Duration::from_millis(200)).await;
    let status = JobStatus {
        collected: Some(collected),
        acknowledged: Some(Instant::now()),
        acknowledged_completion: Some(collected),
        last_collected: Some(BTreeMap::from([(
            "secret".into(),
            Token::Set("private".into()),
        )])),
        last_acknowledged: Some(BTreeMap::from([("secret".into(), Token::Clear)])),
        ..JobStatus::default()
    };
    let redacted = status.json(false, 1000);
    assert!(redacted.get("last_collected").is_none());
    assert!(redacted.get("last_acknowledged").is_none());
    assert!(!redacted.to_string().contains("private"));
    assert_eq!(redacted["estimated_expiry_in_ms"], 800);
    assert_eq!(redacted["last_acknowledgement_age_ms"], 0);
    assert_eq!(
        status.json(true, 1000)["last_collected"]["secret"],
        "private"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    assert_eq!(status.json(false, 1000)["estimated_expiry_in_ms"], 0);
}

#[tokio::test(start_paused = true)]
async fn unobserved_times_remain_unknown() {
    let status = JobStatus::default().json(true, 1000);
    for field in [
        "last_attempt_age_ms",
        "last_collection_age_ms",
        "last_acknowledgement_age_ms",
        "estimated_expiry_in_ms",
        "last_collected",
        "last_acknowledged",
    ] {
        assert!(status[field].is_null(), "{field}");
    }
    assert_eq!(status["consecutive_failures"], 0);
}

#[test]
fn non_utf8_paths_remain_serializable_display_context() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};
    let path = PathBuf::from(OsString::from_vec(b"/repo/bad\xffname".to_vec()));
    let directory = herdr_tokens::herdr::Directory {
        reported: Some(path.clone()),
        canonical: Some(path.clone()),
        reason: "fixture".into(),
    };
    let value = serde_json::to_value(directory).unwrap();
    assert_eq!(value["canonical"], "/repo/bad�name");
    let target = herdr_tokens::diagnostics::TargetStatus::default().json(&path);
    assert_eq!(target["dir"], "/repo/bad�name");
}
