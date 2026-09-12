//! Compile a service's `.proto` files at build time.
//!
//! `connectrpc-build` shells out to `protoc` or `buf` by default. Neither is
//! acceptable here: a Boogy author installs no toolchain (the frontend pipeline
//! sets the same precedent by transpiling TypeScript in Rust). So this crate
//! compiles the descriptor with `protox` — a pure-Rust protobuf compiler — and
//! hands the result to `connectrpc-build` through its precompiled-descriptor
//! hook, which reads a FILE. That file boundary is also why `protox`'s
//! `prost`-typed `FileDescriptorSet` and `buffa`'s never have to meet: they
//! meet as protobuf bytes.
//!
//! The generated tree is **messages only**: `connectrpc-build`'s own RPC
//! machinery (service traits, `Router`, `ServiceRegister`, the generated
//! client) unconditionally names `::connectrpc::` paths, and `connectrpc`
//! pulls `wasm-bindgen-futures` for any `wasm32` target — a guest never
//! needs any of that (it gets its own dispatcher, not connectrpc's Router or
//! client) and must not carry the dependency, so `compile()` deletes that
//! machinery from what it writes and refuses to succeed if any of it
//! survives.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Opaque build failure. Carries the underlying message.
#[derive(Debug)]
pub struct BuildError(String);

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "boogy-proto-build: {}", self.0)
    }
}
impl std::error::Error for BuildError {}

/// Where the descriptor lands, relative to the crate root.
///
/// Fixed rather than configurable so `boogy deploy` can find it without
/// parsing build output or reading the manifest twice.
pub const DESCRIPTOR_REL_PATH: &str = ".boogy/descriptor.bin";

/// The include-root file `compile()` asks `connectrpc-build` to write, and
/// the entry point `boogy_sdk::include_protos!()` splices in. Named once so
/// the writer (`Config::include_file`) and the reachability walk below
/// can't drift apart.
const INCLUDE_FILE_NAME: &str = "_connectrpc.rs";

/// Compile `protos` (resolving imports against `includes`) into `OUT_DIR`.
///
/// Call from `build.rs`. Emits `cargo:rerun-if-changed` for each proto.
pub fn compile(protos: &[&str], includes: &[&str]) -> Result<(), BuildError> {
    let err = |e: String| BuildError(e);

    let out_dir = PathBuf::from(
        std::env::var("OUT_DIR").map_err(|_| err("OUT_DIR is unset".into()))?,
    );
    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .map_err(|_| err("CARGO_MANIFEST_DIR is unset".into()))?,
    );

    for p in protos {
        println!("cargo:rerun-if-changed={p}");
    }

    // 1. Pure-Rust .proto -> FileDescriptorSet. No external binary.
    let fds = protox::compile(protos, includes).map_err(|e| err(e.to_string()))?;
    let bytes = {
        use prost::Message as _;
        fds.encode_to_vec()
    };

    // 2. Persist it twice: OUT_DIR for codegen input, and the deterministic
    //    crate-root path for `boogy deploy` to tarball.
    let fds_out = out_dir.join("descriptor.bin");
    std::fs::write(&fds_out, &bytes).map_err(|e| err(e.to_string()))?;

    let dest = manifest_dir.join(DESCRIPTOR_REL_PATH);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| err(e.to_string()))?;
    }
    std::fs::write(&dest, &bytes).map_err(|e| err(e.to_string()))?;

    // 3. Codegen from the precompiled set — this is the call that would
    //    otherwise invoke protoc.
    //
    //    `Config::descriptor_set` requires `.files()` to name the
    //    proto-RELATIVE paths as they appear in the descriptor set (e.g.
    //    "my/service.proto"), not filesystem paths — the same convention
    //    `protox` used to name each file when it built the descriptor
    //    (relative to whichever `includes` entry contains it). Passing the
    //    filesystem paths straight through, unstripped, makes codegen fail
    //    with "file_to_generate ... not found in descriptor set" the moment
    //    a caller's `protos` argument isn't already include-relative (e.g.
    //    an absolute path) — caught by this crate's own gate test.
    let relative_files: Vec<String> = protos
        .iter()
        .map(|p| relative_to_includes(std::path::Path::new(p), includes))
        .collect();

    connectrpc_build::Config::new()
        .descriptor_set(&fds_out)
        .files(&relative_files)
        .includes(includes)
        .include_file(INCLUDE_FILE_NAME)
        .compile()
        .map_err(|e| err(e.to_string()))?;

    // 4. Guest contract: messages only (see the crate doc comment). Delete
    //    the one line that reaches connectrpc's RPC machinery — buffa-codegen
    //    emits it as the LAST line of each package's `<pkg>.mod.rs` stitcher,
    //    `include!("<stem>.__connect.rs");` — from every generated file it
    //    appears in. Matched by pattern (open on the file name, closed on
    //    the fixed `.__connect.rs");` suffix `connectrpc-build` always uses
    //    for that companion file), not by a fixed file name or line offset,
    //    so this holds across multiple protos/packages in one `compile()`
    //    call.
    strip_connect_includes(&out_dir).map_err(err)?;

    // 5. The guarantee, not a hope: walk everything actually reachable from
    //    the include root a guest's `boogy_sdk::include_protos!()` splices
    //    in (the same traversal a real `include!` chain performs) and refuse to
    //    succeed if `connectrpc::` still shows up anywhere in it. If step 4's
    //    pattern ever stops matching what upstream emits — a codegen
    //    version bump reshaping the include line, or a config change (e.g.
    //    `file_per_package`) that inlines the RPC machinery with no include
    //    line to strip at all — this is what turns that into a loud build
    //    failure instead of a guest silently acquiring the `connectrpc`
    //    dependency this crate exists to avoid.
    let entry = out_dir.join(INCLUDE_FILE_NAME);
    assert_messages_only(&out_dir, &entry).map_err(err)?;

    Ok(())
}

