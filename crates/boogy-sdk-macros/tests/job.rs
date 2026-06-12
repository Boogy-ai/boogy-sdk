/// Integration tests for the `#[job]` attribute macro.
///
/// Each test annotates a small free fn, then exercises the emitted
/// `JobRegistration` constructor directly (calling `.handler` with
/// synthetic inputs) to verify correct dispatch, deserialization,
/// serialization, error propagation, and the optional `ctx: JobContext`.
use boogy_sdk::{JobContext, JobError};
use boogy_sdk_macros::job;
use serde::{Deserialize, Serialize};

/// Build a minimal `JobContext` for direct handler invocation. `attempts`
/// defaults to 1; tests that care override it.
fn ctx(handler: &str) -> JobContext {
    JobContext {
        job_id: "job_test".to_string(),
        handler: handler.to_string(),
        attempts: 1,
        not_before_unix_s: 0,
    }
}

// ---------------------------------------------------------------------------
// Test types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TypedReq {
    v: u32,
}

#[derive(Serialize, Deserialize)]
struct TypedRes {
    doubled: u32,
}

// ---------------------------------------------------------------------------
// Test 1: exact, no payload, unit result  →  handler returns Ok(vec![])
// ---------------------------------------------------------------------------

#[job("exact_no_payload")]
fn exact_no_payload_job() -> Result<(), String> {
    Ok(())
}

#[test]
fn exact_no_payload_emits_correct_registration() {
    let reg = exact_no_payload_job();
    assert_eq!(reg.name, "exact_no_payload");
    assert!(!reg.is_prefix);
    let bytes = (reg.handler)(&ctx("exact_no_payload"), None, b"").expect("handler failed");
    assert!(bytes.is_empty(), "unit result should yield empty vec");
}

// ---------------------------------------------------------------------------
// Test 2: exact, typed payload, typed result  →  deser req, ser res
// ---------------------------------------------------------------------------

#[job("exact_typed_payload")]
fn exact_typed_payload_job(payload: TypedReq) -> Result<TypedRes, String> {
    Ok(TypedRes { doubled: payload.v * 2 })
}

