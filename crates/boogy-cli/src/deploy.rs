use anyhow::Result;

use crate::provision;
use crate::smoke::SmokeOptions;

/// `boogy deploy` is sugar for publish + auto-provision the owner's own
/// service from the manifest in one shot.
pub async fn run(
    host: &str,
    token: &str,
    manifest_path: &str,
    replace: bool,
    smoke: SmokeOptions,
) -> Result<()> {
    println!("Deploying service (publish + provision)...");
    provision::publish(host, token, manifest_path, /* provision = */ true, replace, smoke).await
}