/// Remove every line matching `include!("<name>.__connect.rs");` (optional
/// leading whitespace) from every `.rs` file directly in `out_dir`. That line
/// is the sole path from a package's message/view stitcher to
/// `connectrpc-build`'s generated RPC machinery — deleting it, not the
/// companion file itself, is what makes it unreachable from a guest's
/// `boogy_sdk::include_protos!()` without disturbing anything else
/// buffa-codegen wrote.
fn strip_connect_includes(out_dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(out_dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading '{}': {e}", path.display()))?;
        let mut changed = false;
        let mut stripped = String::with_capacity(content.len());
        for line in content.lines() {
            if is_connect_include_line(line) {
                changed = true;
                continue;
            }
            stripped.push_str(line);
            stripped.push('\n');
        }
        if changed {
            std::fs::write(&path, stripped)
                .map_err(|e| format!("writing '{}': {e}", path.display()))?;
        }
    }
    Ok(())
}

/// `include!("<anything>.__connect.rs");`, ignoring leading whitespace —
/// the exact shape buffa-codegen emits for the RPC-companion include.
fn is_connect_include_line(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("include!(\"") && t.ends_with(".__connect.rs\");")
}

/// Confirm no file reachable from `entry` (following `include!` the same way
/// rustc's preprocessor would) mentions `connectrpc::`. Independent of
/// [`strip_connect_includes`] on purpose: this checks what actually ended up
/// on disk and reachable, not whether the stripping pass believes it did its
/// job.
fn assert_messages_only(out_dir: &Path, entry: &Path) -> Result<(), String> {
    let mut offenders = Vec::new();
    for path in reachable_files(out_dir, entry)? {
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading '{}': {e}", path.display()))?;
        if content.contains("connectrpc::") {
            offenders.push(path.display().to_string());
        }
    }
    if offenders.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "generated code still references connectrpc:: after stripping \
             the RPC-companion include — the guest-facing tree must be \
             messages-only (see the crate doc comment); offending file(s): {}",
            offenders.join(", ")
        ))
    }
}

/// Every file transitively reachable from `entry` by following the two
/// `include!` forms `connectrpc-build`/buffa-codegen emit between sibling
/// files in `out_dir`: `include!("name.rs");` and
/// `include!(concat!(env!("OUT_DIR"), "/name.rs"));`. Not a general Rust
/// parser — just enough text matching to walk this specific, deterministic
/// codegen output.
fn reachable_files(out_dir: &Path, entry: &Path) -> Result<Vec<PathBuf>, String> {
    let mut seen = HashSet::new();
    let mut stack = vec![entry.to_path_buf()];
    let mut files = Vec::new();
    while let Some(path) = stack.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading '{}': {e}", path.display()))?;
        for included in included_file_names(&content) {
            stack.push(out_dir.join(included));
        }
        files.push(path);
    }
    Ok(files)
}

/// Parse the file names out of both `include!` forms described on
/// [`reachable_files`], one per matching line.
fn included_file_names(content: &str) -> Vec<String> {
    content
        .lines()
        .filter_map(|line| {
            let t = line.trim();
            t.strip_prefix("include!(\"")
                .and_then(|rest| rest.strip_suffix("\");"))
                .or_else(|| {
                    t.strip_prefix("include!(concat!(env!(\"OUT_DIR\"), \"/")
                        .and_then(|rest| rest.strip_suffix("\"));"))
                })
                .map(str::to_string)
        })
        .collect()
}

/// Compute the proto-relative name `connectrpc_build::Config::files` (in
/// `descriptor_set` mode) expects: the longest matching `includes` prefix
/// stripped, falling back to the bare file name. Mirrors `protox`'s and
/// `protoc`'s own `--proto_path`-relative naming, and connectrpc-build's own
/// (private) `strip_include_prefix` used for its `protoc` source — so a
/// proto compiled via either `protoc` or `protox` gets the same descriptor
/// file name.
fn relative_to_includes(path: &std::path::Path, includes: &[&str]) -> String {
    let mut sorted: Vec<&str> = includes.to_vec();
    sorted.sort_by_key(|p| std::cmp::Reverse(p.len()));
    for inc in sorted {
        if let Ok(rel) = path.strip_prefix(inc) {
            if let Some(s) = rel.to_str() {
                return s.to_string();
            }
        }
    }
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string()
}

// A guest that wants to `include!` the code `compile()` generated does NOT
// reach for this crate — there is deliberately no `include_generated!()`
// macro here. A path-invoked `macro_rules!` (`boogy_proto_build::whatever!()`)
// resolves through the normal extern prelude, which is populated from
// `[dependencies]`, never `[build-dependencies]`; this crate is a
// build-dependency ONLY (see the crate doc comment on why: `protox` +
// `connectrpc-build` + `prost` + `prettyplease` must never enter a guest's
// real dependency graph). A macro living here is therefore a macro no
// author could ever call — Ruling F10 moved it to
// `boogy_sdk::include_protos!()` instead, which lives in a crate every
// service already depends on and whose body needs nothing from this one:
// just `include!(concat!(env!("OUT_DIR"), "/_connectrpc.rs")))`. The
// filename `_connectrpc.rs` (`INCLUDE_FILE_NAME` above) is this crate's
// implementation detail — `boogy_sdk::include_protos!()`'s doc comment says
// so explicitly, which is the whole reason that macro exists rather than
// asking every service author to spell the path themselves.
