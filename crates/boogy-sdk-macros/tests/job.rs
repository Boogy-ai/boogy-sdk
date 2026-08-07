/// Integration tests for the `#[job]` attribute macro.
///
/// Each test annotates a small free fn, then exercises the emitted
/// `JobRegistration` constructor directly (calling `.handler` with
/// synthetic inputs) to verify correct dispatch, deserialization,
/// serialization, and error propagation.
use boogy_sdk_macros::job;
use serde::{Deserialize, Serialize};

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
    let bytes = (reg.handler)(None, b"").expect("handler failed");
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

    let bytes = (reg.handler)(None, br#"{"v":7}"#).expect("handler failed");
    let res: TypedRes = serde_json::from_slice(&bytes).expect("failed to deserialize response");
    assert_eq!(res.doubled, 14);
}

#[test]
fn exact_typed_bad_payload_returns_err() {
    let reg = exact_typed_payload_job();
    let err = (reg.handler)(None, b"not-json").expect_err("expected error");
    assert!(err.contains("payload deserialize"), "err = {err}");
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

    assert!((reg.handler)(Some("ok"), b"").is_ok());
    let err = (reg.handler)(Some("fail"), b"").expect_err("expected error");
    assert_eq!(err, "nope");
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

    assert!((reg.handler)(None, b"x").is_ok());
    let err = (reg.handler)(None, b"y").expect_err("expected error");
    assert!(err.contains("unexpected payload"), "err = {err}");
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

    let bytes = (reg.handler)(Some("3"), br#"{"v":5}"#).expect("handler failed");
    let res: TypedRes = serde_json::from_slice(&bytes).expect("failed to deserialize");
    assert_eq!(res.doubled, 15);

    let err = (reg.handler)(Some("bad"), br#"{"v":1}"#).expect_err("expected error");
    assert!(err.contains("bad suffix"), "err = {err}");
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

    let bytes = (reg.handler)(None, b"").expect("handler failed");
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

    let bytes = (reg.handler)(Some("hi"), b"world").expect("handler failed");
    // The result is `hi:world` serialized as JSON (Vec<u8> != (), so typed branch).
    // The inner fn returns Vec<u8> which serializes as a JSON array of integers.
    // Just check the raw bytes round-tripped by deserializing from the JSON array:
    let decoded: Vec<u8> = serde_json::from_slice(&bytes).expect("failed to decode");
    assert_eq!(decoded, b"hi:world");
}
