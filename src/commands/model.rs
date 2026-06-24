//! `coop model` — switch a VM between cloud and local model backends.
//!
//! The selection is persisted per-VM ([`crate::model_state`]) and
//! materialized as guest config files (Claude `settings.json` env block,
//! Codex `config.toml` provider block) so every launch path picks it up.
//! Switching is a cheap SSH file write against the running VM — no
//! rebuild — and the persisted state is re-applied on the next start.

use std::io::Write;

use anyhow::{Result, bail};

use crate::backend::VmBackend as _;
use crate::config::{LocalModel, Secret};
use crate::model_state::{ModelMode, ModelState};
use crate::{backend, config, network, prompt};

pub(crate) fn cmd_model(
    be: &backend::PlatformBackend,
    cfg: &config::CoopConfig,
    name: Option<&config::InstanceName>,
    action: Option<ModelMode>,
) -> Result<()> {
    let inst = cfg.resolve_instance(name)?;
    match action {
        None => render_status(be, cfg, &inst),
        Some(ModelMode::Local) => set_local(be, cfg, &inst),
        Some(ModelMode::Remote) => set_remote(be, cfg, &inst),
    }
}

/// Print the per-tool mode and the endpoint each tool resolves to.
fn render_status(
    be: &backend::PlatformBackend,
    cfg: &config::CoopConfig,
    inst: &config::Instance,
) -> Result<()> {
    let state = ModelState::load_or_default(inst)?;
    let guest_host = be.guest_host_address(&cfg.network);
    let out = &mut std::io::stdout();
    writeln!(out, "Instance: {}", inst.name)?;
    writeln!(out, "Mode:     {}", state.mode.as_str())?;
    write_tool_line(
        out,
        "Claude",
        state.mode,
        state.resolved_claude(&cfg.claude),
        &guest_host,
    )?;
    write_tool_line(
        out,
        "Codex",
        state.mode,
        state.resolved_codex(&cfg.codex),
        &guest_host,
    )?;
    Ok(())
}

fn write_tool_line(
    out: &mut impl Write,
    label: &str,
    mode: ModelMode,
    endpoint: Option<&LocalModel>,
    guest_host: &str,
) -> Result<()> {
    match (mode, endpoint) {
        (ModelMode::Local, Some(ep)) => {
            let url = network::rewrite_host_url(ep.host_url(), guest_host)?;
            writeln!(out, "{label:<9} local — {} @ {}", ep.model(), url)?;
        }
        (ModelMode::Local, None) => {
            writeln!(out, "{label:<9} cloud (no local endpoint configured)")?;
        }
        (ModelMode::Remote, _) => writeln!(out, "{label:<9} cloud")?,
    }
    Ok(())
}

fn set_local(
    be: &backend::PlatformBackend,
    cfg: &config::CoopConfig,
    inst: &config::Instance,
) -> Result<()> {
    let mut state = ModelState::load_or_default(inst)?;
    state.mode = ModelMode::Local;

    // Only prompt when nothing is configured anywhere — the "VM exists,
    // no config yet" case. If at least one tool already resolves an
    // endpoint (from config.toml or a previous prompt), switch those and
    // leave the rest on cloud.
    if state.resolved_claude(&cfg.claude).is_none() && state.resolved_codex(&cfg.codex).is_none() {
        if let Some(ep) = prompt_endpoint("Claude")? {
            state.claude_endpoint = Some(ep);
        }
        if let Some(ep) = prompt_endpoint("Codex")? {
            state.codex_endpoint = Some(ep);
        }
    }

    if state.resolved_claude(&cfg.claude).is_none() && state.resolved_codex(&cfg.codex).is_none() {
        bail!(
            "No local model endpoint configured for '{}'.\n\
             Set [claude.local_model] or [codex.local_model] in config.toml, \
             or run `coop model {} local` in an interactive terminal to enter one.",
            inst.name,
            inst.name,
        );
    }

    // Remember that coop now owns Codex's config.toml model keys, so a
    // later switch to remote rewrites a clean file even if the configured
    // endpoint is later removed (Claude's settings.json is unconditional).
    if state.resolved_codex(&cfg.codex).is_some() {
        state.codex_materialized = true;
    }

    state.save(inst)?;
    let applied = apply_to_running(be, cfg, inst)?;
    report_switch(inst, ModelMode::Local, applied)
}

fn set_remote(
    be: &backend::PlatformBackend,
    cfg: &config::CoopConfig,
    inst: &config::Instance,
) -> Result<()> {
    let mut state = ModelState::load_or_default(inst)?;
    // Keep any saved endpoints so a later `local` doesn't re-prompt; only
    // the mode flips, which drops the materialized guest config.
    state.mode = ModelMode::Remote;
    state.save(inst)?;
    let applied = apply_to_running(be, cfg, inst)?;
    report_switch(inst, ModelMode::Remote, applied)
}

/// Tell the user what changed and what (if anything) they need to do.
///
/// Switching never requires a VM restart — the guest config is rewritten
/// live over SSH. An agent that is *already running* won't change
/// mid-session (it reads the config at launch), so the only follow-up is
/// to relaunch `claude`/`codex`.
fn report_switch(inst: &config::Instance, mode: ModelMode, applied: bool) -> Result<()> {
    let target = match mode {
        ModelMode::Local => "a local model",
        ModelMode::Remote => "cloud models",
    };
    let out = &mut std::io::stdout();
    if applied {
        writeln!(out, "'{}' now uses {target}.", inst.name)?;
        writeln!(
            out,
            "Applied to the running VM — no restart needed; relaunch claude/codex \
             (e.g. `coop claude {}`) to pick it up.",
            inst.name
        )?;
    } else {
        writeln!(out, "'{}' will use {target} on next start.", inst.name)?;
    }
    Ok(())
}

/// Re-materialize guest config for the current model state. Returns `true`
/// when the VM was running and the change was applied live, `false` when
/// the VM is stopped and the persisted state will apply on next start.
fn apply_to_running(
    be: &backend::PlatformBackend,
    cfg: &config::CoopConfig,
    inst: &config::Instance,
) -> Result<bool> {
    let Some(running) = be.as_running(cfg, inst.clone())? else {
        return Ok(false);
    };
    let repo = backend::detect_instance_repo(running.instance());
    let (inst, target) = running.into_parts();
    let session = super::prepare_session_from_target(cfg, Some(&inst), target, repo.as_ref())?;
    let guest_host = be.guest_host_address(&cfg.network);
    backend::bootstrap_agents(
        &session,
        cfg,
        &inst,
        backend::BootMode::Restart,
        &guest_host,
    )?;
    Ok(true)
}

/// Interactively collect a local endpoint for `tool`. Returns `None` when
/// the user declines or stdin is not a TTY.
fn prompt_endpoint(tool: &str) -> Result<Option<LocalModel>> {
    if !prompt::confirm(&format!("Configure a local model for {tool}?"))? {
        return Ok(None);
    }
    let Some(host_url) =
        prompt::read_line(&format!("{tool} host URL (e.g. http://localhost:11434)"))?
    else {
        bail!("{tool} host URL is required");
    };
    let url = url::Url::parse(&host_url)
        .map_err(|e| anyhow::anyhow!("invalid {tool} host URL '{host_url}': {e}"))?;
    let Some(model) = prompt::read_line(&format!("{tool} model name"))? else {
        bail!("{tool} model name is required");
    };
    let token = prompt::read_line(&format!(
        "{tool} auth token (optional — press enter to skip)"
    ))?;
    let endpoint = LocalModel::new(url, model, token.map(Secret::new))?;
    Ok(Some(endpoint))
}
