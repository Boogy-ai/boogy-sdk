//! How deep a `.proto` may nest messages. Since protox 0.10 a message (or
//! group) may be nested at most 31 levels deep, and `compile` refuses a
//! deeper one with an error naming the limit; before, a deep enough file
//! overflowed the build's stack.
//!
//! One test in its own binary: `compile` reads `OUT_DIR` and
//! `CARGO_MANIFEST_DIR` from the process environment.

use std::fs;

fn nested(depth: usize) -> String {
    let mut src = String::from("syntax = \"proto3\";\npackage nest.v1;\n");
    for i in 0..depth {
        src.push_str(&format!("message M{i} {{ "));
    }
    for _ in 0..depth {
        src.push_str("} ");
    }
    src
}

#[test]
fn messages_may_nest_31_levels_and_no_deeper() {
    let tmp = std::env::temp_dir().join(format!("boogy-proto-build-nesting-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("proto")).unwrap();
    fs::create_dir_all(tmp.join("out")).unwrap();
    std::env::set_var("OUT_DIR", tmp.join("out"));
    std::env::set_var("CARGO_MANIFEST_DIR", &tmp);
    let include = tmp.join("proto");
    let compile = |depth: usize| {
        let file = include.join(format!("n{depth}.proto"));
        fs::write(&file, nested(depth)).unwrap();
        boogy_proto_build::compile(&[file.to_str().unwrap()], &[include.to_str().unwrap()])
    };

    compile(31).expect("31 levels compile");
    let err = compile(32).expect_err("32 levels are refused").to_string();
    assert!(err.contains("nested messages"), "the error names the limit: {err}");
    let _ = fs::remove_dir_all(&tmp);
}
