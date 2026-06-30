#![cfg(target_os = "macos")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};

#[test]
fn copy_pull_round_trip_does_not_create_appledouble_files() -> Result<()> {
    let coop = PathBuf::from(env!("CARGO_BIN_EXE_coop"));
    let tmp = tempfile::tempdir().context("create tempdir")?;
    let repo = tmp.path().join("repo");
    let pulled = tmp.path().join("pulled");
    let name = format!("appledouble-e2e-{}", std::process::id());

    create_git_repo_with_xattr(&repo)?;
    if let Some(sidecar) = first_appledouble_file(&repo)? {
        bail!("source fixture contains AppleDouble sidecar: {sidecar:?}");
    }

    destroy_instance(&coop, &name);

    // Use a Drop guard so assertion failures after `coop up` still destroy the VM.
    let _cleanup = VmCleanup {
        coop: coop.clone(),
        name: name.clone(),
    };
    run_ok(
        Command::new(&coop)
            .arg("up")
            .arg(&repo)
            .arg("--name")
            .arg(&name)
            .arg("--copy")
            .arg("--no-agents")
            .arg("--no-prompt")
            .arg("--no-devcontainer"),
    )?;
    fs::create_dir(&pulled).context("create pull dir")?;
    run_ok(
        Command::new(&coop)
            .arg("pull")
            .arg(&name)
            .arg("--dir")
            .arg(&pulled)
            .arg("--force"),
    )?;

    if let Some(sidecar) = first_appledouble_file(&pulled)? {
        bail!("pulled checkout contains AppleDouble sidecar: {sidecar:?}");
    }

    Ok(())
}

struct VmCleanup {
    coop: PathBuf,
    name: String,
}

impl Drop for VmCleanup {
    fn drop(&mut self) {
        destroy_instance(&self.coop, &self.name);
    }
}

fn destroy_instance(coop: &Path, name: &str) {
    let _ = Command::new(coop).arg("destroy").arg(name).output();
}

fn create_git_repo_with_xattr(repo: &Path) -> Result<()> {
    fs::create_dir(repo).context("create repo")?;
    run_ok(Command::new("git").arg("init").arg(repo))?;
    run_ok(Command::new("git").arg("-C").arg(repo).args([
        "config",
        "user.email",
        "test@example.com",
    ]))?;
    run_ok(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["config", "user.name", "Coop Test"]),
    )?;

    let tracked = repo.join("AGENTS.md");
    fs::write(&tracked, "hi\n").context("write fixture file")?;
    run_ok(Command::new("git").arg("-C").arg(repo).arg("add").arg("."))?;
    run_ok(
        Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["commit", "-m", "init"]),
    )?;
    run_ok(
        Command::new("xattr")
            .args(["-w", "com.example.coop-test", "value"])
            .arg(&tracked),
    )?;
    Ok(())
}

fn first_appledouble_file(root: &Path) -> Result<Option<PathBuf>> {
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir).context("read dir")? {
            let path = entry.context("read dir entry")?.path();
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("._"))
            {
                return Ok(Some(path));
            }
            if path.is_dir() {
                dirs.push(path);
            }
        }
    }
    Ok(None)
}

fn run_ok(cmd: &mut Command) -> Result<()> {
    let output = cmd
        .output()
        .with_context(|| format!("run command {cmd:?}"))?;
    if !output.status.success() {
        bail!(
            "command failed with {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