#[test]
fn exact_typed_round_trips() {
    let reg = exact_typed_payload_job();
    assert_eq!(reg.name, "exact_typed_payload");
    assert!(!reg.is_prefix);

    let bytes = (reg.handler)(&ctx("exact_typed_payload"), None, br#"{"v":7}"#).expect("handler failed");
    let res: TypedRes = serde_json::from_slice(&bytes).expect("failed to deserialize response");
    assert_eq!(res.doubled, 14);
}

#[test]
fn exact_typed_bad_payload_is_terminal() {
    let reg = exact_typed_payload_job();
    let err = (reg.handler)(&ctx("exact_typed_payload"), None, b"not-json").expect_err("expected error");
    // A bad payload never deserializes on retry → Terminal.
    assert!(matches!(err, JobError::Terminal(_)), "err = {err:?}");
    assert!(err.message().contains("payload deserialize"), "err = {err}");
}

// ---------------------------------------------------------------------------
// Test 3: prefix, no payload, unit result  →  suffix passed; errors propagate
// ---------------------------------------------------------------------------

#[job(prefix = "p_")]
fn prefix_no_payload_job(suffix: &str) -> Result<(), String> {
    if suffix == "fail" {
        Err("nope".into())
    } else {
        Ok(())
    }
}

#[test]
fn prefix_passes_suffix_and_handles_error() {
    let reg = prefix_no_payload_job();
    assert!(reg.is_prefix);
    assert_eq!(reg.name, "p_");

    assert!((reg.handler)(&ctx("p_ok"), Some("ok"), b"").is_ok());
    let err = (reg.handler)(&ctx("p_fail"), Some("fail"), b"").expect_err("expected error");
    // A bare String error maps to Retry.
    assert_eq!(err, JobError::Retry("nope".into()));
}

// ---------------------------------------------------------------------------
// Test 4: exact, raw Vec<u8> payload  →  bytes passed through, no serde
// ---------------------------------------------------------------------------

#[job("raw_payload")]
fn raw_payload_job(payload: Vec<u8>) -> Result<(), String> {
    if payload == b"x" {
        Ok(())
    } else {
        Err(format!("unexpected payload: {:?}", payload))
    }
}

#[test]
fn raw_bytes_skip_serde() {
    let reg = raw_payload_job();
    assert_eq!(reg.name, "raw_payload");
    assert!(!reg.is_prefix);

    assert!((reg.handler)(&ctx("raw_payload"), None, b"x").is_ok());
    let err = (reg.handler)(&ctx("raw_payload"), None, b"y").expect_err("expected error");
    assert!(err.message().contains("unexpected payload"), "err = {err}");
}

// ---------------------------------------------------------------------------
// Test 5: prefix, typed payload  →  suffix + deserialized payload both present
// ---------------------------------------------------------------------------

#[job(prefix = "calc_")]
fn prefix_typed_payload_job(suffix: &str, payload: TypedReq) -> Result<TypedRes, String> {
    let multiplier: u32 = suffix.parse().map_err(|_| format!("bad suffix: {suffix}"))?;
    Ok(TypedRes { doubled: payload.v * multiplier })
}

#[test]
fn prefix_with_typed_payload_round_trips() {
    let reg = prefix_typed_payload_job();
    assert!(reg.is_prefix);
    assert_eq!(reg.name, "calc_");

    let bytes = (reg.handler)(&ctx("calc_3"), Some("3"), br#"{"v":5}"#).expect("handler failed");
    let res: TypedRes = serde_json::from_slice(&bytes).expect("failed to deserialize");
    assert_eq!(res.doubled, 15);

    let err = (reg.handler)(&ctx("calc_bad"), Some("bad"), br#"{"v":1}"#).expect_err("expected error");
    assert!(err.message().contains("bad suffix"), "err = {err}");
}

// ---------------------------------------------------------------------------
// Test 6: exact, typed result (R != ())  →  serde_json serialized on Ok
// ---------------------------------------------------------------------------

#[job("typed_result")]
fn typed_result_job() -> Result<TypedRes, String> {
    Ok(TypedRes { doubled: 42 })
}

#[test]
fn exact_no_payload_typed_result_serializes() {
    let reg = typed_result_job();
    assert_eq!(reg.name, "typed_result");
    assert!(!reg.is_prefix);

    let bytes = (reg.handler)(&ctx("typed_result"), None, b"").expect("handler failed");
    let res: TypedRes = serde_json::from_slice(&bytes).expect("failed to deserialize");
    assert_eq!(res.doubled, 42);
}

// ---------------------------------------------------------------------------
// Test 7: prefix, raw Vec<u8> payload  →  suffix + raw bytes both present
// ---------------------------------------------------------------------------

#[job(prefix = "echo_")]
fn prefix_raw_payload_job(suffix: &str, payload: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut out = format!("{suffix}:").into_bytes();
    out.extend_from_slice(&payload);
    Ok(out)
}

#[test]
fn prefix_raw_payload_round_trips() {
    let reg = prefix_raw_payload_job();
    assert!(reg.is_prefix);
    assert_eq!(reg.name, "echo_");

    let bytes = (reg.handler)(&ctx("echo_hi"), Some("hi"), b"world").expect("handler failed");
    let decoded: Vec<u8> = serde_json::from_slice(&bytes).expect("failed to decode");
    assert_eq!(decoded, b"hi:world");
}

// ---------------------------------------------------------------------------
// Test 8: optional leading `ctx: JobContext` + explicit JobError control
// ---------------------------------------------------------------------------

#[job("attempt_aware")]
fn attempt_aware_job(ctx: JobContext, payload: TypedReq) -> Result<(), JobError> {
    // Use the payload so it's a real typed-payload + ctx handler.
    let _ = payload.v;
    if ctx.attempts >= 3 {
        Err(JobError::Terminal("retries exhausted".into()))
    } else {
        Err(JobError::Retry("try again".into()))
    }
}

#[test]
fn ctx_attempts_drive_retry_then_terminal() {
    let reg = attempt_aware_job();
    assert_eq!(reg.name, "attempt_aware");
    assert!(!reg.is_prefix);

    let mut c = ctx("attempt_aware");
    c.attempts = 1;
    let early = (reg.handler)(&c, None, br#"{"v":1}"#).expect_err("expected retry");
    assert!(matches!(early, JobError::Retry(_)), "early = {early:?}");

    c.attempts = 3;
    let late = (reg.handler)(&c, None, br#"{"v":1}"#).expect_err("expected terminal");
    assert!(matches!(late, JobError::Terminal(_)), "late = {late:?}");
}

// ---------------------------------------------------------------------------
// Test 9: ctx-only handler (no payload, no suffix)
// ---------------------------------------------------------------------------

#[job("ctx_only")]
fn ctx_only_job(ctx: JobContext) -> Result<(), String> {
    if ctx.job_id.is_empty() {
        Err("missing job id".into())
    } else {
        Ok(())
    }
}

#[test]
fn ctx_only_handler_reads_context() {
    let reg = ctx_only_job();
    assert_eq!(reg.name, "ctx_only");
    assert!((reg.handler)(&ctx("ctx_only"), None, b"").is_ok());
}
