//! The gate for the platform's "no toolchain on the author" property.
//!
//! `protoc` IS installed on many dev machines, so a passing build proves
//! nothing on its own — it could have shelled out. This test puts a `protoc`
//! that always FAILS first on `PATH` and in `PROTOC`, so a successful compile
//! is proof that protox did the work.

use std::fs;
use std::process::Command;

#[test]
fn compiles_with_a_sabotaged_protoc() {
    let tmp = std::env::temp_dir().join("boogy-proto-build-gate");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("fakebin")).unwrap();
    fs::create_dir_all(tmp.join("proto")).unwrap();

    let fake = tmp.join("fakebin/protoc");
    fs::write(&fake, "#!/bin/sh\necho 'SABOTAGE: protoc was invoked' >&2\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fs::write(
        tmp.join("proto/gate.proto"),
        "syntax = \"proto3\";\npackage gate.v1;\nmessage Ping { string text = 1; }\n\
         service GateService { rpc Echo(Ping) returns (Ping); }\n",
    )
    .unwrap();

    let out = tmp.join("out");
    fs::create_dir_all(&out).unwrap();

    let path = format!(
        "{}:{}",
        tmp.join("fakebin").display(),
        std::env::var("PATH").unwrap_or_default()
    );

    // Drive the helper in a child process so OUT_DIR/PROTOC are scoped.
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("gate_child")
        .arg("--ignored")
        .env("PATH", &path)
        .env("PROTOC", &fake)
        .env("OUT_DIR", &out)
        .env("CARGO_MANIFEST_DIR", &tmp)
        .env("BOOGY_GATE_PROTO", tmp.join("proto/gate.proto"))
        .env("BOOGY_GATE_INCLUDE", tmp.join("proto"))
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&status.stderr);
    assert!(
        !stderr.contains("SABOTAGE"),
        "protoc was invoked — the no-toolchain property is broken:\n{stderr}"
    );
    assert!(status.status.success(), "child failed:\n{stderr}");
    assert!(
        tmp.join(".boogy/descriptor.bin").exists(),
        "descriptor.bin was not written to the deterministic path"
    );
}

/// Ruling F7's gate: for a `.proto` that DOES declare an RPC `service`
/// (a service-free proto would make this vacuous — messages alone never
/// reference connectrpc, per the pre-flight scan's own finding), no file
/// reachable from the include root a guest's `boogy_sdk::include_protos!()`
/// splices in may mention `connectrpc::`. This walks the ACTUAL files `compile()`
/// wrote to disk (mirroring the `include!` chain by hand), not the crate's
/// own `strip`/`assert` functions — a regression there should fail this
/// independently, not be validated by asking the same code if it worked.
#[test]
fn generated_tree_never_references_connectrpc() {
    let tmp = std::env::temp_dir().join("boogy-proto-build-messages-only-gate");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("proto")).unwrap();
    let out = tmp.join("out");
    fs::create_dir_all(&out).unwrap();

    fs::write(
        tmp.join("proto/svc.proto"),
        "syntax = \"proto3\";\npackage svc.v1;\nmessage Ping { string text = 1; }\n\
         service SvcService { rpc Echo(Ping) returns (Ping); }\n",
    )
    .unwrap();

    // Scoped OUT_DIR/CARGO_MANIFEST_DIR, same reasoning as the sabotage
    // test above: a child process, not in-process env mutation, because
    // tests in this binary run concurrently and share one process env.
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("messages_only_child")
        .arg("--ignored")
        .env("OUT_DIR", &out)
        .env("CARGO_MANIFEST_DIR", &tmp)
        .env("BOOGY_GATE_PROTO", tmp.join("proto/svc.proto"))
        .env("BOOGY_GATE_INCLUDE", tmp.join("proto"))
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "child failed:\n{}",
        String::from_utf8_lossy(&status.stderr)
    );

    let entry = out.join("_connectrpc.rs");
    assert!(entry.exists(), "compile() did not write the include root");

    let mut stack = vec![entry];
    let mut seen = std::collections::HashSet::new();
    let mut visited = 0;
    while let Some(path) = stack.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading generated file {}: {e}", path.display()));
        assert!(
            !content.contains("connectrpc::"),
            "{} is reachable from the include root and still references \
             connectrpc:: — the guest-facing tree must be messages-only",
            path.display()
        );
        visited += 1;
        for line in content.lines() {
            let t = line.trim();
            let included = t
                .strip_prefix("include!(\"")
                .and_then(|rest| rest.strip_suffix("\");"))
                .or_else(|| {
                    t.strip_prefix("include!(concat!(env!(\"OUT_DIR\"), \"/")
                        .and_then(|rest| rest.strip_suffix("\"));"))
                });
            if let Some(name) = included {
                stack.push(out.join(name));
            }
        }
    }
    // Sanity check that the walk actually traversed something (the message
    // file plus the view file plus the mod stitcher, at minimum) rather than
    // vacuously passing because `entry` alone had no includes to follow.
    assert!(
        visited >= 3,
        "expected to walk the message/view/mod-stitcher chain, only visited {visited} file(s)"
    );
}

#[test]
#[ignore = "child process driven by generated_tree_never_references_connectrpc"]
fn messages_only_child() {
    let proto = std::env::var("BOOGY_GATE_PROTO");
    let inc = std::env::var("BOOGY_GATE_INCLUDE");
    let (Ok(proto), Ok(inc)) = (proto, inc) else {
        return;
    };
    // Intentionally not `.expect(...)`: if this errors (including via
    // compile()'s own internal messages-only check), the outer test should
    // fail on its OWN independent inspection of whatever was written to
    // OUT_DIR, not on this child's exit status alone.
    let _ = boogy_proto_build::compile(&[&proto], &[&inc]);
}

#[test]
#[ignore = "child process driven by compiles_with_a_sabotaged_protoc"]
fn gate_child() {
    // `--include-ignored` makes the OUTER `cargo test` harness invoke this
    // function directly too, in-process, alongside spawning it as a
    // subprocess from `compiles_with_a_sabotaged_protoc`. The in-process
    // invocation carries none of that test's env scoping (no sabotaged
    // PATH/PROTOC, no BOOGY_GATE_* vars) — it has nothing to compile and
    // isn't the gate; the gate is the subprocess run, asserted on by the
    // parent test. Skip cleanly rather than panicking on a missing env var.
    let (Ok(proto), Ok(inc)) = (
        std::env::var("BOOGY_GATE_PROTO"),
        std::env::var("BOOGY_GATE_INCLUDE"),
    ) else {
        return;
    };
    boogy_proto_build::compile(&[&proto], &[&inc]).expect("compile without protoc");
}
