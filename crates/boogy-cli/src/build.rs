use std::process::Command;

use anyhow::{Context, Result, bail};

pub async fn run(path: &str) -> Result<()> {
    println!("Building service at: {path}");

    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-wasip2", "--release"])
        .current_dir(path)
        .status()
        .context("failed to run cargo build")?;

    if !status.success() {
        bail!("cargo build failed with status: {status}");
    }

    println!("Build successful.");
    println!("Output: target/wasm32-wasip2/release/*.wasm");
    Ok(())
}
