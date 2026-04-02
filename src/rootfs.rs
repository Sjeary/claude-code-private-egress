use std::fs;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::CoopConfig;

/// Build the rootfs image and fetch the kernel.
pub fn build(cfg: &CoopConfig) -> Result<()> {
    fs::create_dir_all(&cfg.data_dir).context("Failed to create data directory")?;

    fetch_kernel(cfg)?;
    build_rootfs(cfg)?;
    Ok(())
}

/// Download a prebuilt Firecracker-compatible kernel from AWS.
fn fetch_kernel(cfg: &CoopConfig) -> Result<()> {
    if cfg.vm.kernel_path.exists() {
        tracing::info!("Kernel already present at {}", cfg.vm.kernel_path.display());
        return Ok(());
    }

    let script = project_root()?.join("scripts/fetch-kernel.sh");
    if !script.exists() {
        bail!("Kernel fetch script not found at {}", script.display());
    }

    tracing::info!("Fetching Firecracker-compatible kernel");
    let status = Command::new("bash")
        .arg(&script)
        .arg(&cfg.vm.kernel_path)
        .status()
        .context("Failed to run kernel fetch script")?;

    if !status.success() {
        bail!("Kernel fetch script failed");
    }

    Ok(())
}

/// Build the rootfs ext4 image using the build script.
fn build_rootfs(cfg: &CoopConfig) -> Result<()> {
    let template = cfg.template_path();
    if template.exists() {
        tracing::info!("Template rootfs already present at {}", template.display());
        return Ok(());
    }

    let script = project_root()?.join("scripts/build-rootfs.sh");
    if !script.exists() {
        bail!("Rootfs build script not found at {}", script.display());
    }

    tracing::info!("Building rootfs image (this may take several minutes)");
    let status = Command::new("bash")
        .arg(&script)
        .arg(&template)
        .status()
        .context("Failed to run rootfs build script")?;

    if !status.success() {
        bail!("Rootfs build script failed");
    }

    Ok(())
}

/// Locate the project root (directory containing Cargo.toml).
fn project_root() -> Result<std::path::PathBuf> {
    let mut dir = std::env::current_exe().context("Failed to get executable path")?;

    // Walk up from the binary location to find the project root.
    // In development, the binary is in target/debug/ or target/release/.
    for _ in 0..5 {
        dir.pop();
        if dir.join("Cargo.toml").exists() {
            return Ok(dir);
        }
    }

    // Fall back to current working directory
    let cwd = std::env::current_dir().context("Failed to get current directory")?;
    if cwd.join("Cargo.toml").exists() {
        return Ok(cwd);
    }

    bail!("Could not locate project root (directory containing Cargo.toml)");
}
