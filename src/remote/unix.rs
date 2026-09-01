//! Remote thin-client launcher over SSH command stdio.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, IsTerminal, Write as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};

use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex, OnceLock,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const BRIDGE_ACCEPT_POLL: Duration = Duration::from_millis(50);
const BRIDGE_SOCKET_PERMISSION_MODE: u32 = 0o600;
const REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);
const REMOTE_SERVER_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(100);
const CURRENT_PROTOCOL: u32 = crate::protocol::PROTOCOL_VERSION;
const STABLE_UPDATE_MANIFEST_URL: &str = "https://herdr.dev/latest.json";
const PREVIEW_UPDATE_MANIFEST_URL: &str = "https://herdr.dev/preview.json";
const REMOTE_BINARY_ENV_VAR: &str = "HERDR_REMOTE_BINARY";
const SSH_CONTROL_SOCKET_NAME: &str = "ctl";
// `ConnectTimeout` is parameterized per host (see `RemoteSsh::connect_timeout_secs`);
// these remaining noninteractive options stay static.
const NONINTERACTIVE_SSH_OPTIONS: &[&str] = &[
    "-o",
    "BatchMode=yes",
    "-o",
    "ServerAliveInterval=5",
    "-o",
    "ServerAliveCountMax=2",
];
pub(crate) const REATTACH_COMMAND_ENV_VAR: &str = "HERDR_REATTACH_COMMAND";
pub(crate) const REMOTE_CLIENT_BRIDGE_SUBCOMMAND: &str = "remote-client-bridge";
/// Fail-closed/no-start sibling of [`REMOTE_CLIENT_BRIDGE_SUBCOMMAND`] run on
/// the remote host for in-place terminal-session projection streams: it
/// never starts, stops, sets up, installs, updates, or wakes the remote
/// Herdr server (see [`run_remote_client_bridge_no_start`]).
pub(crate) const REMOTE_CLIENT_BRIDGE_NO_START_SUBCOMMAND: &str = "remote-client-bridge-no-start";
pub(crate) const REMOTE_API_BRIDGE_SUBCOMMAND: &str = "remote-api-bridge";
/// CLI flag that selects the persistent remote-API bridge loop
/// ([`run_remote_api_bridge`]). Bare `remote-api-bridge` (no flag) keeps the
/// one-shot stdio socket bridge behavior; `remote-api-bridge --persistent`
/// runs the one-request-per-API-socket loop reused by the local bridge pool.
pub(crate) const REMOTE_API_BRIDGE_PERSISTENT_FLAG: &str = "--persistent";
pub(crate) const REMOTE_FEDERATION_CAPABILITIES_SUBCOMMAND: &str = "remote-federation-capabilities";
pub(crate) const REMOTE_API_STATUS_SUBCOMMAND: &str = "remote-api-status";
pub(crate) const REMOTE_API_PING_SUBCOMMAND: &str = "remote-api-ping";
pub(crate) const REMOTE_API_AGENT_LIST_SUBCOMMAND: &str = "remote-api-agent-list";

pub(crate) const REMOTE_KEYBINDINGS_ENV_VAR: &str = "HERDR_REMOTE_KEYBINDINGS";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteKeybindings {
    Local,
    Server,
}

impl RemoteKeybindings {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "local" => Ok(Self::Local),
            "server" => Ok(Self::Server),
            _ => Err("--remote-keybindings must be 'local' or 'server'".to_string()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Server => "server",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteLaunch {
    pub(crate) target: String,
    pub(crate) keybindings: RemoteKeybindings,
    pub(crate) live_handoff: bool,
}

pub(crate) fn extract_remote_args(
    args: &[String],
) -> Result<(Vec<String>, Option<RemoteLaunch>), String> {
    let mut cleaned = Vec::with_capacity(args.len());
    if let Some(program) = args.first() {
        cleaned.push(program.clone());
    }

    let mut remote_target = None;
    let mut keybindings = RemoteKeybindings::Local;
    let mut keybindings_seen = false;
    let mut live_handoff = false;
    let mut index = 1;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            cleaned.extend_from_slice(&args[index..]);
            break;
        }
        if arg == "--handoff" {
            live_handoff = true;
            index += 1;
            continue;
        }
        if arg == "--remote" {
            if remote_target.is_some() {
                return Err("--remote can only be specified once".to_string());
            }
            let Some(value) = args.get(index + 1) else {
                return Err("missing value for --remote".to_string());
            };
            remote_target = Some(validate_remote_target(value)?.to_owned());
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--remote=") {
            if remote_target.is_some() {
                return Err("--remote can only be specified once".to_string());
            }
            remote_target = Some(validate_remote_target(value)?.to_owned());
            index += 1;
            continue;
        }
        if arg == "--remote-keybindings" {
            if keybindings_seen {
                return Err("--remote-keybindings can only be specified once".to_string());
            }
            let Some(value) = args.get(index + 1) else {
                return Err("missing value for --remote-keybindings".to_string());
            };
            keybindings = RemoteKeybindings::parse(value)?;
            keybindings_seen = true;
            index += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--remote-keybindings=") {
            if keybindings_seen {
                return Err("--remote-keybindings can only be specified once".to_string());
            }
            keybindings = RemoteKeybindings::parse(value)?;
            keybindings_seen = true;
            index += 1;
            continue;
        }

        cleaned.push(arg.clone());
        index += 1;
    }

    let remote = remote_target.map(|target| RemoteLaunch {
        target,
        keybindings,
        live_handoff,
    });
    if remote.is_none() && keybindings_seen {
        return Err("--remote-keybindings requires --remote".to_string());
    }
    if remote.is_none() && live_handoff {
        cleaned.push("--handoff".to_string());
    }

    Ok((cleaned, remote))
}

fn validate_remote_target(target: &str) -> Result<&str, String> {
    validate_remote_target_for(target, "--remote")
}

fn validate_remote_target_for<'a>(target: &'a str, label: &str) -> Result<&'a str, String> {
    if target.is_empty() {
        return Err(format!("missing value for {label}"));
    }
    if target.starts_with('-') {
        return Err(format!("{label} target must not start with '-'"));
    }
    Ok(target)
}

pub(crate) fn run_remote(remote: RemoteLaunch) -> io::Result<()> {
    let session_name = crate::session::active_name()
        .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_string());
    let local_socket = local_forward_socket_path(&remote.target, &session_name);
    let program = std::env::args()
        .next()
        .unwrap_or_else(|| "herdr".to_string());
    let reattach_command = reattach_command(
        &program,
        &remote.target,
        &session_name,
        remote.keybindings,
        remote.live_handoff,
    );
    let manage_ssh_config = crate::config::Config::load()
        .config
        .remote
        .manage_ssh_config;
    let remote_ssh = RemoteSsh::new_for_target(remote.target.clone(), manage_ssh_config);
    let prepared_remote = prepare_remote_herdr(&remote_ssh, remote.live_handoff)?;
    ensure_remote_server_ready(
        &remote_ssh,
        &prepared_remote.remote_herdr,
        prepared_remote.installed_or_replaced,
        prepared_remote.stop_after_install_approved,
        remote.live_handoff,
    )?;

    let _bridge = SshStdioBridge::start(
        remote.target,
        prepared_remote.remote_herdr,
        local_socket.clone(),
        session_name,
        remote_ssh.options(),
        None,
    )?;

    run_client_process(&local_socket, &reattach_command, remote.keybindings)
}

fn remote_ssh_for_host(host: &crate::remote_target::RemoteHostConfig) -> RemoteSsh {
    let manage_ssh_config = crate::config::Config::load()
        .config
        .remote
        .manage_ssh_config;
    RemoteSsh::new_for_host(
        host.target.clone(),
        manage_ssh_config,
        host.connect_timeout_secs,
    )
}

pub(crate) fn run_remote_terminal_attach(
    host: &crate::remote_target::RemoteHostConfig,
    terminal_id: String,
    takeover: bool,
) -> io::Result<()> {
    let remote_ssh = remote_ssh_for_host(host);
    let prepared_remote = prepare_remote_herdr(&remote_ssh, false)?;
    ensure_remote_server_ready(
        &remote_ssh,
        &prepared_remote.remote_herdr,
        prepared_remote.installed_or_replaced,
        prepared_remote.stop_after_install_approved,
        false,
    )?;
    ensure_remote_federation_methods(
        host,
        &remote_ssh,
        &prepared_remote.remote_herdr,
        SshInvocationMode::Interactive,
        &[crate::api::schema::FederationCapabilities::TERMINAL_ATTACH],
    )?;

    let local_socket = remote_client_attach_socket_path(host);
    let _bridge = SshStdioBridge::start(
        host.target.clone(),
        prepared_remote.remote_herdr,
        local_socket.clone(),
        host.session.clone(),
        remote_ssh.options(),
        Some(host.connect_timeout_secs),
    )?;

    crate::client::run_terminal_attach_at_socket(&local_socket, terminal_id, takeover)
}

pub(crate) fn run_remote_api_ping(args: &[String]) -> io::Result<()> {
    run_remote_api_one_shot_probe(args, REMOTE_API_PING_SUBCOMMAND, remote_api_ping_request())
}

pub(crate) fn run_remote_api_agent_list(args: &[String]) -> io::Result<()> {
    run_remote_api_one_shot_probe(
        args,
        REMOTE_API_AGENT_LIST_SUBCOMMAND,
        remote_api_agent_list_request(),
    )
}

pub(crate) fn run_remote_federation_capabilities() -> io::Result<()> {
    let capabilities = crate::api::schema::FederationCapabilities::current();
    let json = serde_json::to_string(&capabilities).map_err(io::Error::other)?;
    println!("{json}");
    Ok(())
}

pub(crate) fn run_remote_api_status() -> io::Result<()> {
    let response = match crate::api::read_runtime_status_at(
        &crate::api::socket_path(),
        Duration::from_millis(500),
    )? {
        Some(status) => RemoteApiStatusResponse {
            state: RemoteApiStatusState::Running,
            version: status.version,
            protocol: status.protocol,
            capabilities: status.capabilities,
        },
        None => RemoteApiStatusResponse {
            state: RemoteApiStatusState::NotRunning,
            version: None,
            protocol: None,
            capabilities: None,
        },
    };
    println!(
        "{}",
        serde_json::to_string(&response).map_err(io::Error::other)?
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RemoteApiStatusResponse {
    pub(crate) state: RemoteApiStatusState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) capabilities: Option<crate::api::schema::ServerCapabilities>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RemoteApiStatusState {
    Running,
    NotRunning,
}

fn run_remote_api_one_shot_probe(
    args: &[String],
    subcommand: &str,
    request: crate::api::schema::Request,
) -> io::Result<()> {
    let target = parse_remote_api_probe_target(args, subcommand).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{err}\nusage: herdr {subcommand} <ssh-target>"),
        )
    })?;
    let session_name = crate::session::active_name()
        .unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.to_string());
    let host = crate::remote_target::RemoteHostConfig::new(target, target, session_name, true);
    let response = send_remote_api_request_to_host(&host, &request)?;
    println!("{response}");
    Ok(())
}

fn parse_remote_api_probe_target<'a>(args: &'a [String], label: &str) -> Result<&'a str, String> {
    let [target] = args else {
        return Err("expected exactly one SSH target".to_string());
    };

    validate_remote_target_for(target, label)
}

fn remote_api_ping_request() -> crate::api::schema::Request {
    crate::api::schema::Request {
        id: REMOTE_API_PING_SUBCOMMAND.into(),
        method: crate::api::schema::Method::Ping(crate::api::schema::PingParams::default()),
    }
}

fn remote_api_agent_list_request() -> crate::api::schema::Request {
    crate::api::schema::Request {
        id: REMOTE_API_AGENT_LIST_SUBCOMMAND.into(),
        method: crate::api::schema::Method::AgentList(crate::api::schema::EmptyParams::default()),
    }
}

fn write_remote_api_request<W: io::Write>(
    writer: &mut W,
    request: &crate::api::schema::Request,
) -> io::Result<()> {
    let request = serde_json::to_string(request).map_err(io::Error::other)?;
    writer.write_all(request.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn read_remote_api_response_line<R: BufRead>(reader: &mut R) -> io::Result<String> {
    let mut response = String::new();
    let bytes_read = reader.read_line(&mut response)?;
    if bytes_read == 0 || response.trim().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "remote API ping returned an empty response",
        ));
    }

    Ok(response.trim_end_matches(['\r', '\n']).to_string())
}

pub(crate) fn send_remote_api_request_to_host(
    host: &crate::remote_target::RemoteHostConfig,
    request: &crate::api::schema::Request,
) -> io::Result<String> {
    let remote_ssh = remote_ssh_for_host(host);
    let prepared_remote = prepare_remote_herdr(&remote_ssh, false)?;
    ensure_remote_server_ready(
        &remote_ssh,
        &prepared_remote.remote_herdr,
        prepared_remote.installed_or_replaced,
        prepared_remote.stop_after_install_approved,
        false,
    )?;

    let (response, _capabilities) = send_remote_api_request_to_host_with_mode(
        host,
        &remote_ssh,
        &prepared_remote.remote_herdr,
        request,
        SshInvocationMode::Interactive,
    )?;
    Ok(response)
}

/// Explicitly prepare a configured remote host's Herdr for use (the
/// `herdr remote setup <HOST>` path).
///
/// This is a thin configured-host wrapper over the existing interactive remote
/// preparation pipeline. It reuses [`remote_ssh_for_host`],
/// [`prepare_remote_herdr`] (which detects the remote platform and finds,
/// installs, or updates a compatible Herdr binary using the existing
/// confirmation prompts), and [`ensure_remote_server_ready`] (which uses the
/// existing confirmed-stop / live-handoff path). It then confirms the remote
/// server/API bridge is usable by sending an existing capability-gated
/// [`remote_api_ping_request`] over the interactive path, exactly as a routed
/// agent request would. When `live_handoff` is true it is threaded into the
/// prepare/ensure steps so the existing live-handoff path fires if the remote
/// server advertises it.
///
/// No new SSH shell-command shapes, confirmation flows, or remote mutation
/// behavior are introduced beyond the existing helpers. On success it returns
/// the prepared [`RemoteHerdr`]; the CLI layer surfaces the alias, target,
/// session, and `shell_path()`. The caller is responsible for resolving the
/// configured host and rejecting disabled federation / unknown hosts /
/// invalid config before calling this.
pub(crate) fn setup_remote_host_interactive(
    host: &crate::remote_target::RemoteHostConfig,
    live_handoff: bool,
) -> io::Result<RemoteHerdr> {
    let remote_ssh = remote_ssh_for_host(host);
    let prepared_remote = prepare_remote_herdr(&remote_ssh, live_handoff)?;
    ensure_remote_server_ready(
        &remote_ssh,
        &prepared_remote.remote_herdr,
        prepared_remote.installed_or_replaced,
        prepared_remote.stop_after_install_approved,
        live_handoff,
    )?;
    // Confirm the remote server/API bridge is usable via an existing
    // capability-gated ping over the interactive bridge path. This is the same
    // round-trip a routed agent request performs; it does no new mutation.
    send_remote_api_request_to_host_with_mode(
        host,
        &remote_ssh,
        &prepared_remote.remote_herdr,
        &remote_api_ping_request(),
        SshInvocationMode::Interactive,
    )?;
    Ok(prepared_remote.remote_herdr)
}

pub(crate) fn send_remote_api_request_to_host_noninteractive(
    host: &crate::remote_target::RemoteHostConfig,
    request: &crate::api::schema::Request,
) -> io::Result<String> {
    let remote_ssh = remote_ssh_for_host(host);
    let remote_herdr = prepare_remote_herdr_noninteractive(&remote_ssh)?;
    let (response, _capabilities) = send_remote_api_request_to_host_with_mode(
        host,
        &remote_ssh,
        &remote_herdr,
        request,
        SshInvocationMode::Noninteractive,
    )?;
    Ok(response)
}

/// Like [`send_remote_api_request_to_host_noninteractive`] but also returns the
/// prepared [`RemoteApiBridgeState`] captured on the successful round-trip
/// (prepared remote Herdr shell path plus advertised federation capabilities).
/// A connected remote-source supervisor publishes this state so routed agent
/// dispatch can reuse it instead of redoing per-request binary preparation and
/// capability/ping probes. This reuses already-prepared data; it is not
/// connection pooling and does not persist an SSH bridge between requests.
pub(crate) fn send_remote_api_request_to_host_noninteractive_with_state(
    host: &crate::remote_target::RemoteHostConfig,
    request: &crate::api::schema::Request,
) -> io::Result<(String, RemoteApiBridgeState)> {
    let remote_ssh = remote_ssh_for_host(host);
    let remote_herdr = prepare_remote_herdr_noninteractive(&remote_ssh)?;
    let (response, capabilities) = send_remote_api_request_to_host_with_mode(
        host,
        &remote_ssh,
        &remote_herdr,
        request,
        SshInvocationMode::Noninteractive,
    )?;
    Ok((
        response,
        RemoteApiBridgeState {
            shell_path: remote_herdr.shell_path.clone(),
            capabilities,
        },
    ))
}

/// Send a remote API request reusing cached supervisor-prepared bridge state.
///
/// Validates the cached full federation capabilities against the request's
/// required method locally (using the same method mapping as the current
/// bridge path), then builds the `remote-api-bridge` command from the cached
/// prepared shell path and sends the actual request without re-running remote
/// binary preparation, the `remote-federation-capabilities` probe, or the API
/// ping probe. A fresh SSH process is still spawned for the request itself, so
/// this reuses prepared *data*, not a persistent connection. The actual remote
/// API request still fails authoritatively on drift, mapped through the existing
/// remote error handling by the caller.
pub(crate) fn send_remote_api_request_with_prepared_state(
    host: &crate::remote_target::RemoteHostConfig,
    state: &RemoteApiBridgeState,
    request: &crate::api::schema::Request,
) -> io::Result<String> {
    let required_methods = required_federation_methods_for_request(request);
    // Local cached-capability check first: preserves today's early clean error
    // before any SSH work, using the same required-method mapping as the full
    // bridge path.
    validate_federation_capabilities(host, &state.capabilities, &required_methods)?;

    let remote_ssh = remote_ssh_for_host(host);
    let bridge_command = remote_bridge_command_for_shell_path(
        &state.shell_path,
        &host.session,
        REMOTE_API_BRIDGE_SUBCOMMAND,
    );
    send_remote_api_request_with_mode(
        &remote_ssh,
        &bridge_command,
        request,
        SshInvocationMode::Noninteractive,
        None,
    )
}

/// Starts a local bridge listener for in-place terminal-session projection
/// streams (`ObserveTerminal` / `ControlTerminal` over the render/client
/// bridge), fail-closed and no-start.
///
/// This is the projection-specific sibling of [`SshStdioBridge::start`]: it
/// reuses already-cached supervisor-prepared bridge state
/// (`RemoteApiBridgeState`, published only from a successful supervisor ping
/// — see `RemoteSourceCache::connected_bridge_state`) instead of running any
/// remote binary preparation, capability probing, or server-readiness/setup
/// step, and it dispatches the remote-side
/// [`REMOTE_CLIENT_BRIDGE_NO_START_SUBCOMMAND`], which never calls
/// `ensure_remote_server_running`. Selecting/projecting a host can therefore
/// never start, stop, set up, install, update, or wake the remote Herdr
/// server: if the remote session is not already running, every accepted
/// connection simply fails to attach instead of starting one. Callers must
/// only invoke this for a host with cached prepared state (a `Connected`
/// host); there is no fallback path here that re-runs preparation.
pub(crate) fn start_projection_bridge(
    host: &crate::remote_target::RemoteHostConfig,
    state: &RemoteApiBridgeState,
    local_socket: PathBuf,
    max_concurrent: usize,
) -> io::Result<SshStdioBridge> {
    let remote_ssh = remote_ssh_for_host(host);
    let ssh_options = remote_ssh.options().cloned();
    let bridge_command = remote_bridge_command_for_shell_path(
        &state.shell_path,
        &host.session,
        REMOTE_CLIENT_BRIDGE_NO_START_SUBCOMMAND,
    );
    SshStdioBridge::start_with_bridge_command(
        host.target.clone(),
        bridge_command,
        local_socket,
        ssh_options.as_ref(),
        Some(host.connect_timeout_secs),
        max_concurrent,
        Some(remote_ssh),
        PathBuf::from("ssh"),
    )
}

/// Test-only real listener/worker seam for projection connector turnover.
/// The supplied executable stands in for `ssh` while the production bridge
/// accept/capacity/worker machinery remains unmocked.
#[cfg(test)]
pub(crate) fn start_test_projection_bridge(
    local_socket: PathBuf,
    max_concurrent: usize,
    ssh_program: PathBuf,
) -> io::Result<SshStdioBridge> {
    SshStdioBridge::start_with_bridge_command(
        "ignored-target".into(),
        "ignored-command".into(),
        local_socket,
        None,
        None,
        max_concurrent,
        None,
        ssh_program,
    )
}

pub(crate) fn prepare_remote_binary_to_host_noninteractive(
    host: &crate::remote_target::RemoteHostConfig,
) -> io::Result<RemoteHerdr> {
    let remote_ssh = remote_ssh_for_host(host);
    prepare_remote_herdr_noninteractive(&remote_ssh)
}

pub(crate) fn remote_federation_capabilities_for_prepared_host_noninteractive(
    host: &crate::remote_target::RemoteHostConfig,
    remote_herdr: &RemoteHerdr,
) -> io::Result<crate::api::schema::FederationCapabilities> {
    let remote_ssh = remote_ssh_for_host(host);
    fetch_remote_federation_capabilities(
        host,
        &remote_ssh,
        remote_herdr,
        SshInvocationMode::Noninteractive,
    )
}

pub(crate) fn remote_api_status_to_host_noninteractive(
    host: &crate::remote_target::RemoteHostConfig,
) -> io::Result<RemoteApiStatusResponse> {
    let remote_herdr = prepare_remote_binary_to_host_noninteractive(host)?;
    remote_api_status_for_prepared_host_noninteractive(host, &remote_herdr)
}

pub(crate) fn remote_api_status_for_prepared_host_noninteractive(
    host: &crate::remote_target::RemoteHostConfig,
    remote_herdr: &RemoteHerdr,
) -> io::Result<RemoteApiStatusResponse> {
    let remote_ssh = remote_ssh_for_host(host);
    let command =
        remote_bridge_command_for(remote_herdr, &host.session, REMOTE_API_STATUS_SUBCOMMAND);
    let output =
        remote_ssh.user_shell_output_with_mode(&command, SshInvocationMode::Noninteractive)?;
    parse_remote_api_status_output(host, &output)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteFailureClass {
    NeedsUpdate,
    Unreachable,
    Unknown,
}

pub(crate) fn classify_remote_failure(err: &io::Error) -> RemoteFailureClass {
    match err.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::NotFound => RemoteFailureClass::NeedsUpdate,
        io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::TimedOut
        | io::ErrorKind::WouldBlock
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::PermissionDenied => RemoteFailureClass::Unreachable,
        _ if looks_like_ssh_transport_error(&err.to_string()) => RemoteFailureClass::Unreachable,
        _ => RemoteFailureClass::Unknown,
    }
}

pub(crate) fn looks_like_ssh_transport_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "ssh",
        "permission denied",
        "could not resolve hostname",
        "name or service not known",
        "connection timed out",
        "connection refused",
        "connection reset",
        "no route to host",
        "remote platform detection failed: exit status: 255",
        "exit status: 255",
        "host key verification failed",
        "known_hosts",
        "publickey",
        "batchmode",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn parse_remote_api_status_output(
    host: &crate::remote_target::RemoteHostConfig,
    output: &Output,
) -> io::Result<RemoteApiStatusResponse> {
    if !output.status.success() {
        let detail = command_output_detail(output)
            .unwrap_or_else(|| format!("ssh remote API status exited with {}", output.status));
        let lower = detail.to_ascii_lowercase();
        let kind = if lower.contains("unknown command") || lower.contains("usage:") {
            io::ErrorKind::InvalidData
        } else {
            io::ErrorKind::ConnectionAborted
        };
        return Err(io::Error::new(
            kind,
            format!(
                "remote host {} API status probe failed: {detail}",
                host.name
            ),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(stdout.trim()).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "remote host {} returned invalid API status JSON: {err}",
                host.name
            ),
        )
    })
}

fn send_remote_api_request_to_host_with_mode(
    host: &crate::remote_target::RemoteHostConfig,
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    request: &crate::api::schema::Request,
    mode: SshInvocationMode,
) -> io::Result<(String, crate::api::schema::FederationCapabilities)> {
    let required_methods = required_federation_methods_for_request(request);
    // The advertised capabilities from the successful federation probe are the
    // prepared bridge state a connected supervisor cache may reuse to skip
    // per-request probes, so capture them here alongside the response.
    let capabilities =
        ensure_remote_federation_methods(host, ssh, remote_herdr, mode, &required_methods)?;

    let bridge_command = remote_api_bridge_command_for_host(remote_herdr, host);
    if matches!(request.method, crate::api::schema::Method::Ping(_)) {
        let response =
            send_remote_api_request_with_mode(ssh, &bridge_command, request, mode, None)?;
        validate_remote_api_ping_capabilities(host, &response, &required_methods)?;
        return Ok((response, capabilities));
    }

    validate_remote_api_capabilities_with_mode(
        host,
        ssh,
        &bridge_command,
        mode,
        &required_methods,
    )?;
    let response = send_remote_api_request_with_mode(ssh, &bridge_command, request, mode, None)?;
    Ok((response, capabilities))
}

fn ensure_remote_federation_methods(
    host: &crate::remote_target::RemoteHostConfig,
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    mode: SshInvocationMode,
    required_methods: &[&'static str],
) -> io::Result<crate::api::schema::FederationCapabilities> {
    let capabilities = fetch_remote_federation_capabilities(host, ssh, remote_herdr, mode)?;
    validate_federation_capabilities(host, &capabilities, required_methods)?;
    Ok(capabilities)
}

fn fetch_remote_federation_capabilities(
    host: &crate::remote_target::RemoteHostConfig,
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    mode: SshInvocationMode,
) -> io::Result<crate::api::schema::FederationCapabilities> {
    let command = remote_bridge_command_for(
        remote_herdr,
        &host.session,
        REMOTE_FEDERATION_CAPABILITIES_SUBCOMMAND,
    );
    let output = ssh.user_shell_output_with_mode(&command, mode)?;
    parse_remote_federation_capabilities_probe_output(host, &output)
}

fn parse_remote_federation_capabilities_probe_output(
    host: &crate::remote_target::RemoteHostConfig,
    output: &Output,
) -> io::Result<crate::api::schema::FederationCapabilities> {
    if !output.status.success() {
        return Err(federation_not_advertised_error(
            host,
            command_output_detail(output),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_remote_federation_capabilities_json(&stdout).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "remote host {} returned invalid federation capabilities: {err}",
                host.name
            ),
        )
    })
}

fn parse_remote_federation_capabilities_json(
    json: &str,
) -> io::Result<crate::api::schema::FederationCapabilities> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "empty federation capabilities response",
        ));
    }
    serde_json::from_str(trimmed).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

fn validate_remote_api_capabilities_with_mode(
    host: &crate::remote_target::RemoteHostConfig,
    ssh: &RemoteSsh,
    bridge_command: &str,
    mode: SshInvocationMode,
    required_methods: &[&'static str],
) -> io::Result<()> {
    let response = send_remote_api_request_with_mode(
        ssh,
        bridge_command,
        &remote_api_ping_request(),
        mode,
        None,
    )?;
    validate_remote_api_ping_capabilities(host, &response, required_methods)
}

fn validate_remote_api_ping_capabilities(
    host: &crate::remote_target::RemoteHostConfig,
    response: &str,
    required_methods: &[&'static str],
) -> io::Result<()> {
    let value: serde_json::Value = serde_json::from_str(response).map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid remote API ping response JSON: {err}"),
        )
    })?;
    let response = crate::api::client::parse_response_value(value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    let crate::api::schema::ResponseResult::Pong { capabilities, .. } = response.result else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote API ping did not return pong",
        ));
    };
    let Some(federation) = capabilities.and_then(|capabilities| capabilities.federation) else {
        return Err(federation_not_advertised_error(host, None));
    };

    validate_federation_capabilities(host, &federation, required_methods)
}

fn validate_federation_capabilities(
    host: &crate::remote_target::RemoteHostConfig,
    capabilities: &crate::api::schema::FederationCapabilities,
    required_methods: &[&'static str],
) -> io::Result<()> {
    for method in required_methods {
        if !capabilities.supports_method(method) {
            return Err(federation_method_not_advertised_error(host, method));
        }
    }
    Ok(())
}

fn required_federation_methods_for_request(
    request: &crate::api::schema::Request,
) -> Vec<&'static str> {
    let mut methods = vec![crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE];
    if let Some(method) = federation_method_for_api_method(&request.method) {
        methods.push(method);
    }
    methods
}

fn federation_method_for_api_method(method: &crate::api::schema::Method) -> Option<&'static str> {
    match method {
        crate::api::schema::Method::WorkspaceCreate(_) => {
            Some(crate::api::schema::FederationCapabilities::WORKSPACE_CREATE)
        }
        crate::api::schema::Method::WorkspaceListLocal(_) => {
            Some(crate::api::schema::FederationCapabilities::WORKSPACE_LIST_LOCAL)
        }
        crate::api::schema::Method::WorkspaceRename(_) => {
            Some(crate::api::schema::FederationCapabilities::WORKSPACE_RENAME)
        }
        crate::api::schema::Method::AgentList(_) => {
            Some(crate::api::schema::FederationCapabilities::AGENT_LIST)
        }
        crate::api::schema::Method::AgentListLocal(_) => {
            Some(crate::api::schema::FederationCapabilities::AGENT_LIST_LOCAL)
        }
        crate::api::schema::Method::AgentGet(_) => {
            Some(crate::api::schema::FederationCapabilities::AGENT_GET)
        }
        crate::api::schema::Method::AgentRead(_) => {
            Some(crate::api::schema::FederationCapabilities::AGENT_READ)
        }
        crate::api::schema::Method::AgentSend(_) => {
            Some(crate::api::schema::FederationCapabilities::AGENT_SEND)
        }
        crate::api::schema::Method::AgentSubmit(_) => {
            Some(crate::api::schema::FederationCapabilities::AGENT_SUBMIT)
        }
        crate::api::schema::Method::AgentFocus(_) => {
            Some(crate::api::schema::FederationCapabilities::AGENT_FOCUS)
        }
        crate::api::schema::Method::AgentStart(_) => {
            Some(crate::api::schema::FederationCapabilities::AGENT_START)
        }
        crate::api::schema::Method::AgentTeardown(_) => {
            Some(crate::api::schema::FederationCapabilities::AGENT_TEARDOWN)
        }
        crate::api::schema::Method::PaneSplit(_) => {
            Some(crate::api::schema::FederationCapabilities::PANE_SPLIT)
        }
        crate::api::schema::Method::PaneClose(_) => {
            Some(crate::api::schema::FederationCapabilities::PANE_CLOSE)
        }
        crate::api::schema::Method::PaneRename(_) => {
            Some(crate::api::schema::FederationCapabilities::PANE_RENAME)
        }
        crate::api::schema::Method::PaneFocus(_) => {
            Some(crate::api::schema::FederationCapabilities::PANE_FOCUS)
        }
        crate::api::schema::Method::PaneFocusDirection(_) => {
            Some(crate::api::schema::FederationCapabilities::PANE_FOCUS_DIRECTION)
        }
        crate::api::schema::Method::TabCreate(_) => {
            Some(crate::api::schema::FederationCapabilities::TAB_CREATE)
        }
        crate::api::schema::Method::TabList(_) => {
            Some(crate::api::schema::FederationCapabilities::TAB_LIST)
        }
        crate::api::schema::Method::TabFocus(_) => {
            Some(crate::api::schema::FederationCapabilities::TAB_FOCUS)
        }
        crate::api::schema::Method::TabClose(_) => {
            Some(crate::api::schema::FederationCapabilities::TAB_CLOSE)
        }
        crate::api::schema::Method::TabRename(_) => {
            Some(crate::api::schema::FederationCapabilities::TAB_RENAME)
        }
        crate::api::schema::Method::LayoutExport(_) => {
            Some(crate::api::schema::FederationCapabilities::LAYOUT_EXPORT)
        }
        _ => None,
    }
}

fn federation_not_advertised_error(
    host: &crate::remote_target::RemoteHostConfig,
    detail: Option<String>,
) -> io::Error {
    let mut message = format!(
        "remote host {} has a Herdr binary that does not advertise federation support; install/update Herdr on the remote host",
        host.name
    );
    if let Some(detail) = detail.filter(|detail| !detail.is_empty()) {
        message.push_str(": ");
        message.push_str(&detail);
    }
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn federation_method_not_advertised_error(
    host: &crate::remote_target::RemoteHostConfig,
    method: &str,
) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!(
        "remote host {} has a Herdr binary that does not advertise federation method {method}; install/update Herdr on the remote host",
        host.name
    ))
}

fn command_output_detail(output: &Output) -> Option<String> {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return Some(stderr);
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout)
    }
}

/// FIX-2 (post-v11 correction): when `wrote` is supplied, it is set to
/// `true` immediately BEFORE the first write call — never inferred from
/// spawn/stdin/stdout acquisition, which are still pre-write (a
/// connect/spawn/stdin failure leaves it `false`). Once the write is
/// attempted, the conservative direction from round-3 finding 6 still
/// applies: a partial write, a flush failure, or any later failure (read,
/// child wait) all count as written — never reset back to `false`. Plain
/// `Option<&mut bool>`, no atomics: this runs entirely on the calling
/// (worker) thread and never crosses a thread boundary. `None` for every
/// caller that does not need the pre-write/post-write distinction (this
/// function's other, non-routed callers are unaffected).
/// FIX-4 (diff review round 4, high 4 — round-3 finding 6 only partially
/// resolved): reaps the wrapped one-shot ssh child on drop, on EVERY exit
/// path from `send_remote_api_request_with_mode` — normal completion or any
/// early `?` return (stdin/stdout acquisition, write). Without this, those
/// early-return sites left the spawned child unreaped, contradicting the
/// routed one-shot contract that the worker owns and reaps its own child;
/// this could leave an ssh process or a zombie behind. `Deref`/`DerefMut` let
/// callers use the wrapped `Child` exactly as before (`child.stdin.take()`,
/// `child.wait()`, ...); the explicit `child.wait()` call on the normal exit
/// path already reaps the process, so `reap_child`'s own `try_wait()` on
/// drop is a cheap no-op then (idempotent — waiting on an already-reaped
/// child just returns the cached exit status). No atomics, no CAS: this is
/// plain RAII on the calling thread.
struct ReapOnDrop(Child);

impl std::ops::Deref for ReapOnDrop {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for ReapOnDrop {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for ReapOnDrop {
    fn drop(&mut self) {
        let _ = reap_child(&mut self.0);
    }
}

fn send_remote_api_request_with_mode(
    ssh: &RemoteSsh,
    bridge_command: &str,
    request: &crate::api::schema::Request,
    mode: SshInvocationMode,
    wrote: Option<&mut bool>,
) -> io::Result<String> {
    let mut command = ssh.command_with_mode(mode);
    command
        .arg(bridge_command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child =
        ReapOnDrop(command.spawn().map_err(|err| {
            io::Error::new(err.kind(), format!("failed to start ssh bridge: {err}"))
        })?);
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "ssh bridge stdin missing"))?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "ssh bridge stdout missing"))?;

    // Past this point a failure is no longer "nothing was sent": the write
    // is about to be attempted.
    if let Some(wrote) = wrote {
        *wrote = true;
    }
    write_remote_api_request(&mut child_stdin, request)?;
    drop(child_stdin);

    let mut reader = BufReader::new(child_stdout);
    let response = read_remote_api_response_line(&mut reader);
    drop(reader);

    let status = child.wait()?;
    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            format!("ssh bridge exited with {status}"),
        ));
    }

    response
}

/// One-shot prepared-state request leg for routed IO workers: like
/// [`send_remote_api_request_with_prepared_state`], running entirely on the
/// calling (worker) thread, which owns and reaps the child. `wrote` (FIX-2)
/// is set `true` immediately before the first write call; a capability
/// validation failure here never touches it (a local check, no network I/O),
/// so it correctly stays a pre-write failure.
pub(crate) fn one_shot_prepared_request(
    host: &crate::remote_target::RemoteHostConfig,
    state: &RemoteApiBridgeState,
    request: &crate::api::schema::Request,
    wrote: &mut bool,
) -> io::Result<String> {
    let required_methods = required_federation_methods_for_request(request);
    validate_federation_capabilities(host, &state.capabilities, &required_methods)?;

    let remote_ssh = remote_ssh_for_host(host);
    let bridge_command = remote_bridge_command_for_shell_path(
        &state.shell_path,
        &host.session,
        REMOTE_API_BRIDGE_SUBCOMMAND,
    );
    send_remote_api_request_with_mode(
        &remote_ssh,
        &bridge_command,
        request,
        SshInvocationMode::Noninteractive,
        Some(wrote),
    )
}

/// One-shot full-preparation request leg for routed IO workers: like
/// [`send_remote_api_request_to_host_noninteractive`] (remote binary
/// preparation + capability probes + request, one ssh child per step). Used
/// when no cached prepared state exists; the existing supervisor generation
/// path repopulates prepared state independently. `wrote` (FIX-2) is set
/// `true` immediately before the first write call for THIS request; a
/// failure during remote binary preparation (a separate ssh round trip) never
/// touches it, so it correctly stays a pre-write failure — none of this
/// request's own bytes were ever sent.
pub(crate) fn one_shot_full_request(
    host: &crate::remote_target::RemoteHostConfig,
    request: &crate::api::schema::Request,
    wrote: &mut bool,
) -> io::Result<String> {
    let remote_ssh = remote_ssh_for_host(host);
    let remote_herdr = prepare_remote_herdr_noninteractive(&remote_ssh)?;
    let bridge_command = remote_bridge_command_for_shell_path(
        remote_herdr.shell_path(),
        &host.session,
        REMOTE_API_BRIDGE_SUBCOMMAND,
    );
    send_remote_api_request_with_mode(
        &remote_ssh,
        &bridge_command,
        request,
        SshInvocationMode::Noninteractive,
        Some(wrote),
    )
}

pub(crate) fn run_remote_client_bridge() -> io::Result<()> {
    ensure_remote_server_running()?;

    let (socket_path, description) = remote_bridge_socket_target(RemoteBridgeSocketKind::Client);
    run_stdio_socket_bridge(&socket_path, description)
}

/// Fail-closed/no-start variant of [`run_remote_client_bridge`] for in-place
/// terminal-session projection streams: it never calls
/// `ensure_remote_server_running`, so merely selecting/projecting a host can
/// never start, stop, set up, install, update, or wake the remote Herdr
/// server. If the remote client socket for this session is not already
/// listening (no already-running remote Herdr server/session), connecting
/// simply fails instead of starting one.
pub(crate) fn run_remote_client_bridge_no_start() -> io::Result<()> {
    let (socket_path, description) = remote_bridge_socket_target(RemoteBridgeSocketKind::Client);
    run_stdio_socket_bridge(&socket_path, description)
}

/// Which `remote-api-bridge` mode a CLI invocation selects. Bare
/// `remote-api-bridge` (no flag, or any unrecognized arg) stays on the existing
/// one-shot stdio socket bridge; `remote-api-bridge --persistent` runs the
/// Phase G.10 one-request-per-API-socket loop reused by the local bridge pool.
/// Factored out so the routing decision is pinnable by tests without a running
/// Herdr server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteApiBridgeMode {
    OneShot,
    Persistent,
}

fn remote_api_bridge_mode(args: &[String]) -> RemoteApiBridgeMode {
    if args
        .iter()
        .any(|arg| arg == REMOTE_API_BRIDGE_PERSISTENT_FLAG)
    {
        RemoteApiBridgeMode::Persistent
    } else {
        RemoteApiBridgeMode::OneShot
    }
}

pub(crate) fn run_remote_api_bridge(args: &[String]) -> io::Result<()> {
    match remote_api_bridge_mode(args) {
        RemoteApiBridgeMode::Persistent => run_remote_api_bridge_persistent(),
        RemoteApiBridgeMode::OneShot => {
            ensure_remote_server_running()?;

            let (socket_path, description) =
                remote_bridge_socket_target(RemoteBridgeSocketKind::Api);
            run_stdio_socket_bridge(&socket_path, description)
        }
    }
}

/// Persistent remote-API bridge loop (Phase G.10). Runs on the authoritative
/// remote host. For each newline-terminated JSON request line read from stdin it
/// opens one fresh Herdr API Unix-socket connection, forwards exactly that one
/// request line, reads exactly one response line, writes that response line to
/// stdout, flushes, closes that API socket, and loops for the next request.
///
/// Contract:
/// - Exits cleanly on stdin EOF.
/// - One active request at a time; no multiplexing and no request queue.
/// - Never streams or holds a subscription: only the single-response routed
///   methods are gated into this path locally.
/// - A per-request API-socket connect/IO failure (the request never reached the
///   API socket) emits one structured `remote_request_failed` response line for
///   that request and keeps the loop alive, rather than killing the bridge.
fn run_remote_api_bridge_persistent() -> io::Result<()> {
    ensure_remote_server_running()?;

    let (socket_path, description) = remote_bridge_socket_target(RemoteBridgeSocketKind::Api);
    let stdin = io::stdin();
    let mut stdin = stdin.lock();
    let stdout = io::stdout();
    let stdout = io::BufWriter::new(stdout.lock());
    run_persistent_api_bridge_loop_io(&mut stdin, stdout, &socket_path, description)
}

/// Core of the persistent remote-API bridge loop, factored out so tests can
/// drive it against a local `UnixListener` (a fake remote API socket) with piped
/// stdin/stdout, without spawning a real Herdr server. The production entry
/// point ([`run_remote_api_bridge_persistent`]) locks the process stdin/stdout
/// and calls this. See the loop contract on [`run_remote_api_bridge_persistent`].
fn run_persistent_api_bridge_loop_io<R: BufRead, W: io::Write>(
    stdin: &mut R,
    mut stdout: W,
    socket_path: &Path,
    description: &str,
) -> io::Result<()> {
    let mut request_line = String::new();
    loop {
        request_line.clear();
        let bytes_read = stdin.read_line(&mut request_line)?;
        if bytes_read == 0 {
            // Clean stdin EOF: exit the loop. No partial request is in flight
            // because a request only begins once a full line has been read.
            return Ok(());
        }
        let request = request_line.trim_matches(['\r', '\n']);
        if request.is_empty() {
            // Skip blank lines without consuming an API socket connection.
            continue;
        }

        // Open one fresh API socket connection per request. The remote API
        // remains one-request-per-socket; this bridge only forwards.
        let mut stream = match UnixStream::connect(socket_path) {
            Ok(stream) => stream,
            Err(err) => {
                // The request never reached the API socket: emit a structured
                // error response line for this request and keep looping. This
                // is an idempotency-safe failure (no delivery happened), so the
                // local pool never needs to retry it.
                write_persistent_bridge_error_response(
                    &mut stdout,
                    request,
                    format!(
                        "failed to connect to {description} {}: {err}",
                        socket_path.display()
                    ),
                )?;
                continue;
            }
        };

        // Forward exactly this one request line.
        if let Err(err) = stream
            .write_all(request.as_bytes())
            .and_then(|()| stream.write_all(b"\n"))
            .and_then(|()| stream.flush())
        {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            write_persistent_bridge_error_response(
                &mut stdout,
                request,
                format!("failed to forward request to {description}: {err}"),
            )?;
            continue;
        }

        // Read exactly one response line.
        let mut reader = io::BufReader::new(&stream);
        let mut response_line = String::new();
        let read_result = reader.read_line(&mut response_line).and_then(|n| {
            if n == 0 || response_line.trim().is_empty() {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "remote API returned an empty response",
                ))
            } else {
                Ok(())
            }
        });
        // Close this per-request socket before looping regardless of outcome.
        let _ = stream.shutdown(std::net::Shutdown::Both);
        drop(reader);

        match read_result {
            Ok(()) => {
                stdout.write_all(response_line.trim_end_matches(['\r', '\n']).as_bytes())?;
                stdout.write_all(b"\n")?;
                stdout.flush()?;
            }
            Err(err) => {
                write_persistent_bridge_error_response(
                    &mut stdout,
                    request,
                    format!("failed to read response from {description}: {err}"),
                )?;
            }
        }
    }
}

/// Build a structured `remote_request_failed` JSON response line for one
/// persistent-bridge request whose per-request API-socket IO failed before the
/// request reached the API. The `id` is echoed back leniently from the request
/// line when it parses as JSON with an `id` field, so the local dispatcher can
/// still correlate the failure; otherwise the response carries a null id. Only
/// the controlled `remote_request_failed` code and the id are emitted; the
/// message is the bridge-local IO detail.
fn write_persistent_bridge_error_response<W: io::Write>(
    stdout: &mut W,
    request_line: &str,
    message: String,
) -> io::Result<()> {
    let id = serde_json::from_str::<serde_json::Value>(request_line.trim())
        .ok()
        .and_then(|value| value.get("id").cloned())
        .unwrap_or(serde_json::Value::Null);
    let payload = serde_json::json!({
        "id": id,
        "error": {
            "code": "remote_request_failed",
            "message": message,
        }
    });
    // `to_string` cannot fail for this shape.
    let encoded = serde_json::to_string(&payload).map_err(io::Error::other)?;
    stdout.write_all(encoded.as_bytes())?;
    stdout.write_all(b"\n")?;
    stdout.flush()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteBridgeSocketKind {
    Client,
    Api,
}

fn remote_bridge_socket_target(kind: RemoteBridgeSocketKind) -> (PathBuf, &'static str) {
    match kind {
        RemoteBridgeSocketKind::Client => (
            crate::server::socket_paths::client_socket_path(),
            "remote Herdr client socket",
        ),
        RemoteBridgeSocketKind::Api => (crate::api::socket_path(), "remote Herdr API socket"),
    }
}

fn run_stdio_socket_bridge(socket_path: &Path, description: &str) -> io::Result<()> {
    let stream = UnixStream::connect(socket_path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to connect to {description} {}: {err}",
                socket_path.display()
            ),
        )
    })?;

    bridge_stdio_to_unix_stream(stream)
}

fn bridge_stdio_to_unix_stream(stream: UnixStream) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    let mut socket_to_stdout = stream.try_clone()?;
    let mut stdin_to_socket = stream;

    let _upload = thread::spawn(move || {
        let mut stdin = io::stdin();
        let _ = copy_flush(&mut stdin, &mut stdin_to_socket);
        let _ = stdin_to_socket.shutdown(std::net::Shutdown::Write);
    });

    copy_flush(&mut socket_to_stdout, &mut stdout).map(|_| ())
}

fn ensure_remote_server_running() -> io::Result<()> {
    let socket_path = crate::server::socket_paths::client_socket_path();
    if crate::server::autodetect::is_server_listening() {
        let status = crate::api::read_runtime_status_at(
            &crate::api::socket_path(),
            Duration::from_millis(500),
        )?
        .ok_or_else(|| io::Error::other("remote server status API is unavailable"))?;
        if status.protocol == Some(CURRENT_PROTOCOL) {
            return Ok(());
        }
        return Err(io::Error::other(
            "remote herdr server must restart before this bridge can attach; rerun `herdr --remote` from an interactive terminal to approve stopping it",
        ));
    }

    crate::server::autodetect::spawn_server_daemon()?;
    crate::server::autodetect::wait_for_server_socket(&socket_path, Duration::from_secs(5))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemotePlatform {
    os: &'static str,
    arch: &'static str,
}

impl RemotePlatform {
    fn from_uname(os: &str, arch: &str) -> Option<Self> {
        let os = match os.trim() {
            "Linux" => "linux",
            "Darwin" => "macos",
            _ => return None,
        };
        let arch = match arch.trim() {
            "x86_64" | "amd64" => "x86_64",
            "aarch64" | "arm64" => "aarch64",
            _ => return None,
        };
        Some(Self { os, arch })
    }

    fn local() -> Self {
        let os = if cfg!(target_os = "linux") {
            "linux"
        } else if cfg!(target_os = "macos") {
            "macos"
        } else {
            "unknown"
        };

        let arch = if cfg!(target_arch = "x86_64") {
            "x86_64"
        } else if cfg!(target_arch = "aarch64") {
            "aarch64"
        } else {
            "unknown"
        };

        Self { os, arch }
    }

    fn asset_key(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteHerdr {
    install_suffix: String,
    shell_path: String,
    platform: RemotePlatform,
}

/// Reusable, cloneable prepared bridge state for a remote host: the prepared
/// remote Herdr shell path plus the full advertised [`FederationCapabilities`]
/// captured from a successful supervisor compatibility/ping round-trip.
///
/// This is rebuildable soft data (no live handles, sockets, or threads): a
/// connected remote-source supervisor publishes it through an `AppEvent`, and
/// routed agent dispatch may reuse it to skip per-request remote binary
/// preparation and capability/ping probes while the host stays `Connected`. It
/// is invalidated when the host becomes non-connected. Storing the shell path
/// string keeps `AppState`/`RemoteSourceCache` free of platform-specific
/// `RemoteHerdr`.
///
/// [`FederationCapabilities`]: crate::api::schema::FederationCapabilities
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteApiBridgeState {
    pub(crate) shell_path: String,
    pub(crate) capabilities: crate::api::schema::FederationCapabilities,
}

/// One live persistent remote-API bridge connection: a remote
/// `ssh ... remote-api-bridge --persistent` child speaking the one-request
/// stdio loop. The local bridge pool owns these and moves one out of the pool
/// during an active request, so a checked-out connection is never shared
/// between two requesters (one active request per bridge, no multiplexing).
///
/// This is a transport object only: `write_request`/`read_response` forward
/// exactly one JSON line each way. Production is [`SshPersistentBridge`]; tests
/// inject a fake via [`dispatch_via_remote_bridge_pool`]'s starter parameter.
pub(crate) trait PersistentRemoteBridgeConnection: Send {
    /// Write exactly one newline-terminated JSON request line to the remote
    /// persistent loop. A failure here (including a partial write or broken
    /// pipe on a silently-dead idle bridge) means the request may or may not
    /// have been delivered; callers must treat it as delivered-and-failed and
    /// must not retry or fall back.
    fn write_request(&mut self, request: &crate::api::schema::Request) -> io::Result<()>;
    /// Read exactly one JSON response line from the remote persistent loop.
    fn read_response(&mut self) -> io::Result<String>;
    /// Cheap pre-write liveness probe. Returns `false` only when the underlying
    /// transport has definitively exited (e.g. the ssh child has died). A
    /// `true` result is not a guarantee the next write/read succeeds; a
    /// half-dead connection still fails on write/read and is discarded then.
    fn is_alive(&mut self) -> bool;
}

/// Signature of a starter that opens one persistent remote-API bridge
/// connection. Production uses [`start_persistent_remote_api_bridge`]; pool
/// tests inject a fake so the checkout/start/return/prune/invalidation logic
/// is exercised without real SSH.
pub(crate) type PersistentRemoteBridgeStarter =
    fn(
        &crate::remote_target::RemoteHostConfig,
        &RemoteApiBridgeState,
    ) -> io::Result<Box<dyn PersistentRemoteBridgeConnection>>;

/// Real persistent remote-API bridge connection: one non-interactive `ssh`
/// child running `remote-api-bridge --persistent`, plus the [`RemoteSsh`] whose
/// `Drop` tears the SSH control master down. One active request at a time; the
/// pool never hands the same connection to two requesters.
pub(crate) struct SshPersistentBridge {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    /// Held so its `Drop` runs `ssh -O exit` control-master cleanup when this
    /// connection is torn down. Kept alive for exactly the bridge's lifetime.
    _ssh: RemoteSsh,
}

impl SshPersistentBridge {
    /// Take a stdin handle for writing request lines. Returns `None` if the
    /// child's stdin was already taken (should not happen for a pooled entry).
    fn stdin(&mut self) -> io::Result<&mut ChildStdin> {
        self.stdin.as_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "persistent bridge stdin closed")
        })
    }
}

impl PersistentRemoteBridgeConnection for SshPersistentBridge {
    fn write_request(&mut self, request: &crate::api::schema::Request) -> io::Result<()> {
        write_remote_api_request(self.stdin()?, request)
    }

    fn read_response(&mut self) -> io::Result<String> {
        read_remote_api_response_line(&mut self.stdout)
    }

    fn is_alive(&mut self) -> bool {
        // `try_wait` returning `Ok(None)` means the child is still running.
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for SshPersistentBridge {
    fn drop(&mut self) {
        // Teardown ordering (Phase G.10): close the child's stdin first so the
        // remote persistent loop observes EOF and exits cleanly, then reap the
        // child, then drop the `RemoteSsh` so control-master cleanup (`ssh -O
        // exit`) still happens. Closing stdin before drop matters: without it
        // the remote loop would block on the next read and the ssh child would
        // not exit promptly.
        drop(self.stdin.take());
        // Reap with a short grace window; kill only if it refuses to exit so a
        // wedged remote loop cannot leak an ssh process indefinitely.
        if let Err(err) = reap_child(&mut self.child) {
            tracing::warn!(
                event = "remote.route.bridge_teardown_reap_failed",
                subsystem = "remote",
                pid = self.child.id(),
                err = %err,
                "persistent bridge teardown reap failed"
            );
        }
    }
}

/// Reap a child process: wait briefly for it to exit on its own, then escalate
/// to `kill` + wait so teardown never leaks a process. Runs on `Drop` paths
/// where there is no caller to propagate a `Result` to, so kill/wait failures
/// are not returned as an error — but they ARE surfaced via `tracing::warn!`
/// rather than silently swallowed (H1: "surfaced reap/join failures").
fn reap_child(child: &mut Child) -> io::Result<()> {
    let deadline = Instant::now() + PERSISTENT_BRIDGE_REAP_GRACE;
    loop {
        match child.try_wait()? {
            Some(_status) => return Ok(()),
            None if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            None => {
                if let Err(err) = child.kill() {
                    tracing::warn!(
                        event = "remote.route.bridge_teardown_kill_failed",
                        subsystem = "remote",
                        pid = child.id(),
                        err = %err,
                        "persistent bridge teardown kill failed (child may already be dead)"
                    );
                }
                if let Err(err) = child.wait() {
                    tracing::warn!(
                        event = "remote.route.bridge_teardown_wait_failed",
                        subsystem = "remote",
                        pid = child.id(),
                        err = %err,
                        "persistent bridge teardown wait failed after kill"
                    );
                }
                return Ok(());
            }
        }
    }
}

/// Start one real persistent remote-API bridge connection to `host`, reusing
/// cached supervisor-prepared bridge state (shell path + capabilities) so the
/// bridge command is built without redoing remote binary preparation. Returns
/// a connection backed by a non-interactive `ssh` child running
/// `remote-api-bridge --persistent`. A fresh child is started per call; pooling
/// is the caller's responsibility.
pub(crate) fn start_persistent_remote_api_bridge(
    host: &crate::remote_target::RemoteHostConfig,
    state: &RemoteApiBridgeState,
) -> io::Result<Box<dyn PersistentRemoteBridgeConnection>> {
    let remote_ssh = remote_ssh_for_host(host);
    let bridge_command =
        remote_persistent_api_bridge_command_for_shell_path(&state.shell_path, &host.session);
    let mut command = remote_ssh.command_with_mode(SshInvocationMode::Noninteractive);
    command
        .arg(&bridge_command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child = command.spawn().map_err(|err| {
        io::Error::new(
            err.kind(),
            format!("failed to start persistent ssh bridge: {err}"),
        )
    })?;
    let stdin = child.stdin.take().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "persistent ssh bridge stdin missing",
        )
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "persistent ssh bridge stdout missing",
        )
    })?;

    Ok(Box::new(SshPersistentBridge {
        child,
        stdin: Some(stdin),
        stdout: BufReader::new(stdout),
        _ssh: remote_ssh,
    }))
}

/// Idle TTL for pooled persistent bridges. A pooled entry older than this since
/// its last use is pruned at checkout/return. Named constant per the plan: a
/// bridge idle longer than this is closed and reaped rather than reused.
const PERSISTENT_BRIDGE_IDLE_TTL: Duration = Duration::from_secs(30);
/// Grace window before a persistent bridge child is force-killed on teardown.
/// The remote loop exits on stdin EOF within milliseconds under normal
/// conditions; this only bounds the worst case.
const PERSISTENT_BRIDGE_REAP_GRACE: Duration = Duration::from_secs(2);

/// Identity captured for a pooled persistent bridge so stale children are never
/// reused across config reload, SSH target change, prepared-state change, or
/// capability change. The cached prepared state is already `Eq`, so comparing it
/// is the primary lazy invalidation path (no separate generation counter); the
/// SSH target/session/config fields additionally catch a config reload that
/// keeps the same alias/session but changes the underlying SSH target.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PersistentBridgeIdentity {
    shell_path: String,
    capabilities: crate::api::schema::FederationCapabilities,
    ssh_target: String,
    session: String,
    manage_ssh_config: bool,
    connect_timeout_secs: u32,
}

impl PersistentBridgeIdentity {
    fn from_host_state(
        host: &crate::remote_target::RemoteHostConfig,
        state: &RemoteApiBridgeState,
    ) -> Self {
        Self {
            shell_path: state.shell_path.clone(),
            capabilities: state.capabilities.clone(),
            ssh_target: host.target.clone(),
            session: host.session.clone(),
            manage_ssh_config: crate::config::Config::load()
                .config
                .remote
                .manage_ssh_config,
            connect_timeout_secs: host.connect_timeout_secs,
        }
    }
}

/// One pooled idle persistent bridge entry.
struct PooledBridgeEntry {
    identity: PersistentBridgeIdentity,
    connection: Box<dyn PersistentRemoteBridgeConnection>,
    last_used: Instant,
    /// `false` once the entry is retired (idle-pruned, invalidated by a
    /// non-connected transition, or superseded). Retired entries are reaped at
    /// the next checkout/return/shutdown rather than in a hot loop.
    reusable: bool,
}

/// Per-`RemoteHostKey` pool state. `active` counts connections currently
/// checked out (held by worker threads, not stored here); `idle` holds parked
/// reusable connections. The invariant `active + idle.len() <= max_per_key` is
/// enforced at start-new time (see [`RemoteAgentBridgePool::reserve_new`]).
///
/// `generation` is the per-host invalidation epoch (Phase G.10): a checked-out
/// or reserved connection captures the generation at checkout/reserve time, and
/// `return_connection` parks it only when that captured generation still equals
/// the current one. `invalidate_host` and `drain` advance the generation, so an
/// active bridge returning after a disconnect/shutdown is reaped instead of
/// parked even though the worker still passes `reusable: true`. Idle entries are
/// also marked non-reusable at invalidate time (they have no captured
/// generation, so the `reusable` flag catches entries parked before the
/// invalidation).
#[derive(Default)]
struct HostBridgePool {
    idle: Vec<PooledBridgeEntry>,
    active: usize,
    generation: u64,
}

impl HostBridgePool {
    fn total(&self) -> usize {
        self.idle.len() + self.active
    }
}

/// Local per-`RemoteHostKey` bounded idle persistent-bridge pool (Phase G.10).
/// Used only when a dispatch descriptor carries G.9 [`RemoteApiBridgeState`] and
/// the cached capabilities advertise [`FederationCapabilities::REMOTE_API_BRIDGE_PERSISTENT`].
/// The per-(host alias, session) in-flight limiter (`REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT`)
/// is acquired *before* this pool is consulted, so saturating that limiter
/// returns `remote_bridge_busy` without any pool checkout/start.
///
/// Invariants:
/// - `active + idle.len() <= max_per_key` (enforced at start-new; reuse/return
///   never grow the total).
/// - A checked-out connection is owned by one worker; the pool never hands the
///   same connection to two requesters (one active request per bridge).
/// - Idle entries hold no limiter permit; only active dispatch holds the RAII
///   permit (acquired by the starter before this pool runs).
///
/// [`FederationCapabilities`]: crate::api::schema::FederationCapabilities
/// [`FederationCapabilities::REMOTE_API_BRIDGE_PERSISTENT`]: crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE_PERSISTENT
pub(crate) struct RemoteAgentBridgePool {
    inner: Mutex<RemoteAgentBridgePoolInner>,
    max_per_key: usize,
    idle_ttl: Duration,
}

struct RemoteAgentBridgePoolInner {
    hosts: BTreeMap<crate::remote_source::RemoteHostKey, HostBridgePool>,
}

impl RemoteAgentBridgePool {
    /// Construct a pool with an explicit per-key cap and idle TTL. Production
    /// uses [`remote_agent_bridge_pool`]; tests construct local instances so
    /// they never touch or saturate the process-global pool.
    pub(crate) fn new(max_per_key: usize, idle_ttl: Duration) -> Self {
        Self {
            inner: Mutex::new(RemoteAgentBridgePoolInner {
                hosts: BTreeMap::new(),
            }),
            max_per_key,
            idle_ttl,
        }
    }

    /// Per-key cap. Test only: the equality test pins it against the app-layer
    /// limiter `REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT`.
    #[cfg(test)]
    pub(crate) fn max_per_key(&self) -> usize {
        self.max_per_key
    }
}

/// Maximum number of live persistent bridges (active + idle) the local pool
/// will keep per configured (host alias, session). Mirrors the per-(host,
/// session) in-flight dispatch limiter
/// `REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT` (app layer): the limiter is acquired
/// before this pool is consulted, and the pool cap stays no greater than it so
/// `active + idle` never exceeds the limiter. Kept local to the remote layer so
/// it does not depend on the app layer; `pub(crate)` so the app-layer test can
/// pin the two consts equal.
pub(crate) const REMOTE_AGENT_BRIDGE_POOL_MAX_PER_HOST: usize = 4;

impl RemoteAgentBridgePool {
    fn lock(&self) -> std::sync::MutexGuard<'_, RemoteAgentBridgePoolInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Idle connection count for one (host, session). Test only.
    #[cfg(test)]
    pub(crate) fn idle_for(&self, key: &crate::remote_source::RemoteHostKey) -> usize {
        self.lock()
            .hosts
            .get(key)
            .map(|h| h.idle.len())
            .unwrap_or(0)
    }

    /// Seed an active reservation directly, bypassing real checkout/start, so
    /// tests can exercise a release-side fix (e.g. `release_persistent_bridge_active`)
    /// against the real pool counter without real SSH. Test only.
    #[cfg(test)]
    pub(crate) fn seed_active_for_test(&self, key: &crate::remote_source::RemoteHostKey) {
        self.lock().hosts.entry(key.clone()).or_default().active += 1;
    }

    /// Active (checked-out) connection count for one (host, session). Test only.
    #[cfg(test)]
    pub(crate) fn active_for(&self, key: &crate::remote_source::RemoteHostKey) -> usize {
        self.lock().hosts.get(key).map(|h| h.active).unwrap_or(0)
    }

    /// Try to check out a reusable idle connection matching `identity`. On
    /// success the connection is moved out of the pool, `active` is incremented
    /// (the worker now owns it exclusively — one active request per bridge), and
    /// the host generation captured at checkout is returned alongside it so the
    /// later [`Self::return_connection`] call can refuse to park the connection
    /// if the host was invalidated while it was active. Expired, retired, dead,
    /// or identity-mismatched idle entries are pruned during the scan and reaped
    /// outside the pool lock.
    fn checkout_reusable(
        &self,
        key: &crate::remote_source::RemoteHostKey,
        identity: &PersistentBridgeIdentity,
    ) -> Option<(Box<dyn PersistentRemoteBridgeConnection>, u64)> {
        let mut pruned = Vec::new();
        let checked_out = {
            let mut inner = self.lock();
            let now = Instant::now();
            let host_pool = inner.hosts.entry(key.clone()).or_default();
            let mut keep = Vec::with_capacity(host_pool.idle.len());
            let mut checked_out: Option<Box<dyn PersistentRemoteBridgeConnection>> = None;
            for mut entry in host_pool.idle.drain(..) {
                // Cheap pre-write liveness probe: a dead ssh child is pruned
                // rather than handed to a requester. Identity mismatch and TTL
                // expiry are also pruned here (lazy invalidation).
                let alive = entry.connection.is_alive();
                let fresh = now.duration_since(entry.last_used) < self.idle_ttl;
                if checked_out.is_none()
                    && entry.reusable
                    && alive
                    && fresh
                    && entry.identity == *identity
                {
                    checked_out = Some(entry.connection);
                } else if entry.reusable && alive && fresh && entry.identity == *identity {
                    // Keep additional matching fresh idle entries after one has
                    // been checked out; mismatched-but-fresh entries are pruned
                    // here so they cannot temporarily fill the pool with stale
                    // identities (matching the doc comment above).
                    keep.push(entry);
                } else {
                    pruned.push(entry.connection);
                }
            }
            host_pool.idle = keep;
            let generation = host_pool.generation;
            if checked_out.is_some() {
                host_pool.active += 1;
            }
            checked_out.map(|connection| (connection, generation))
        };
        drop(pruned);
        checked_out
    }

    /// Reserve an active slot for starting a new connection. Returns
    /// `Some(generation)` if `total < max_per_key` after pruning (incrementing
    /// `active`), or `None` if the pool is full — in which case the caller falls
    /// back to the one-shot prepared path (pre-write, safe). The returned
    /// generation is captured for the active connection so the later
    /// [`Self::return_connection`] call can refuse to park it if the host was
    /// invalidated between reserve and return. Reaping happens outside the lock.
    fn reserve_new(&self, key: &crate::remote_source::RemoteHostKey) -> Option<u64> {
        let mut pruned = Vec::new();
        let reserved = {
            let mut inner = self.lock();
            let now = Instant::now();
            let host_pool = inner.hosts.entry(key.clone()).or_default();
            prune_host_idle_collect(host_pool, self.idle_ttl, now, &mut pruned);
            if host_pool.total() < self.max_per_key {
                host_pool.active += 1;
                Some(host_pool.generation)
            } else {
                None
            }
        };
        drop(pruned);
        reserved
    }

    /// Release an active slot reserved by [`Self::reserve_new`] when the
    /// connection start failed before any byte was written. Pre-write, so the
    /// caller is still allowed to fall back to the one-shot prepared path. Also
    /// used to release the slot of a connection discarded after a post-write
    /// failure (the connection itself is reaped by the caller).
    fn release_active(&self, key: &crate::remote_source::RemoteHostKey) {
        let mut inner = self.lock();
        if let Some(host_pool) = inner.hosts.get_mut(key) {
            if host_pool.active > 0 {
                host_pool.active -= 1;
            }
        }
    }

    /// Return a connection that was checked out or started: decrement `active`,
    /// prune expired idle entries, then park the connection for reuse when there
    /// is room, it is `reusable`, AND its `checked_out_generation` still equals
    /// the host's current generation; otherwise reap it. The generation check is
    /// the active-connection invalidation boundary: a bridge checked out during
    /// a disconnect/non-connected transition (or before a shutdown drain) has a
    /// stale captured generation by the time it returns, so it is reaped outside
    /// the pool lock rather than parked with an old identity. `active` is always
    /// released exactly once here regardless of whether the connection is
    /// parked. Pruning and the returned connection's teardown both happen
    /// outside the pool lock so a slow child reap cannot block other workers.
    fn return_connection(
        &self,
        key: &crate::remote_source::RemoteHostKey,
        identity: PersistentBridgeIdentity,
        connection: Box<dyn PersistentRemoteBridgeConnection>,
        reusable: bool,
        checked_out_generation: u64,
    ) {
        let mut pruned = Vec::new();
        let not_parked = {
            let mut inner = self.lock();
            let now = Instant::now();
            let host_pool = inner.hosts.entry(key.clone()).or_default();
            if host_pool.active > 0 {
                host_pool.active -= 1;
            }
            prune_host_idle_collect(host_pool, self.idle_ttl, now, &mut pruned);
            // Park only when reusable, the generation is still current (the host
            // was not invalidated/drained while this connection was active), and
            // there is room. A stale generation reaps the connection below.
            let park = reusable
                && host_pool.generation == checked_out_generation
                && host_pool.idle.len() < self.max_per_key;
            if park {
                host_pool.idle.push(PooledBridgeEntry {
                    identity,
                    connection,
                    last_used: now,
                    reusable: true,
                });
                None
            } else {
                Some(connection)
            }
        };
        drop(not_parked);
        drop(pruned);
    }

    /// Invalidate every pooled bridge for `key` on a non-connected transition
    /// (disconnect, config reload, prepared-state loss). Advances the per-host
    /// invalidation generation so an ACTIVE bridge checked out during the
    /// transition is not parked when it later returns (its captured generation
    /// will be stale), and marks idle entries non-reusable so entries parked
    /// before the transition are pruned at the next checkout. Mark-only and
    /// lock-bounded: it never reaps a child, so the App/headless reducer loop
    /// that calls it on a non-connected transition never stalls on process
    /// cleanup. Idle entries and active bridges returning with a stale
    /// generation are reaped lazily on the next checkout/return.
    pub(crate) fn invalidate_host(&self, key: &crate::remote_source::RemoteHostKey) {
        let mut inner = self.lock();
        if let Some(host_pool) = inner.hosts.get_mut(key) {
            host_pool.generation = host_pool.generation.wrapping_add(1);
            for entry in host_pool.idle.iter_mut() {
                entry.reusable = false;
            }
        }
    }

    /// Strong per-host drain for an explicit runtime lifecycle action
    /// (disconnect/reconnect). Advances this host's pool generation and moves
    /// every idle bridge for `key` into owned cleanup work under the pool lock,
    /// then returns the owned connections so the caller reaps them (child
    /// stdin close + reap) outside the lock -- never on the App/headless/
    /// render/input loop. Active checked-out bridges are left to finish, but
    /// their captured generation is now stale, so [`Self::return_connection`]
    /// reaps instead of parking them. This is distinct from the mark-only
    /// [`Self::invalidate_host`] (lazy invalidation on a non-connected
    /// transition): it actually removes ownership of idle bridges now and
    /// hands them to the caller to drop. Other hosts are untouched. Safe to
    /// call on a host with no pooled bridges (returns empty).
    pub(crate) fn drain_host(
        &self,
        key: &crate::remote_source::RemoteHostKey,
    ) -> Vec<Box<dyn PersistentRemoteBridgeConnection>> {
        let mut inner = self.lock();
        let Some(host_pool) = inner.hosts.get_mut(key) else {
            return Vec::new();
        };
        host_pool.generation = host_pool.generation.wrapping_add(1);
        host_pool
            .idle
            .drain(..)
            .map(|entry| entry.connection)
            .collect()
    }

    /// Drain every idle bridge for every host and advance every host generation
    /// so an active bridge returning after this drain is not parked. Idle
    /// connections are collected under the lock and reaped (child stdin close +
    /// reap) outside it so a slow child reap cannot block another worker or the
    /// shutdown path. Intended for process shutdown only; safe to call on a
    /// never-used or empty pool, and idempotent. `active` counts are left as-is
    /// (in-flight dispatches release their own active slot on return, where the
    /// advanced generation prevents parking).
    pub(crate) fn drain(&self) {
        let mut drained = Vec::new();
        {
            let mut inner = self.lock();
            for host_pool in inner.hosts.values_mut() {
                host_pool.generation = host_pool.generation.wrapping_add(1);
                drained.extend(host_pool.idle.drain(..).map(|entry| entry.connection));
            }
        }
        // Reap outside the lock: each drop closes stdin + reaps the ssh child
        // (ControlPersist cleanup via `RemoteSsh::Drop`).
        drop(drained);
    }
}

/// Move expired/retired idle entries out of a host pool into `pruned` (to be
/// reaped by the caller outside the pool lock). Keeps only entries that are
/// still `reusable` and within the idle TTL. It does not probe liveness (that
/// is done per-entry at checkout); a dead-but-not-expired entry is kept here
/// and pruned lazily on its next checkout attempt.
fn prune_host_idle_collect(
    host_pool: &mut HostBridgePool,
    ttl: Duration,
    now: Instant,
    pruned: &mut Vec<Box<dyn PersistentRemoteBridgeConnection>>,
) {
    let mut keep = Vec::with_capacity(host_pool.idle.len());
    for entry in host_pool.idle.drain(..) {
        if entry.reusable && now.duration_since(entry.last_used) < ttl {
            keep.push(entry);
        } else {
            pruned.push(entry.connection);
        }
    }
    host_pool.idle = keep;
}

/// Process-global persistent-bridge pool bound to
/// [`REMOTE_AGENT_BRIDGE_POOL_MAX_PER_HOST`] and [`PERSISTENT_BRIDGE_IDLE_TTL`].
/// Consulted only inside [`try_pooled_remote_api_request`] when a dispatch
/// descriptor carries G.9 prepared state and the cached capabilities advertise
/// the persistent-bridge capability. Dormant otherwise.
static REMOTE_AGENT_BRIDGE_POOL: OnceLock<RemoteAgentBridgePool> = OnceLock::new();

pub(crate) fn remote_agent_bridge_pool() -> &'static RemoteAgentBridgePool {
    REMOTE_AGENT_BRIDGE_POOL.get_or_init(|| {
        RemoteAgentBridgePool::new(
            REMOTE_AGENT_BRIDGE_POOL_MAX_PER_HOST,
            PERSISTENT_BRIDGE_IDLE_TTL,
        )
    })
}

/// Mark every idle persistent bridge for `key` non-reusable. Called from the
/// remote-source reducer on a non-connected transition so idle bridges are not
/// reused after a disconnect; they are reaped lazily at the next checkout.
/// Also advances the per-host generation so an active bridge returning after
/// the transition is not parked.
pub(crate) fn invalidate_remote_bridge_pool_host(key: &crate::remote_source::RemoteHostKey) {
    remote_agent_bridge_pool().invalidate_host(key);
}

/// Synchronous per-host drain+reap for a lifecycle supervisor worker
/// (connect/reconnect) to run before its initial ping. Collects this host's
/// idle bridges under the pool lock and drops them here (off-loop, in the
/// worker thread) so the fresh generation starts with no stale pooled
/// bridges. Distinct from the mark-only [`invalidate_remote_bridge_pool_host`].
/// Other hosts are untouched. Safe on a dormant pool / host with no idle
/// bridges. Re-exported cross-platform by [`crate::remote`].
pub(crate) fn drain_remote_bridge_pool_host_inline(key: &crate::remote_source::RemoteHostKey) {
    let drained = remote_agent_bridge_pool().drain_host(key);
    drop(drained);
}

/// Off-loop per-host drain+reap with deferred completion for an explicit
/// disconnect. Collects this host's idle bridges under the pool lock, spawns a
/// bounded cleanup worker that drops/reaps them outside the lock and off-loop,
/// then reports completion back to the App through
/// [`AppEvent::RemoteSourcePoolDrainCompleted`] tagged with `generation`.
/// Active checked-out bridges are left to finish, but their captured
/// generation is now stale so they cannot re-pool. Other hosts are untouched.
/// Safe on a dormant pool / host with no idle bridges (still reports
/// completion so the disconnect pending responder always resolves).
/// Re-exported cross-platform by [`crate::remote`].
pub(crate) fn drain_remote_bridge_pool_host_off_loop(
    key: crate::remote_source::RemoteHostKey,
    generation: u64,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
) {
    let drained = remote_agent_bridge_pool().drain_host(&key);
    std::thread::spawn(move || {
        // Reap outside the pool lock + off-loop: each drop closes stdin +
        // reaps the ssh child (ControlPersist cleanup via `RemoteSsh::Drop`).
        // The completion guard guarantees the disconnect pending responder is
        // resolved exactly once even if a drop/reap panics: `complete()` sends
        // the normal completion and disarms the fallback; `Drop` sends a
        // fallback completion only if `complete()` never ran. The App treats a
        // completion for a superseded/already-resolved generation as a no-op,
        // so a late fallback can never double-resolve.
        let mut guard = PoolDrainCompletionGuard::new(key, generation, event_tx);
        drop(drained);
        guard.complete();
    });
}

/// Completion guard for [`drain_remote_bridge_pool_host_off_loop`]. Guarantees
/// the disconnect pending responder resolves exactly once even if dropping
/// (reaping) the drained connections panics: [`Self::complete`] sends the
/// normal completion and disarms; `Drop` sends a fallback completion only if
/// `complete` never ran. Mirrors [`LifecycleCompletionGuard`]; the App's
/// remove-on-resolve admission makes a late fallback a harmless no-op.
struct PoolDrainCompletionGuard {
    host: crate::remote_source::RemoteHostKey,
    generation: u64,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    completed: bool,
}

impl PoolDrainCompletionGuard {
    fn new(
        host: crate::remote_source::RemoteHostKey,
        generation: u64,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    ) -> Self {
        Self {
            host,
            generation,
            event_tx,
            completed: false,
        }
    }

    /// Send the normal pool-drain completion and disarm the fallback. Idempotent.
    fn complete(&mut self) {
        if self.completed {
            return;
        }
        self.completed = true;
        let _ =
            self.event_tx
                .blocking_send(crate::events::AppEvent::RemoteSourcePoolDrainCompleted {
                    host: self.host.clone(),
                    generation: self.generation,
                });
    }
}

impl Drop for PoolDrainCompletionGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        // Panic while dropping/reaping drained connections: still report
        // completion so the disconnect responder is never stranded. Harmless
        // if the generation was already superseded/resolved.
        self.completed = true;
        let _ =
            self.event_tx
                .blocking_send(crate::events::AppEvent::RemoteSourcePoolDrainCompleted {
                    host: self.host.clone(),
                    generation: self.generation,
                });
    }
}

/// Drain the process-global persistent-bridge pool at process shutdown: reap
/// every idle bridge (closing stdin + reaping the ssh child so `RemoteSsh::Drop`
/// runs `ssh -O exit` control-master cleanup) and advance every host generation
/// so an active bridge returning after this drain is not parked. Safe to call
/// when the pool was never used (it stays uninitialized) and idempotent. Called
/// from the headless server and TUI run exit paths; it never runs on the
/// reducer/AppState hot mutation paths.
pub(crate) fn drain_remote_bridge_pool() {
    if let Some(pool) = REMOTE_AGENT_BRIDGE_POOL.get() {
        pool.drain();
    }
}

/// Run one routed remote-agent request against the persistent-bridge pool.
///
/// Returns:
/// - `Ok(Some(response))` when a pooled bridge served the request.
/// - `Ok(None)` when pool checkout/start failed *before any byte of the
///   request was written* (no reusable idle entry, pool full, or start
///   failed). The caller must fall back to the one-shot prepared path.
/// - `Err(_)` once a write attempt has begun (partial write, broken pipe, EOF,
///   IO, or parse failure). The bridge is discarded; the caller must map this
///   to `remote_request_failed` and must NOT retry or fall back. This uniform
///   rule applies to every routed method, idempotent or not.
///
/// The per-(host, session) limiter (`REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT`) is
/// acquired by the dispatch starter *before* this runs, so a saturated limiter
/// returns `remote_bridge_busy` without any pool checkout/start.
pub(crate) fn dispatch_via_remote_bridge_pool(
    pool: &RemoteAgentBridgePool,
    host: &crate::remote_target::RemoteHostConfig,
    state: &RemoteApiBridgeState,
    request: &crate::api::schema::Request,
    starter: PersistentRemoteBridgeStarter,
) -> io::Result<Option<String>> {
    let key = crate::remote_source::RemoteHostKey::new(&host.name, &host.session);
    let identity = PersistentBridgeIdentity::from_host_state(host, state);

    // Capability validation (pre-write, safe): the normal required methods plus
    // the persistent capability. Failure surfaces exactly like the one-shot
    // prepared path (it becomes `remote_request_failed` at the worker).
    let mut required = required_federation_methods_for_request(request);
    required.push(crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE_PERSISTENT);
    validate_federation_capabilities(host, &state.capabilities, &required)?;

    // 1. Reuse an idle bridge if one matches; otherwise reserve room to start.
    //    `checked_out_generation` captures the host generation at checkout/reserve
    //    so a non-connected transition (or shutdown drain) that advances it
    //    while this connection is active prevents it from being parked on return.
    let (mut connection, checked_out_generation) = match pool.checkout_reusable(&key, &identity) {
        Some((connection, generation)) => {
            tracing::debug!(
                event = "remote.route.bridge_pool",
                subsystem = "remote",
                outcome = "hit",
                host = %host.name,
                session = %host.session,
                "reused idle persistent remote bridge"
            );
            (connection, generation)
        }
        None => {
            let generation = match pool.reserve_new(&key) {
                Some(generation) => generation,
                None => {
                    tracing::debug!(
                        event = "remote.route.bridge_pool",
                        subsystem = "remote",
                        outcome = "disabled_pool_full",
                        host = %host.name,
                        session = %host.session,
                        "persistent bridge pool full; falling back to one-shot"
                    );
                    return Ok(None);
                }
            };
            // Start a fresh persistent connection. This is still pre-write: a
            // failure here releases the reservation and the caller falls back.
            match starter(host, state) {
                Ok(connection) => {
                    tracing::debug!(
                        event = "remote.route.bridge_pool",
                        subsystem = "remote",
                        outcome = "miss",
                        host = %host.name,
                        session = %host.session,
                        "started new persistent remote bridge"
                    );
                    (connection, generation)
                }
                Err(err) => {
                    pool.release_active(&key);
                    tracing::debug!(
                        err = %err,
                        event = "remote.route.bridge_pool",
                        subsystem = "remote",
                        outcome = "start_failed",
                        host = %host.name,
                        session = %host.session,
                        "persistent bridge start failed; falling back to one-shot"
                    );
                    return Ok(None);
                }
            }
        }
    };

    // 2. WRITE BOUNDARY: a connection is now exclusively held. Any failure from
    //    here (partial write, broken pipe on a silently-dead bridge, EOF/IO/parse
    //    on read) maps to `remote_request_failed`: discard the bridge, no retry,
    //    no one-shot fallback. Uniform for every routed method.
    let result = connection
        .write_request(request)
        .and_then(|()| connection.read_response());
    match result {
        Ok(response) => {
            pool.return_connection(
                &key,
                identity,
                connection,
                /* reusable */ true,
                checked_out_generation,
            );
            Ok(Some(response))
        }
        Err(err) => {
            pool.release_active(&key);
            // Reap the discarded bridge outside the pool lock.
            drop(connection);
            tracing::debug!(
                err = %err,
                event = "remote.route.bridge_pool",
                subsystem = "remote",
                outcome = "discarded",
                host = %host.name,
                session = %host.session,
                "persistent remote bridge failed after write; discarded"
            );
            Err(err)
        }
    }
}

/// Production entry point for pooled persistent-bridge dispatch. Consults the
/// process-global pool with the real SSH starter. Returns `Ok(None)` when the
/// caller should fall back to the one-shot prepared path, and `Err(_)` once a
/// write has begun (see [`dispatch_via_remote_bridge_pool`]).
pub(crate) fn try_pooled_remote_api_request(
    host: &crate::remote_target::RemoteHostConfig,
    state: &RemoteApiBridgeState,
    request: &crate::api::schema::Request,
) -> io::Result<Option<String>> {
    dispatch_via_remote_bridge_pool(
        remote_agent_bridge_pool(),
        host,
        state,
        request,
        start_persistent_remote_api_bridge,
    )
}

/// One checked-out (or freshly started) pooled persistent bridge, moved out of
/// the pool and owned exclusively by one routed IO worker for the duration of
/// its sequence. `checked_out_generation` is the pool generation captured at
/// checkout/start; [`return_persistent_bridge`] refuses to park the connection
/// if the host was invalidated while it was active.
pub(crate) struct PooledHandle {
    key: crate::remote_source::RemoteHostKey,
    identity: PersistentBridgeIdentity,
    pub(crate) connection: Box<dyn PersistentRemoteBridgeConnection>,
    checked_out_generation: u64,
}

impl PooledHandle {
    /// Stable per-connection identity for route-selection telemetry (pooled
    /// route + connection id). Derived from the connection object's address:
    /// unique per live connection, never dereferenced.
    pub(crate) fn connection_id(&self) -> usize {
        let pointer: *const dyn PersistentRemoteBridgeConnection = self.connection.as_ref();
        pointer as *const () as usize
    }

    /// Test-only constructor: wraps an injected fake connection in a pooled
    /// handle so the routed executor tests exercise the checkout/return/
    /// release bookkeeping (against a local pool) without real SSH.
    #[cfg(test)]
    pub(crate) fn for_test(
        connection: Box<dyn PersistentRemoteBridgeConnection>,
        key: crate::remote_source::RemoteHostKey,
    ) -> Self {
        Self {
            key,
            identity: PersistentBridgeIdentity {
                shell_path: "test-shell".to_string(),
                capabilities: crate::api::schema::FederationCapabilities {
                    methods: Vec::new(),
                },
                ssh_target: "test-target".to_string(),
                session: "test-session".to_string(),
                manage_ssh_config: false,
                connect_timeout_secs: 1,
            },
            connection,
            checked_out_generation: 0,
        }
    }
}

/// Try to check out or start one pooled persistent bridge for `host` reusing
/// cached prepared `state` (Slice A pooled-first routing for routed sequences).
/// All failure modes here are PRE-WRITE: `Ok(None)` means the caller falls
/// back to the one-shot prepared path (pool full or persistent capability not
/// advertised); `Err` is a capability/start failure that must surface as a
/// request failure (the start failure slot is released first). No request byte
/// is written by this call. `primary_request` drives the required-method
/// validation (the same mapping as the one-shot bridge path).
pub(crate) fn try_checkout_persistent_bridge(
    pool: &RemoteAgentBridgePool,
    host: &crate::remote_target::RemoteHostConfig,
    state: &RemoteApiBridgeState,
    primary_request: &crate::api::schema::Request,
) -> io::Result<Option<PooledHandle>> {
    let key = crate::remote_source::RemoteHostKey::new(&host.name, &host.session);
    let identity = PersistentBridgeIdentity::from_host_state(host, state);

    // Capability validation (pre-write, safe): the primary request's required
    // methods plus the persistent capability. Failure surfaces exactly like the
    // one-shot prepared path (it becomes `remote_request_failed` at the
    // worker).
    let mut required = required_federation_methods_for_request(primary_request);
    required.push(crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE_PERSISTENT);
    validate_federation_capabilities(host, &state.capabilities, &required)?;
    if let Some((connection, generation)) = pool.checkout_reusable(&key, &identity) {
        return Ok(Some(PooledHandle {
            key,
            identity,
            connection,
            checked_out_generation: generation,
        }));
    }
    let Some(generation) = pool.reserve_new(&key) else {
        return Ok(None);
    };
    match start_persistent_remote_api_bridge(host, state) {
        Ok(connection) => Ok(Some(PooledHandle {
            key,
            identity,
            connection,
            checked_out_generation: generation,
        })),
        Err(err) => {
            // Start failure is PRE-WRITE: release the reservation and let the
            // caller fall back to the one-shot prepared path (never a retry
            // after any byte, which has not been written yet).
            pool.release_active(&key);
            tracing::debug!(
                err = %err,
                event = "remote.route.bridge_pool",
                subsystem = "remote",
                outcome = "start_failed",
                host = %host.name,
                session = %host.session,
                "persistent bridge start failed; routed sequence falls back to one-shot"
            );
            Ok(None)
        }
    }
}

/// Return a pooled bridge checked out by [`try_checkout_persistent_bridge`] to
/// the pool (normal completion path; called by the executor, never by the
/// worker). Decrements `active` exactly once and parks the connection only
/// when it is still reusable and its captured generation is current.
pub(crate) fn return_persistent_bridge(pool: &RemoteAgentBridgePool, handle: PooledHandle) {
    let PooledHandle {
        key,
        identity,
        connection,
        checked_out_generation,
    } = handle;
    pool.return_connection(&key, identity, connection, true, checked_out_generation);
}

/// Decrement-only active-slot release for a pooled connection that must
/// never be parked (a timeout or post-write transport/protocol error): the
/// pool must never count or park a connection the caller is about to drop.
/// The caller owns the actual drop; this call only corrects `active`
/// accounting.
pub(crate) fn release_persistent_bridge_active(
    pool: &RemoteAgentBridgePool,
    key: &crate::remote_source::RemoteHostKey,
) {
    pool.release_active(key);
}

impl RemoteHerdr {
    pub(crate) fn shell_path(&self) -> &str {
        &self.shell_path
    }

    fn for_platform(platform: RemotePlatform) -> Self {
        let install_suffix = ".local/bin/herdr".to_string();
        let shell_path = format!("\"$HOME/{install_suffix}\"");
        Self {
            install_suffix,
            shell_path,
            platform,
        }
    }

    fn with_shell_path(mut self, shell_path: String) -> Self {
        self.shell_path = shell_path;
        self
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum RemoteAssetRef {
    Url(String),
    Object { url: String, sha256: Option<String> },
}

impl RemoteAssetRef {
    fn url(&self) -> &str {
        match self {
            Self::Url(url) => url,
            Self::Object { url, .. } => url,
        }
    }

    fn sha256(&self) -> Option<&str> {
        match self {
            Self::Url(_) => None,
            Self::Object { sha256, .. } => {
                sha256.as_deref().filter(|value| !value.trim().is_empty())
            }
        }
    }
}

#[derive(Deserialize)]
struct RemoteUpdateManifest {
    version: String,
    protocol: Option<u32>,
    assets: BTreeMap<String, RemoteAssetRef>,
    #[serde(default, deserialize_with = "deserialize_remote_manifest_releases")]
    releases: BTreeMap<String, RemoteReleaseMetadata>,
}

#[derive(Deserialize)]
struct RemoteReleaseMetadata {
    protocol: Option<u32>,
    #[serde(default)]
    assets: BTreeMap<String, RemoteAssetRef>,
}

#[derive(Deserialize)]
struct RemotePreviewManifest {
    build_id: String,
    protocol: u32,
    assets: BTreeMap<String, RemoteAssetRef>,
    #[serde(default)]
    builds: BTreeMap<String, RemotePreviewBuildMetadata>,
}

#[derive(Deserialize)]
struct RemotePreviewBuildMetadata {
    protocol: u32,
    assets: BTreeMap<String, RemoteAssetRef>,
}

fn deserialize_remote_manifest_releases<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, RemoteReleaseMetadata>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(serde_json::Value::Object(object)) => object
            .into_iter()
            .filter_map(|(version, release)| {
                serde_json::from_value::<RemoteReleaseMetadata>(release)
                    .ok()
                    .map(|metadata| (version, metadata))
            })
            .collect(),
        _ => BTreeMap::new(),
    })
}

impl RemoteUpdateManifest {
    fn release_for_version(&self, version: &str) -> Option<RemoteManifestReleaseRef<'_>> {
        if self.version.trim_start_matches('v') == version {
            return Some(RemoteManifestReleaseRef {
                protocol: self.protocol,
                assets: &self.assets,
            });
        }

        self.releases.get(version).and_then(|release| {
            (!release.assets.is_empty()).then_some(RemoteManifestReleaseRef {
                protocol: release.protocol,
                assets: &release.assets,
            })
        })
    }
}

#[derive(Clone, Copy)]
struct RemoteManifestReleaseRef<'a> {
    protocol: Option<u32>,
    assets: &'a BTreeMap<String, RemoteAssetRef>,
}

fn current_version() -> String {
    crate::build_info::version()
}

fn current_channel() -> &'static str {
    crate::build_info::channel()
}

struct InstallSource {
    path: PathBuf,
    temporary_dir: Option<PathBuf>,
}

struct RemoteReleaseAsset {
    url: String,
    sha256: Option<String>,
}

struct PreparedRemoteHerdr {
    remote_herdr: RemoteHerdr,
    installed_or_replaced: bool,
    stop_after_install_approved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SshInvocationMode {
    Interactive,
    Noninteractive,
}

#[derive(Clone)]
struct ManagedSshOptions {
    config_path: PathBuf,
    control_path: PathBuf,
}

struct ManagedSshConfig {
    options: ManagedSshOptions,
}

impl Drop for ManagedSshConfig {
    fn drop(&mut self) {
        if let Some(dir) = self.options.config_path.parent() {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

struct RemoteSsh {
    target: String,
    managed_config: Option<ManagedSshConfig>,
    /// SSH `ConnectTimeout` override, in seconds. `None` means there is no
    /// configured-host override: bare-target interactive commands keep the
    /// prior no-explicit-`ConnectTimeout` behavior, while bare-target
    /// noninteractive commands (`command_with_mode` with
    /// `SshInvocationMode::Noninteractive`) fall back to
    /// `DEFAULT_CONNECT_TIMEOUT_SECS` to preserve the previous hardcoded
    /// noninteractive timeout. `Some(secs)` is used for configured-host
    /// connections, which carry a bounded `connect_timeout_secs` and always
    /// win for both interactive and noninteractive commands.
    connect_timeout_secs: Option<u32>,
}

impl RemoteSsh {
    fn new(target: String, manage_ssh_config: bool, connect_timeout_secs: Option<u32>) -> Self {
        let managed_config = if manage_ssh_config {
            write_managed_ssh_config()
                .inspect_err(|err| {
                    tracing::debug!(%err, "could not write managed ssh config; using plain ssh");
                })
                .ok()
        } else {
            None
        };

        Self {
            target,
            managed_config,
            connect_timeout_secs,
        }
    }

    /// Bare target-only helpers (e.g. `herdr --remote <target>`) have no
    /// `RemoteHostConfig`, so this carries no configured-host override
    /// (`None`). Interactive commands preserve prior behavior: no explicit
    /// `ConnectTimeout`. Noninteractive commands (`command_with_mode` with
    /// `SshInvocationMode::Noninteractive`) still fall back to
    /// `DEFAULT_CONNECT_TIMEOUT_SECS`, preserving the previous hardcoded
    /// noninteractive timeout.
    fn new_for_target(target: String, manage_ssh_config: bool) -> Self {
        Self::new(target, manage_ssh_config, None)
    }

    /// Configured-host connections carry a bounded `connect_timeout_secs`.
    fn new_for_host(target: String, manage_ssh_config: bool, connect_timeout_secs: u32) -> Self {
        Self::new(target, manage_ssh_config, Some(connect_timeout_secs))
    }

    fn target(&self) -> &str {
        &self.target
    }

    fn options(&self) -> Option<&ManagedSshOptions> {
        self.managed_config.as_ref().map(|config| &config.options)
    }

    fn command(&self) -> Command {
        self.command_with_mode(SshInvocationMode::Interactive)
    }

    fn command_with_mode(&self, mode: SshInvocationMode) -> Command {
        let mut command = self.base_command();
        command.arg("-T");
        // Interactive commands only get an explicit `ConnectTimeout` when a
        // configured host provides one; bare-target interactive commands
        // keep the prior no-explicit-timeout behavior. Noninteractive
        // commands always get an explicit `ConnectTimeout`: a configured
        // host's bound wins, and bare-target noninteractive commands fall
        // back to `DEFAULT_CONNECT_TIMEOUT_SECS` to preserve the previous
        // hardcoded noninteractive timeout.
        let effective_connect_timeout_secs = match mode {
            SshInvocationMode::Interactive => self.connect_timeout_secs,
            SshInvocationMode::Noninteractive => Some(
                self.connect_timeout_secs
                    .unwrap_or(crate::remote_target::DEFAULT_CONNECT_TIMEOUT_SECS),
            ),
        };
        if let Some(connect_timeout_secs) = effective_connect_timeout_secs {
            command
                .arg("-o")
                .arg(format!("ConnectTimeout={connect_timeout_secs}"));
        }
        if mode == SshInvocationMode::Noninteractive {
            command.args(NONINTERACTIVE_SSH_OPTIONS);
        }
        command.arg(&self.target);
        command
    }

    fn base_command(&self) -> Command {
        let mut command = Command::new("ssh");
        apply_managed_ssh_options(&mut command, self.options());
        command
    }

    fn sh_output(&self, script: &str) -> io::Result<Output> {
        self.sh_output_with_mode(script, SshInvocationMode::Interactive)
    }

    fn sh_output_with_mode(&self, script: &str, mode: SshInvocationMode) -> io::Result<Output> {
        let mut child = self
            .command_with_mode(mode)
            .arg("/bin/sh -s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let write_result = if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(script.as_bytes())
        } else {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh bootstrap stdin missing",
            ))
        };
        let output = child.wait_with_output()?;
        write_result?;
        Ok(output)
    }

    fn user_shell_output(&self, command: &str) -> io::Result<Output> {
        self.user_shell_output_with_mode(command, SshInvocationMode::Interactive)
    }

    fn user_shell_output_with_mode(
        &self,
        command: &str,
        mode: SshInvocationMode,
    ) -> io::Result<Output> {
        self.command_with_mode(mode).arg(command).output()
    }

    fn install_herdr(&self, remote_herdr: &RemoteHerdr, source_path: &Path) -> io::Result<()> {
        let script = format!(
            r#"dest="$HOME/{install_suffix}"
dir="${{dest%/*}}"
mkdir -p "$dir"
tmp="${{dest}}.tmp.$$"
cat > "$tmp"
chmod 755 "$tmp"
mv "$tmp" "$dest"
"#,
            install_suffix = remote_herdr.install_suffix
        );

        let mut child = self
            .command()
            .arg(format!("/bin/sh -eu -c {}", shell_quote(&script)))
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|err| {
                io::Error::new(err.kind(), format!("failed to start ssh install: {err}"))
            })?;

        let mut source = File::open(source_path)?;
        let copy_result = if let Some(mut stdin) = child.stdin.take() {
            io::copy(&mut source, &mut stdin).map(|_| ())
        } else {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "ssh install stdin missing",
            ))
        };
        let status = child.wait()?;
        copy_result?;

        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "remote install exited with {status}"
            )))
        }
    }
}

impl Drop for RemoteSsh {
    fn drop(&mut self) {
        if self.managed_config.is_none() {
            return;
        }

        let _ = self
            .base_command()
            .arg("-O")
            .arg("exit")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg(&self.target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn apply_managed_ssh_options(command: &mut Command, options: Option<&ManagedSshOptions>) {
    let Some(options) = options else {
        return;
    };

    command
        .arg("-F")
        .arg(&options.config_path)
        .arg("-S")
        .arg(&options.control_path)
        .arg("-o")
        .arg("ControlMaster=auto")
        .arg("-o")
        .arg("ControlPersist=yes");
}

/// Applies managed ssh config/control-socket options plus an optional bounded
/// `ConnectTimeout` to a bridge `ssh` command, mirroring
/// `RemoteSsh::command_with_mode`'s behavior: bare bridges (no
/// `RemoteHostConfig`) pass `None` and keep the prior no-explicit-timeout
/// behavior, while configured-host bridges pass `Some(host.connect_timeout_secs)`.
fn apply_bridge_ssh_options(
    command: &mut Command,
    ssh_options: Option<&ManagedSshOptions>,
    connect_timeout_secs: Option<u32>,
) {
    apply_managed_ssh_options(command, ssh_options);
    command.arg("-T");
    if let Some(connect_timeout_secs) = connect_timeout_secs {
        command
            .arg("-o")
            .arg(format!("ConnectTimeout={connect_timeout_secs}"));
    }
}

impl InstallSource {
    fn persistent(path: PathBuf) -> Self {
        Self {
            path,
            temporary_dir: None,
        }
    }

    fn temporary(path: PathBuf, temporary_dir: PathBuf) -> Self {
        Self {
            path,
            temporary_dir: Some(temporary_dir),
        }
    }

    fn cleanup(&self) {
        if let Some(dir) = &self.temporary_dir {
            let _ = fs::remove_dir_all(dir);
        }
    }
}

fn prepare_remote_herdr(
    ssh: &RemoteSsh,
    live_handoff_enabled: bool,
) -> io::Result<PreparedRemoteHerdr> {
    let platform = detect_remote_platform(ssh)?;
    let remote_herdr = RemoteHerdr::for_platform(platform);
    let override_binary = remote_binary_override_path()?;
    let remote_binary_candidates = remote_binary_candidates(ssh, &remote_herdr)?;

    if override_binary.is_none() {
        for candidate in &remote_binary_candidates {
            if remote_binary_matches(ssh, candidate).unwrap_or(false) {
                return Ok(PreparedRemoteHerdr {
                    remote_herdr: candidate.clone(),
                    installed_or_replaced: false,
                    stop_after_install_approved: false,
                });
            }
        }
        if remote_binary_matches(ssh, &remote_herdr)? {
            return Ok(PreparedRemoteHerdr {
                remote_herdr,
                installed_or_replaced: false,
                stop_after_install_approved: false,
            });
        }
    }

    let mut stop_after_install_approved = false;
    if let Some(status_probe_herdr) = remote_binary_candidates.first().or_else(|| {
        remote_binary_exists(ssh, &remote_herdr)
            .ok()
            .and_then(|exists| exists.then_some(&remote_herdr))
    }) {
        stop_after_install_approved = confirm_remote_install_with_running_server(
            ssh,
            status_probe_herdr,
            live_handoff_enabled,
        )?;
    }
    confirm_remote_install(
        ssh.target(),
        &remote_herdr,
        &install_source_description(&remote_herdr.platform, override_binary.as_deref()),
    )?;
    let source = resolve_install_source(&remote_herdr.platform, override_binary)?;
    let install_result = ssh.install_herdr(&remote_herdr, &source.path);
    source.cleanup();
    install_result?;

    if !remote_binary_matches(ssh, &remote_herdr)? {
        return Err(io::Error::other(format!(
            "installed remote herdr at {}, but it did not report version {}",
            remote_herdr.shell_path,
            current_version()
        )));
    }
    warn_if_remote_bin_not_on_path(ssh)?;

    Ok(PreparedRemoteHerdr {
        remote_herdr,
        installed_or_replaced: true,
        stop_after_install_approved,
    })
}

fn detect_remote_platform(ssh: &RemoteSsh) -> io::Result<RemotePlatform> {
    detect_remote_platform_with_mode(ssh, SshInvocationMode::Interactive)
}

fn prepare_remote_herdr_noninteractive(ssh: &RemoteSsh) -> io::Result<RemoteHerdr> {
    let platform = detect_remote_platform_with_mode(ssh, SshInvocationMode::Noninteractive)?;
    let remote_herdr = RemoteHerdr::for_platform(platform);

    if let Some(path_remote_herdr) =
        remote_binary_on_path_any_with_mode(ssh, &remote_herdr, SshInvocationMode::Noninteractive)?
            .filter(|candidate| {
                remote_binary_matches_with_mode(ssh, candidate, SshInvocationMode::Noninteractive)
                    .unwrap_or(false)
            })
    {
        return Ok(path_remote_herdr);
    }

    if remote_binary_matches_with_mode(ssh, &remote_herdr, SshInvocationMode::Noninteractive)? {
        return Ok(remote_herdr);
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "compatible herdr v{} protocol {} was not found on remote host {}; run `herdr --remote {}` interactively to install or upgrade it",
            current_version(),
            CURRENT_PROTOCOL,
            ssh.target(),
            ssh.target()
        ),
    ))
}

fn detect_remote_platform_with_mode(
    ssh: &RemoteSsh,
    mode: SshInvocationMode,
) -> io::Result<RemotePlatform> {
    let output = ssh.sh_output_with_mode("uname -s\nuname -m\n", mode)?;
    if !output.status.success() {
        return Err(command_failed("remote platform detection failed", &output));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let os = lines.next().unwrap_or_default();
    let arch = lines.next().unwrap_or_default();
    RemotePlatform::from_uname(os, arch).ok_or_else(|| {
        io::Error::other(format!(
            "unsupported remote platform: {} {}",
            os.trim(),
            arch.trim()
        ))
    })
}

fn remote_binary_candidates(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<Vec<RemoteHerdr>> {
    let mut candidates = Vec::new();

    if let Some(path_candidate) = remote_binary_on_path_any(ssh, remote_herdr)? {
        push_if_new_remote_binary_candidate(&mut candidates, path_candidate);
    }

    let output = ssh.sh_output(&known_remote_binary_candidate_script(
        &remote_herdr.platform,
    ))?;
    if !output.status.success() {
        return Err(command_failed("remote binary discovery failed", &output));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for candidate in remote_herdrs_from_path_discovery(remote_herdr, &stdout) {
        push_if_new_remote_binary_candidate(&mut candidates, candidate);
    }

    Ok(candidates)
}

fn push_if_new_remote_binary_candidate(candidates: &mut Vec<RemoteHerdr>, candidate: RemoteHerdr) {
    if !candidates
        .iter()
        .any(|existing| existing.shell_path == candidate.shell_path)
    {
        candidates.push(candidate);
    }
}

fn known_remote_binary_candidate_script(platform: &RemotePlatform) -> String {
    let mut script = String::from(
        r#"home=${HOME:-}
user=${USER:-}
version="#,
    );
    script.push_str(&shell_quote(&current_version()));
    script.push_str(
        r#"
emit() {
    path=$1
    if [ -n "$path" ] && [ -x "$path" ]; then
        printf '%s\n' "$path"
    fi
}
if [ -n "$home" ]; then
    emit "$home/.local/bin/herdr"
fi
"#,
    );
    if platform.os == "macos" {
        script.push_str(
            r#"    emit "/opt/homebrew/bin/herdr"
    emit "/usr/local/bin/herdr"
"#,
        );
    } else if platform.os == "linux" {
        script.push_str(
            r#"    emit "/home/linuxbrew/.linuxbrew/bin/herdr"
"#,
        );
    }
    script.push_str(
        r#"if [ -n "$home" ]; then
    emit "$home/.local/share/mise/installs/herdr/$version/bin/herdr"
    emit "$home/.local/share/mise/installs/github-ogulcancelik-herdr/$version/herdr"
    emit "$home/.nix-profile/bin/herdr"
fi
if [ -n "$user" ]; then
    emit "/etc/profiles/per-user/$user/bin/herdr"
fi
emit "/nix/var/nix/profiles/default/bin/herdr"
emit "/run/current-system/sw/bin/herdr"
"#,
    );

    script
}

fn remote_binary_on_path_any(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<Option<RemoteHerdr>> {
    remote_binary_on_path_any_with_mode(ssh, remote_herdr, SshInvocationMode::Interactive)
}

fn remote_binary_on_path_any_with_mode(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    mode: SshInvocationMode,
) -> io::Result<Option<RemoteHerdr>> {
    let output = ssh.user_shell_output_with_mode("command -v herdr", mode)?;
    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(remote_herdr_from_path_discovery(remote_herdr, &stdout))
}

fn remote_herdrs_from_path_discovery(remote_herdr: &RemoteHerdr, stdout: &str) -> Vec<RemoteHerdr> {
    stdout
        .lines()
        .filter_map(|path| remote_herdr_from_path(remote_herdr, path))
        .collect()
}

fn remote_herdr_from_path_discovery(
    remote_herdr: &RemoteHerdr,
    stdout: &str,
) -> Option<RemoteHerdr> {
    stdout
        .lines()
        .find_map(|path| remote_herdr_from_path(remote_herdr, path))
}

fn remote_herdr_from_path(remote_herdr: &RemoteHerdr, path: &str) -> Option<RemoteHerdr> {
    let path = path.trim();
    if !path.starts_with('/') {
        return None;
    }
    if is_mise_shim_path(path) {
        return None;
    }
    Some(remote_herdr.clone().with_shell_path(shell_quote(path)))
}

fn is_mise_shim_path(path: &str) -> bool {
    path.ends_with("/mise/shims/herdr")
}

fn remote_binary_matches(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<bool> {
    remote_binary_matches_with_mode(ssh, remote_herdr, SshInvocationMode::Interactive)
}

fn remote_binary_matches_with_mode(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    mode: SshInvocationMode,
) -> io::Result<bool> {
    let command = format!(
        "test -x {0} && {0} --version && {0} status client --json",
        remote_herdr.shell_path
    );
    let output = ssh.sh_output_with_mode(&command, mode)?;
    if !output.status.success() {
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let version = lines.next().unwrap_or_default().trim();
    let status = lines.next().unwrap_or_default();
    Ok(version == format!("herdr {}", current_version())
        && parse_client_status_json(status)
            .map(|status| status.protocol == CURRENT_PROTOCOL)
            .unwrap_or(false))
}

fn remote_binary_exists(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<bool> {
    let command = format!("test -x {}", remote_herdr.shell_path);
    Ok(ssh.sh_output(&command)?.status.success())
}

fn remote_binary_override_path() -> io::Result<Option<PathBuf>> {
    let Some(value) = std::env::var_os(REMOTE_BINARY_ENV_VAR) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{REMOTE_BINARY_ENV_VAR} must not be empty"),
        ));
    }

    let path = PathBuf::from(value);
    let metadata = fs::metadata(&path).map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to inspect {REMOTE_BINARY_ENV_VAR} path {}: {err}",
                path.display()
            ),
        )
    })?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{REMOTE_BINARY_ENV_VAR} path is not a file: {}",
                path.display()
            ),
        ));
    }

    Ok(Some(path))
}

fn install_source_description(platform: &RemotePlatform, override_binary: Option<&Path>) -> String {
    install_source_description_for(
        platform,
        override_binary,
        local_binary_can_seed_remote(platform),
    )
}

fn install_source_description_for(
    platform: &RemotePlatform,
    override_binary: Option<&Path>,
    local_binary_can_seed_remote: bool,
) -> String {
    if let Some(path) = override_binary {
        return format!("{REMOTE_BINARY_ENV_VAR} ({})", path.display());
    }

    if local_binary_can_seed_remote {
        "the current local herdr binary".to_string()
    } else {
        format!(
            "the {} {} asset for {}",
            current_version(),
            current_channel(),
            platform.asset_key()
        )
    }
}

fn resolve_install_source(
    platform: &RemotePlatform,
    override_binary: Option<PathBuf>,
) -> io::Result<InstallSource> {
    if let Some(path) = override_binary {
        return Ok(InstallSource::persistent(path));
    }

    if *platform == RemotePlatform::local() {
        let path = std::env::current_exe()?;
        if !crate::update::is_package_manager_managed_exe_path(&path) {
            return Ok(InstallSource::persistent(path));
        }
    }

    download_release_asset(platform)
}

fn local_binary_can_seed_remote(platform: &RemotePlatform) -> bool {
    if *platform != RemotePlatform::local() {
        return false;
    }

    std::env::current_exe()
        .map(|path| !crate::update::is_package_manager_managed_exe_path(&path))
        .unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteServerStatus {
    Running {
        version: Option<String>,
        protocol: Option<u32>,
        live_handoff: bool,
        detached_server_daemon: bool,
    },
    NotRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteServerRestartReason {
    ProtocolMismatch,
    DaemonDetachMissing,
    BinaryUpdated,
    VersionMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteInstallRunningServerPlan {
    KeepRunning,
    LiveHandoff,
    StopRequired(RemoteServerRestartReason),
}

fn ensure_remote_server_ready(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    remote_binary_changed: bool,
    stop_after_install_approved: bool,
    live_handoff_enabled: bool,
) -> io::Result<()> {
    let status = remote_server_status(ssh, remote_herdr)?;
    let RemoteServerStatus::Running {
        version,
        protocol,
        live_handoff,
        detached_server_daemon,
    } = status
    else {
        return Ok(());
    };

    let Some(reason) = remote_server_restart_reason(
        version.as_deref(),
        protocol,
        detached_server_daemon,
        remote_binary_changed,
    ) else {
        return Ok(());
    };

    if live_handoff_enabled && live_handoff {
        match live_handoff_remote_server(ssh, remote_herdr) {
            Ok(()) => return Ok(()),
            Err(err) => {
                eprintln!("remote live handoff failed: {err}");
                eprintln!("falling back to remote server restart.");
            }
        }
    }

    if stop_after_install_approved {
        stop_remote_server(ssh, remote_herdr)?;
        return Ok(());
    }

    if confirm_remote_server_stop(ssh.target(), version.as_deref(), protocol, reason)? {
        stop_remote_server(ssh, remote_herdr)?;
    }
    Ok(())
}

fn remote_server_restart_reason(
    version: Option<&str>,
    protocol: Option<u32>,
    detached_server_daemon: bool,
    remote_binary_changed: bool,
) -> Option<RemoteServerRestartReason> {
    if protocol != Some(CURRENT_PROTOCOL) {
        return Some(RemoteServerRestartReason::ProtocolMismatch);
    }
    if !detached_server_daemon {
        return Some(RemoteServerRestartReason::DaemonDetachMissing);
    }
    if version != Some(current_version().as_str()) {
        return Some(RemoteServerRestartReason::VersionMismatch);
    }
    if remote_binary_changed {
        return Some(RemoteServerRestartReason::BinaryUpdated);
    }
    None
}

fn confirm_remote_install_with_running_server(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
    live_handoff_enabled: bool,
) -> io::Result<bool> {
    let target = ssh.target();
    let status = match remote_server_status(ssh, remote_herdr) {
        Ok(status) => status,
        Err(err) => {
            if !io::stdin().is_terminal() {
                return Err(io::Error::other(format!(
                    "could not inspect the running remote herdr server on {target} before installing: {err}; run from an interactive terminal to approve updating the remote binary"
                )));
            }
            eprintln!(
                "could not inspect the running remote herdr server on {target} before installing: {err}"
            );
            eprint!("continue installing the remote herdr binary? [y/N] ");
            io::stderr().flush()?;

            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            let answer = answer.trim().to_ascii_lowercase();
            if answer != "y" && answer != "yes" {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "remote herdr install cancelled",
                ));
            }
            return Ok(false);
        }
    };
    let RemoteServerStatus::Running {
        version,
        protocol,
        live_handoff,
        detached_server_daemon,
    } = &status
    else {
        return Ok(false);
    };
    let plan = remote_install_running_server_plan(
        version.as_deref(),
        *protocol,
        *detached_server_daemon,
        true,
        *live_handoff,
        live_handoff_enabled,
    );

    if plan == RemoteInstallRunningServerPlan::KeepRunning {
        if io::stdin().is_terminal() {
            eprintln!("remote herdr server on {target} is already compatible:");
            eprintln!("  server: v{}", version_label(version.as_deref()));
            eprintln!(
                "Herdr will install {} without stopping the running remote server.",
                current_version()
            );
        }
        return Ok(false);
    }

    if !io::stdin().is_terminal() {
        match plan {
            RemoteInstallRunningServerPlan::LiveHandoff => return Ok(false),
            RemoteInstallRunningServerPlan::StopRequired(_) => {
                return Err(io::Error::other(format!(
                    "remote herdr server on {target} is running v{}; run from an interactive terminal to approve stopping it for the update",
                    version_label(version.as_deref())
                )));
            }
            RemoteInstallRunningServerPlan::KeepRunning => return Ok(false),
        }
    }

    if plan == RemoteInstallRunningServerPlan::LiveHandoff {
        eprintln!("remote herdr server on {target} is currently running:");
        eprintln!("  server: v{}", version_label(version.as_deref()));
        eprintln!(
            "Herdr will install {} and hand off live pane processes to the prepared server.",
            current_version()
        );
        return Ok(false);
    }

    eprintln!("remote herdr server on {target} is currently running:");
    eprintln!("  server: v{}", version_label(version.as_deref()));
    eprintln!(
        "To complete the remote update, Herdr must stop the running remote server after installing."
    );
    eprintln!("This stops active remote pane processes, including shells, dev servers, and tests.");
    eprintln!();
    eprint!(
        "Install {} and stop the remote server now? [y/N] ",
        current_version()
    );
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer != "y" && answer != "yes" {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote herdr install cancelled",
        ));
    }

    Ok(true)
}

fn remote_install_running_server_plan(
    version: Option<&str>,
    protocol: Option<u32>,
    detached_server_daemon: bool,
    remote_binary_changed: bool,
    live_handoff: bool,
    live_handoff_enabled: bool,
) -> RemoteInstallRunningServerPlan {
    let Some(reason) = remote_server_restart_reason(
        version,
        protocol,
        detached_server_daemon,
        remote_binary_changed,
    ) else {
        return RemoteInstallRunningServerPlan::KeepRunning;
    };

    if live_handoff_enabled && live_handoff {
        return RemoteInstallRunningServerPlan::LiveHandoff;
    }

    RemoteInstallRunningServerPlan::StopRequired(reason)
}

fn remote_server_status(
    ssh: &RemoteSsh,
    remote_herdr: &RemoteHerdr,
) -> io::Result<RemoteServerStatus> {
    let command = format!("{} status server --json", remote_herdr.shell_path);
    let output = ssh.sh_output(&command)?;
    if !output.status.success() {
        return Err(command_failed("remote server status failed", &output));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_remote_server_status_json(stdout.trim())
}

#[derive(Debug, Deserialize)]
struct RemoteClientStatusJson {
    protocol: u32,
}

#[derive(Debug, Deserialize)]
struct RemoteServerStatusJson {
    running: bool,
    version: Option<String>,
    protocol: Option<u32>,
    capabilities: Option<RemoteServerCapabilitiesJson>,
}

#[derive(Debug, Deserialize)]
struct RemoteServerCapabilitiesJson {
    live_handoff: bool,
    #[serde(default)]
    detached_server_daemon: bool,
}

fn parse_client_status_json(status: &str) -> Option<RemoteClientStatusJson> {
    serde_json::from_str(status).ok()
}

fn parse_remote_server_status_json(status: &str) -> io::Result<RemoteServerStatus> {
    let parsed: RemoteServerStatusJson = serde_json::from_str(status).map_err(|err| {
        io::Error::other(format!(
            "could not parse remote server status JSON from `{status}`: {err}"
        ))
    })?;
    if !parsed.running {
        return Ok(RemoteServerStatus::NotRunning);
    }

    let capabilities = parsed.capabilities;

    Ok(RemoteServerStatus::Running {
        version: parsed.version,
        protocol: parsed.protocol,
        live_handoff: capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.live_handoff),
        detached_server_daemon: capabilities
            .as_ref()
            .is_some_and(|capabilities| capabilities.detached_server_daemon),
    })
}

fn confirm_remote_server_stop(
    target: &str,
    version: Option<&str>,
    _protocol: Option<u32>,
    reason: RemoteServerRestartReason,
) -> io::Result<bool> {
    if !io::stdin().is_terminal() {
        if reason == RemoteServerRestartReason::ProtocolMismatch {
            return Err(io::Error::other(format!(
                "remote herdr server on {target} must stop before this client can attach; run from an interactive terminal to approve stopping it"
            )));
        }

        eprintln!(
            "remote herdr server on {target} is still running v{}; it will use {} after it restarts.",
            version_label(version),
            current_version()
        );
        return Ok(false);
    }

    eprintln!("remote herdr server on {target} is currently running:");
    eprintln!("  server: v{}", version_label(version));
    eprintln!("  prepared binary: {}", current_version());
    eprintln!();

    match reason {
        RemoteServerRestartReason::ProtocolMismatch => {
            eprintln!("the remote server must stop before this client can attach.");
        }
        RemoteServerRestartReason::DaemonDetachMissing => {
            eprintln!(
                "the remote server was started by a herdr build that may not survive SSH connection loss. restart it so network drops disconnect only this client."
            );
        }
        RemoteServerRestartReason::BinaryUpdated => {
            eprintln!(
                "the remote herdr binary was installed or replaced. restart the remote server so it uses the prepared binary."
            );
        }
        RemoteServerRestartReason::VersionMismatch => {
            eprintln!(
                "the remote server is still running a different herdr version. restart it so it uses the prepared binary."
            );
        }
    }

    let prompt = if reason == RemoteServerRestartReason::ProtocolMismatch {
        "stop the remote server and continue attaching? [Y/n] "
    } else {
        "restart the remote server now? [y/N] "
    };
    eprint!("{prompt}");
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer == "y" || answer == "yes" {
        return Ok(true);
    }
    if answer.is_empty() && reason == RemoteServerRestartReason::ProtocolMismatch {
        return Ok(true);
    }
    if reason == RemoteServerRestartReason::ProtocolMismatch {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote herdr server stop cancelled",
        ));
    }

    Ok(false)
}

fn live_handoff_remote_server(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    let command = format!(
        "{} server live-handoff --import-exe {} --expected-protocol {} --expected-version {}",
        remote_herdr.shell_path,
        remote_herdr.shell_path,
        CURRENT_PROTOCOL,
        current_version()
    );
    let output = ssh.sh_output(&command)?;
    if !output.status.success() {
        return Err(command_failed("remote server live handoff failed", &output));
    }

    eprintln!(
        "handed off the remote herdr server on {}; reconnecting to the prepared server.",
        ssh.target()
    );
    Ok(())
}

fn stop_remote_server(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    let command = format!("{} server stop", remote_herdr.shell_path);
    let output = ssh.sh_output(&command)?;
    if !output.status.success() {
        return Err(command_failed("remote server stop failed", &output));
    }

    wait_for_remote_server_shutdown(ssh, remote_herdr)?;
    eprintln!(
        "stopped the remote herdr server on {}; it will restart when the remote client bridge attaches.",
        ssh.target()
    );
    Ok(())
}

fn wait_for_remote_server_shutdown(ssh: &RemoteSsh, remote_herdr: &RemoteHerdr) -> io::Result<()> {
    let deadline = Instant::now() + REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT;
    loop {
        if remote_server_status(ssh, remote_herdr)? == RemoteServerStatus::NotRunning {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "shutdown was requested, but the old remote herdr server on {target} is still responding after {} seconds",
                    REMOTE_SERVER_SHUTDOWN_CONFIRM_TIMEOUT.as_secs(),
                    target = ssh.target()
                ),
            ));
        }
        thread::sleep(REMOTE_SERVER_SHUTDOWN_POLL_INTERVAL);
    }
}

fn version_label(version: Option<&str>) -> &str {
    version.unwrap_or("unknown")
}

fn warn_if_remote_bin_not_on_path(ssh: &RemoteSsh) -> io::Result<()> {
    let output = ssh.user_shell_output("command -v herdr")?;
    if output.status.success()
        && remote_shell_resolves_managed_install(&String::from_utf8_lossy(&output.stdout))
    {
        return Ok(());
    }

    eprintln!(
        "herdr: installed remote binary to ~/.local/bin/herdr, but the remote shell does not resolve `herdr` to that path"
    );
    Ok(())
}

fn remote_shell_resolves_managed_install(stdout: &str) -> bool {
    stdout
        .lines()
        .next()
        .map(str::trim)
        .is_some_and(|path| path.ends_with("/.local/bin/herdr"))
}

fn download_release_asset(platform: &RemotePlatform) -> io::Result<InstallSource> {
    let asset_key = platform.asset_key();
    let asset = remote_release_asset(&asset_key)?;

    let dir = private_download_dir(&asset_key)?;
    let path = dir.join("herdr.tmp");
    let status = Command::new("curl")
        .args(["-sfL", "--max-time", "120", "-o"])
        .arg(&path)
        .arg(&asset.url)
        .status()
        .map_err(|err| io::Error::new(err.kind(), format!("download failed: {err}")))?;
    if !status.success() {
        let _ = fs::remove_dir_all(&dir);
        return Err(io::Error::other("download failed"));
    }
    if let Some(expected) = &asset.sha256 {
        if let Err(err) = crate::checksum::verify_sha256(&path, expected) {
            let _ = fs::remove_dir_all(&dir);
            return Err(io::Error::new(
                err.kind(),
                format!("downloaded remote asset checksum verification failed: {err}"),
            ));
        }
    }

    Ok(InstallSource::temporary(path, dir))
}

fn fetch_remote_manifest(url: &str) -> io::Result<Vec<u8>> {
    let output = Command::new("curl")
        .args([
            "-sfL",
            "--retry",
            "3",
            "--connect-timeout",
            "10",
            "--max-time",
            "20",
            url,
        ])
        .output()
        .map_err(|err| io::Error::new(err.kind(), format!("curl failed: {err}")))?;
    if !output.status.success() {
        return Err(command_failed("failed to fetch update manifest", &output));
    }
    Ok(output.stdout)
}

fn remote_asset_info(asset: &RemoteAssetRef) -> RemoteReleaseAsset {
    RemoteReleaseAsset {
        url: asset.url().to_string(),
        sha256: asset.sha256().map(str::to_string),
    }
}

fn preview_assets_for_build<'a>(
    manifest: &'a RemotePreviewManifest,
    build_id: &str,
) -> io::Result<(u32, &'a BTreeMap<String, RemoteAssetRef>)> {
    if manifest.build_id == build_id {
        return Ok((manifest.protocol, &manifest.assets));
    }
    let build = manifest.builds.get(build_id).ok_or_else(|| {
        io::Error::other(format!(
            "preview manifest no longer includes build {build_id}; run `herdr update` locally or set {REMOTE_BINARY_ENV_VAR}=target/release/herdr"
        ))
    })?;
    Ok((build.protocol, &build.assets))
}

fn remote_release_asset(asset_key: &str) -> io::Result<RemoteReleaseAsset> {
    if crate::build_info::is_preview() {
        let build_id = crate::build_info::build_id().ok_or_else(|| {
            io::Error::other("preview client has no build id; set HERDR_REMOTE_BINARY or install Herdr on the remote manually")
        })?;
        let manifest_bytes = fetch_remote_manifest(PREVIEW_UPDATE_MANIFEST_URL)?;
        let manifest: RemotePreviewManifest =
            serde_json::from_slice(&manifest_bytes).map_err(|err| {
                io::Error::other(format!("failed to parse preview manifest JSON: {err}"))
            })?;
        let (protocol, assets) = preview_assets_for_build(&manifest, build_id)?;
        if protocol != CURRENT_PROTOCOL {
            return Err(io::Error::other(format!(
                "preview manifest has build {build_id} protocol {protocol}, but this client needs protocol {CURRENT_PROTOCOL}; set {REMOTE_BINARY_ENV_VAR}=target/release/herdr or install a matching Herdr on the remote host manually"
            )));
        }
        return assets.get(asset_key).map(remote_asset_info).ok_or_else(|| {
            io::Error::other(format!(
                "no {asset_key} binary in the preview manifest for build {build_id}"
            ))
        });
    }

    let current_version = current_version();
    let manifest_bytes = fetch_remote_manifest(STABLE_UPDATE_MANIFEST_URL)?;
    let manifest: RemoteUpdateManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|err| io::Error::other(format!("failed to parse update manifest JSON: {err}")))?;
    let release = manifest.release_for_version(&current_version).ok_or_else(|| {
        io::Error::other(format!(
            "release manifest does not include herdr {current_version}; build herdr for {} or install it there manually",
            asset_key
        ))
    })?;
    if let Some(protocol) = release.protocol {
        if protocol != CURRENT_PROTOCOL {
            return Err(io::Error::other(format!(
                "release manifest has herdr {current_version} protocol {protocol}, but this client needs protocol {CURRENT_PROTOCOL}; set {REMOTE_BINARY_ENV_VAR}=target/release/herdr or install a matching herdr on the remote host manually"
            )));
        }
    }
    release
        .assets
        .get(asset_key)
        .map(remote_asset_info)
        .ok_or_else(|| {
            io::Error::other(format!(
                "no {asset_key} binary in the release manifest for herdr {current_version}"
            ))
        })
}

fn private_download_dir(asset_key: &str) -> io::Result<PathBuf> {
    let base = std::env::temp_dir();
    for attempt in 0..100 {
        let dir = base.join(format!(
            "herdr-remote-{}-{}-{attempt}",
            std::process::id(),
            asset_key
        ));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "failed to create private herdr remote download directory",
    ))
}

fn confirm_remote_install(
    target: &str,
    remote_herdr: &RemoteHerdr,
    source_description: &str,
) -> io::Result<()> {
    if !io::stdin().is_terminal() {
        return Err(io::Error::other(format!(
            "matching remote herdr {} is not installed at {}; run from an interactive terminal to approve installation",
            current_version(),
            remote_herdr.shell_path
        )));
    }

    eprintln!(
        "matching herdr {} is not installed on {target} for {}.",
        current_version(),
        remote_herdr.platform.asset_key()
    );
    eprint!(
        "Install {} to {}? [Y/n] ",
        source_description, remote_herdr.shell_path
    );
    io::stderr().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    let answer = answer.trim().to_ascii_lowercase();
    if answer == "n" || answer == "no" {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "remote herdr installation cancelled",
        ));
    }

    Ok(())
}

fn remote_bridge_command(remote_herdr: &RemoteHerdr, session_name: &str) -> String {
    remote_bridge_command_for(remote_herdr, session_name, REMOTE_CLIENT_BRIDGE_SUBCOMMAND)
}

fn remote_bridge_command_for(
    remote_herdr: &RemoteHerdr,
    session_name: &str,
    bridge_subcommand: &str,
) -> String {
    remote_bridge_command_for_shell_path(&remote_herdr.shell_path, session_name, bridge_subcommand)
}

/// Build a remote bridge subcommand from a prepared shell path string rather
/// than a full platform-specific [`RemoteHerdr`]. Used by the prepared-state
/// dispatch path, which only has the cached shell path plus advertised
/// capabilities and must not re-run remote binary preparation.
fn remote_bridge_command_for_shell_path(
    shell_path: &str,
    session_name: &str,
    bridge_subcommand: &str,
) -> String {
    let mut command = format!("exec {}", shell_path);
    if session_name != crate::session::DEFAULT_SESSION_NAME {
        command.push_str(" --session ");
        command.push_str(&shell_quote(session_name));
    }
    command.push(' ');
    command.push_str(bridge_subcommand);
    command
}

fn remote_api_bridge_command_for_host(
    remote_herdr: &RemoteHerdr,
    host: &crate::remote_target::RemoteHostConfig,
) -> String {
    remote_bridge_command_for(remote_herdr, &host.session, REMOTE_API_BRIDGE_SUBCOMMAND)
}

/// Build the persistent remote-API bridge command from a prepared shell path
/// string plus optional session, used by the local bridge pool to start a
/// `remote-api-bridge --persistent` SSH child. Reuses the one-shot builder so
/// the `exec <shell> [--session X] remote-api-bridge` shape stays identical,
/// then appends the persistent flag.
fn remote_persistent_api_bridge_command_for_shell_path(
    shell_path: &str,
    session_name: &str,
) -> String {
    let mut command = remote_bridge_command_for_shell_path(
        shell_path,
        session_name,
        REMOTE_API_BRIDGE_SUBCOMMAND,
    );
    command.push(' ');
    command.push_str(REMOTE_API_BRIDGE_PERSISTENT_FLAG);
    command
}

fn reattach_command(
    program: &str,
    target: &str,
    session_name: &str,
    keybindings: RemoteKeybindings,
    live_handoff: bool,
) -> String {
    let program = if program.is_empty() { "herdr" } else { program };
    let mut command = format!("{} --remote {}", shell_quote(program), shell_quote(target));
    if keybindings != RemoteKeybindings::Local {
        command.push_str(" --remote-keybindings ");
        command.push_str(keybindings.as_str());
    }
    if live_handoff {
        command.push_str(" --handoff");
    }
    if session_name != crate::session::DEFAULT_SESSION_NAME {
        command.push_str(" --session ");
        command.push_str(&shell_quote(session_name));
    }
    command
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || matches!(
                    ch,
                    '@' | '%' | '_' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
                )
        })
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

fn command_failed(context: &str, output: &Output) -> io::Error {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    if stderr.is_empty() {
        io::Error::other(format!("{context}: {}", output.status))
    } else {
        io::Error::other(format!("{context}: {stderr}"))
    }
}

/// Maximum simultaneous 1:1 bridge workers for one [`SshStdioBridge`]
/// listener. Deliberately mirrors the authoritative layout pane cap
/// (`crate::app::api::MAX_LAYOUT_PANES`) so a projection can never open more
/// concurrent terminal-session streams than a layout could ever validly
/// contain; a mirrored (not shared-by-reference) constant so this transport
/// module has no dependency on the `app` layer. Kept `pub(crate)` so the
/// `app` layer can pin the two constants equal in a test, matching the
/// existing `REMOTE_AGENT_BRIDGE_POOL_MAX_PER_HOST` pinning pattern.
pub(crate) const BRIDGE_MAX_CONCURRENT_STREAMS: usize = 24;

/// Shared bounded teardown budget for one bridge's whole worker batch
/// (independent of how many workers are open): local sockets are shut down
/// first for every worker, then this is the single ceiling on how long
/// [`SshStdioBridge::shutdown_and_join`] waits before detaching any worker
/// that has not finished, rather than blocking indefinitely on a stalled
/// remote EOF/child exit.
const BRIDGE_TEARDOWN_BUDGET: Duration = Duration::from_millis(800);

/// Grace window a single bridge worker gives its own `ssh` child to exit
/// after the local accepted stream closes, before killing it. Bounds
/// [`bridge_connection`] even when nothing external ever calls
/// [`SshStdioBridge::shutdown_and_join`] (e.g. a natural remote-side hangup).
const BRIDGE_CHILD_REAP_GRACE: Duration = Duration::from_millis(500);

/// One tracked, cancellable per-connection bridge worker. Every worker is a
/// plain 1:1 stdio pipe to its own `ssh` child — no custom multiplexing or
/// shared request framing is introduced by tracking them.
struct BridgeWorkerHandle {
    /// A clone of the accepted local stream, kept only so external teardown
    /// can force it closed before ever attempting to join the worker thread.
    shutdown_stream: Option<UnixStream>,
    /// Set by the worker itself immediately before it returns.
    done: Arc<AtomicBool>,
    join: JoinHandle<()>,
}

fn prune_finished_bridge_workers(workers: &Mutex<Vec<BridgeWorkerHandle>>) {
    let mut guard = workers
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut index = 0;
    while index < guard.len() {
        if guard[index].done.load(Ordering::Acquire) {
            let worker = guard.remove(index);
            // Already finished: this join returns immediately.
            let _ = worker.join.join();
        } else {
            index += 1;
        }
    }
}

pub(crate) struct SshStdioBridge {
    local_socket: PathBuf,
    should_stop: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
    workers: Arc<Mutex<Vec<BridgeWorkerHandle>>>,
    /// Keeps a managed SSH ControlMaster alive for the bridge's lifetime when
    /// the caller has no other `RemoteSsh` of its own already in scope (the
    /// projection bridge starter — see [`start_projection_bridge`]). `None`
    /// for callers that already keep their own `RemoteSsh` alive for at least
    /// as long as the bridge (`run_remote`, `run_remote_terminal_attach`).
    /// Declared last so it drops after `shutdown_and_join` has already torn
    /// down every worker (Rust drops fields in declaration order after a
    /// type's own `Drop::drop` body returns), so the control master never
    /// exits while a worker might still want it.
    _ssh_keepalive: Option<RemoteSsh>,
}

impl SshStdioBridge {
    fn start(
        target: String,
        remote_herdr: RemoteHerdr,
        local_socket: PathBuf,
        session_name: String,
        ssh_options: Option<&ManagedSshOptions>,
        connect_timeout_secs: Option<u32>,
    ) -> io::Result<Self> {
        let bridge_command = remote_bridge_command(&remote_herdr, &session_name);
        Self::start_with_bridge_command(
            target,
            bridge_command,
            local_socket,
            ssh_options,
            connect_timeout_secs,
            BRIDGE_MAX_CONCURRENT_STREAMS,
            None,
            PathBuf::from("ssh"),
        )
    }

    /// Builds a bridge whose per-connection remote command is already fully
    /// resolved, bounded to `max_concurrent` simultaneous 1:1 pipes. Each
    /// accepted local connection gets its own independent tracked,
    /// cancellable worker thread; the accept loop keeps accepting while
    /// workers run concurrently instead of serializing behind the first
    /// connection. No custom multiplexer is introduced: every worker is still
    /// a plain 1:1 stdio pipe to its own `ssh` child.
    fn start_with_bridge_command(
        target: String,
        bridge_command: String,
        local_socket: PathBuf,
        ssh_options: Option<&ManagedSshOptions>,
        connect_timeout_secs: Option<u32>,
        max_concurrent: usize,
        ssh_keepalive: Option<RemoteSsh>,
        ssh_program: PathBuf,
    ) -> io::Result<Self> {
        let _ = std::fs::remove_file(&local_socket);
        let listener = UnixListener::bind(&local_socket)?;
        crate::ipc::restrict_socket_permissions(&local_socket, BRIDGE_SOCKET_PERMISSION_MODE)?;
        listener.set_nonblocking(true)?;

        let should_stop = Arc::new(AtomicBool::new(false));
        let workers: Arc<Mutex<Vec<BridgeWorkerHandle>>> = Arc::new(Mutex::new(Vec::new()));
        let active_count = Arc::new(AtomicUsize::new(0));

        let thread_stop = Arc::clone(&should_stop);
        let thread_workers = Arc::clone(&workers);
        let thread_active = Arc::clone(&active_count);
        let thread_ssh_options = ssh_options.cloned();
        let accept_thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        prune_finished_bridge_workers(&thread_workers);
                        if thread_active.load(Ordering::Acquire) >= max_concurrent {
                            // Backstop cap only: normal admission never asks for
                            // more than `max_concurrent` streams to begin with,
                            // so this guards a caller bug rather than doing any
                            // real work. Refuse the connection immediately —
                            // never queue it and never multiplex it onto an
                            // existing worker.
                            drop(stream);
                            continue;
                        }
                        if let Err(err) = stream.set_nonblocking(false) {
                            eprintln!(
                                "herdr: remote bridge failed to prepare client socket: {err}"
                            );
                            continue;
                        }
                        let shutdown_stream = match stream.try_clone() {
                            Ok(clone) => clone,
                            Err(err) => {
                                eprintln!(
                                    "herdr: remote bridge failed to clone client socket: {err}"
                                );
                                continue;
                            }
                        };
                        thread_active.fetch_add(1, Ordering::AcqRel);
                        let done = Arc::new(AtomicBool::new(false));
                        let worker_done = Arc::clone(&done);
                        let worker_active = Arc::clone(&thread_active);
                        let worker_target = target.clone();
                        let worker_command = bridge_command.clone();
                        let worker_ssh_options = thread_ssh_options.clone();
                        let worker_ssh_program = ssh_program.clone();
                        let join = thread::spawn(move || {
                            if let Err(err) = bridge_connection(
                                stream,
                                &worker_ssh_program,
                                &worker_target,
                                &worker_command,
                                worker_ssh_options.as_ref(),
                                connect_timeout_secs,
                            ) {
                                eprintln!("herdr: remote bridge worker failed: {err}");
                            }
                            worker_active.fetch_sub(1, Ordering::AcqRel);
                            worker_done.store(true, Ordering::Release);
                        });
                        thread_workers
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(BridgeWorkerHandle {
                                shutdown_stream: Some(shutdown_stream),
                                done,
                                join,
                            });
                    }
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(BRIDGE_ACCEPT_POLL);
                    }
                    Err(err) => {
                        eprintln!("herdr: remote bridge listener failed: {err}");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            local_socket,
            should_stop,
            accept_thread: Some(accept_thread),
            workers,
            _ssh_keepalive: ssh_keepalive,
        })
    }

    /// Genuinely bounded teardown, in this mandatory order: (1) stop
    /// admitting new connections, (2) force-close every still-open accepted
    /// local socket so each worker's blocking IO unblocks (a worker never
    /// gets joined while its local stream handle might still be live), (3)
    /// give the whole batch one shared bounded budget to finish naturally
    /// (each worker's own `ssh`-child reap is itself bounded — see
    /// `bridge_connection`), then (4) join only the workers that finished in
    /// time and detach the rest. Detaching instead of blocking further is
    /// safe: their local socket is already shut and their internal reap is
    /// bounded, so they still terminate promptly on their own, but this call
    /// never blocks the caller past the shared budget regardless of pane
    /// count — even if a remote EOF/child exit stalls.
    fn shutdown_and_join(&mut self) {
        self.should_stop.store(true, Ordering::Release);
        let _ = std::fs::remove_file(&self.local_socket);
        if let Some(accept_thread) = self.accept_thread.take() {
            let _ = accept_thread.join();
        }

        let mut workers = {
            let mut guard = self
                .workers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *guard)
        };

        // Local socket shutdown BEFORE any join, for every worker, whether or
        // not it has already finished on its own.
        for worker in &mut workers {
            if let Some(stream) = worker.shutdown_stream.take() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                // Drop the final manager-owned local stream handle before any
                // worker join attempt. The worker's own accepted stream is now
                // shutdown at the OS level and its blocking IO is unblocked.
                drop(stream);
            }
        }

        let deadline = Instant::now() + BRIDGE_TEARDOWN_BUDGET;
        while !workers
            .iter()
            .all(|worker| worker.done.load(Ordering::Acquire))
        {
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        for worker in workers.drain(..) {
            if worker.done.load(Ordering::Acquire) {
                let _ = worker.join.join();
            } else {
                // Never join while a live local stream handle might remain:
                // the socket is already shut and `bridge_connection` bounds
                // its own ssh-child reap, so the thread finishes on its own;
                // detach it instead of blocking this call further.
                drop(worker.join);
            }
        }
    }
}

impl Drop for SshStdioBridge {
    fn drop(&mut self) {
        self.shutdown_and_join();
    }
}

/// Creates a fresh user-only (`0700`) directory for the generated ssh config
/// and control socket, returning its path.
///
/// Using a private directory created with fail-if-exists semantics — rather
/// than a predictable file in the world-writable temp dir — stops a local user
/// from pre-planting a symlink or world-writable file that herdr would write
/// and `ssh -F` would then read.
fn private_ssh_config_dir() -> io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;

    let mut bases = vec![std::env::temp_dir()];
    let short_tmp = PathBuf::from("/tmp");
    if bases.first() != Some(&short_tmp) {
        bases.push(short_tmp);
    }

    let mut last_error = None;
    for base in bases {
        for attempt in 0..100 {
            let dir = base.join(format!("herdr-ssh-{}-{attempt}", std::process::id()));
            if !fits_unix_socket_path(&dir.join(SSH_CONTROL_SOCKET_NAME)) {
                continue;
            }
            match fs::DirBuilder::new().mode(0o700).create(&dir) {
                Ok(()) => return Ok(dir),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(err) => {
                    last_error = Some(err);
                    break;
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "failed to create private herdr ssh config directory",
        )
    }))
}

/// Quotes a path for an ssh_config `Include` so a path containing spaces (or
/// glob metacharacters) is treated as one literal token instead of being split
/// or expanded by ssh — otherwise the user's config might not be Included and
/// herdr's fallback would wrongly take effect.
fn ssh_config_quote(path: &str) -> String {
    format!("\"{path}\"")
}

/// Builds a temporary ssh config for remote attach commands without overriding
/// the user's own settings, returning its path.
///
/// The file `Include`s the user's real ssh config first, so ssh's
/// first-value-wins rule keeps any `ServerAlive*` the user set there (including
/// an explicit `0` to disable it). Herdr's keepalive values apply only when
/// the user has none.
fn write_managed_ssh_config() -> io::Result<ManagedSshConfig> {
    use std::os::unix::fs::OpenOptionsExt;

    let dir = private_ssh_config_dir()?;
    let path = dir.join("config");
    let control_path = dir.join(SSH_CONTROL_SOCKET_NAME);

    let mut contents = String::new();
    if let Some(home) = std::env::var_os("HOME") {
        let user_config = PathBuf::from(home).join(".ssh").join("config");
        if user_config.is_file() {
            contents.push_str(&format!(
                "Include {}\n",
                ssh_config_quote(&user_config.to_string_lossy())
            ));
        }
    }
    if Path::new("/etc/ssh/ssh_config").is_file() {
        contents.push_str("Include /etc/ssh/ssh_config\n");
    }
    contents.push_str("Host *\n");
    contents.push_str("  ServerAliveInterval 15\n");
    contents.push_str("  ServerAliveCountMax 4\n");

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(BRIDGE_SOCKET_PERMISSION_MODE)
        .open(&path)?;
    file.write_all(contents.as_bytes())?;
    Ok(ManagedSshConfig {
        options: ManagedSshOptions {
            config_path: path,
            control_path,
        },
    })
}

fn bridge_connection(
    stream: UnixStream,
    ssh_program: &Path,
    target: &str,
    bridge_command: &str,
    ssh_options: Option<&ManagedSshOptions>,
    connect_timeout_secs: Option<u32>,
) -> io::Result<()> {
    let mut command = Command::new(ssh_program);
    apply_bridge_ssh_options(&mut command, ssh_options, connect_timeout_secs);
    command.arg(target).arg(bridge_command);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child = command
        .spawn()
        .map_err(|err| io::Error::new(err.kind(), format!("failed to start ssh bridge: {err}")))?;
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "ssh bridge stdin missing"))?;
    let mut child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "ssh bridge stdout missing"))?;
    let mut stream_to_child = stream.try_clone()?;
    let mut child_to_stream = stream;

    let upload = thread::spawn(move || {
        let _ = copy_flush(&mut stream_to_child, &mut child_stdin);
    });
    let download = thread::spawn(move || {
        let _ = copy_flush(&mut child_stdout, &mut child_to_stream);
        let _ = child_to_stream.shutdown(std::net::Shutdown::Write);
    });

    // Wait only for local->child upload first. External bridge teardown has
    // already shut the accepted local socket, so this returns and drops child
    // stdin. Do NOT join the child->local download yet: it can be blocked on a
    // stalled remote EOF until the ssh child is killed below.
    let _ = upload.join();

    // Bounded reap instead of a plain blocking `wait()`: even if the remote
    // side's EOF/child exit stalls (e.g. a wedged network path), this worker
    // terminates within `BRIDGE_CHILD_REAP_GRACE` by killing the ssh child.
    // Killing/exit closes child stdout, which unblocks the download thread; only
    // then is it safe to join that thread.
    let status = reap_ssh_bridge_child(&mut child, BRIDGE_CHILD_REAP_GRACE)?;
    let _ = download.join();
    match status {
        Some(status) if status.success() => Ok(()),
        Some(status) => Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            format!("ssh bridge exited with {status}"),
        )),
        None => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "ssh bridge did not exit after local stream closed; killed after grace window",
        )),
    }
}

/// Waits up to `grace` for `child` to exit on its own, then kills and reaps
/// it. Returns `Ok(Some(status))` when the child exited naturally within the
/// grace window, or `Ok(None)` when it had to be killed. Shares the same
/// bounded-reap shape as the persistent-bridge `reap_child` helper, but
/// returns the exit status (when available) so the caller can still
/// distinguish a clean exit from a nonzero one.
fn reap_ssh_bridge_child(
    child: &mut Child,
    grace: Duration,
) -> io::Result<Option<std::process::ExitStatus>> {
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait()? {
            Some(status) => return Ok(Some(status)),
            None if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(15));
            }
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(None);
            }
        }
    }
}

fn copy_flush<R: io::Read, W: io::Write>(reader: &mut R, writer: &mut W) -> io::Result<u64> {
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0;

    loop {
        let bytes_read = match reader.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(bytes_read) => bytes_read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };

        writer.write_all(&buffer[..bytes_read])?;
        writer.flush()?;
        total += bytes_read as u64;
    }
}

fn run_client_process(
    local_socket: &Path,
    reattach_command: &str,
    keybindings: RemoteKeybindings,
) -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let status = Command::new(exe)
        .arg("client")
        .env(
            crate::server::socket_paths::CLIENT_SOCKET_PATH_ENV_VAR,
            local_socket,
        )
        .env("HERDR_RENDER_ENCODING", "terminal-ansi")
        .env(REATTACH_COMMAND_ENV_VAR, reattach_command)
        .env(REMOTE_KEYBINDINGS_ENV_VAR, keybindings.as_str())
        .env_remove(crate::api::SOCKET_PATH_ENV_VAR)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            format!("remote client exited with {status}"),
        ))
    }
}

fn local_forward_socket_path(target: &str, session_name: &str) -> PathBuf {
    let pid = std::process::id();
    let target_clean = sanitize_path_component(target);
    let session_clean = sanitize_path_component(session_name);

    let tmpdir = std::env::temp_dir();
    let readable = tmpdir.join(format!(
        "herdr-remote-{pid}-{target_clean}-{session_clean}.sock"
    ));
    if fits_unix_socket_path(&readable) {
        return readable;
    }

    // macOS' per-user TMPDIR (~49 chars under /var/folders/...) can push the
    // readable name past sun_path's 104-byte ceiling. Fall back to a hashed
    // short name in TMPDIR, then to /tmp as a last resort when TMPDIR itself
    // is longer than the budget. The hash covers the full unsanitized
    // target/session so uniqueness does not depend on the prefix truncation;
    // the prefix is kept only for debuggability.
    let target_prefix: String = target_clean.chars().take(8).collect();
    let hash = short_socket_hash(target, session_name);
    let short_name = format!("herdr-r-{pid}-{target_prefix}-{hash}.sock");
    let short_in_tmp = tmpdir.join(&short_name);
    if fits_unix_socket_path(&short_in_tmp) {
        return short_in_tmp;
    }
    PathBuf::from("/tmp").join(short_name)
}

fn remote_client_attach_socket_path(host: &crate::remote_target::RemoteHostConfig) -> PathBuf {
    local_forward_socket_path(&host.target, &host.session)
}

fn fits_unix_socket_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    // sun_path is byte-limited: 104 bytes on macOS, 108 on Linux. Reserve
    // 1 byte for the trailing NUL and use the smaller cap for portability.
    const MAX: usize = 103;
    path.as_os_str().as_bytes().len() <= MAX
}

fn short_socket_hash(target: &str, session: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    target.hash(&mut hasher);
    0u8.hash(&mut hasher);
    session.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn sanitize_path_component(input: &str) -> String {
    let sanitized: String = input
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect();

    sanitized.trim_matches('-').chars().take(32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process_output(status: i32, stdout: &str, stderr: &str) -> Output {
        use std::os::unix::process::ExitStatusExt;

        Output {
            status: std::process::ExitStatus::from_raw(status),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn bridge_socket_is_user_only() {
        use std::os::unix::fs::PermissionsExt;

        let socket = std::env::temp_dir().join(format!(
            "herdr-bridge-permissions-test-{}.sock",
            std::process::id()
        ));
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let bridge = SshStdioBridge::start(
            "example".to_string(),
            remote_herdr,
            socket.clone(),
            "default".to_string(),
            None,
            None,
        )
        .expect("start bridge listener");

        let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, BRIDGE_SOCKET_PERMISSION_MODE);

        drop(bridge);
        let _ = std::fs::remove_file(socket);
    }

    fn bridge_test_program(body: &str, label: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!(
            "herdr-bridge-{label}-{}-{}.sh",
            std::process::id(),
            short_socket_hash(body, label)
        ));
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fake ssh");
        let mut permissions = std::fs::metadata(&path)
            .expect("fake ssh metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&path, permissions).expect("make fake ssh executable");
        path
    }

    #[test]
    fn bridge_accepts_two_live_connections_concurrently() {
        use std::io::{Read as _, Write as _};

        let socket = std::env::temp_dir().join(format!(
            "herdr-bridge-concurrent-test-{}.sock",
            std::process::id()
        ));
        let program = bridge_test_program("exec cat", "concurrent");
        let bridge = SshStdioBridge::start_with_bridge_command(
            "ignored-target".into(),
            "ignored-command".into(),
            socket.clone(),
            None,
            None,
            2,
            None,
            program.clone(),
        )
        .expect("start concurrent bridge");

        let mut first = UnixStream::connect(&socket).expect("first local stream");
        first
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("first timeout");
        first.write_all(b"one\n").expect("first write");
        let mut one = [0_u8; 4];
        first.read_exact(&mut one).expect("first echo");
        assert_eq!(&one, b"one\n");

        // Keep `first` live while admitting/round-tripping `second`. The old
        // serialized accept loop blocked here behind the first connection.
        let mut second = UnixStream::connect(&socket).expect("second local stream");
        second
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("second timeout");
        second.write_all(b"two\n").expect("second write");
        let mut two = [0_u8; 4];
        second.read_exact(&mut two).expect("second echo");
        assert_eq!(&two, b"two\n");
        assert_eq!(
            bridge
                .workers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len(),
            2
        );

        drop(first);
        drop(second);
        drop(bridge);
        let _ = std::fs::remove_file(socket);
        let _ = std::fs::remove_file(program);
    }

    #[test]
    fn bridge_shutdown_releases_tracked_workers_to_zero() {
        let socket = std::env::temp_dir().join(format!(
            "herdr-bridge-zero-workers-test-{}.sock",
            std::process::id()
        ));
        let program = bridge_test_program("exec cat", "zero-workers");
        let mut bridge = SshStdioBridge::start_with_bridge_command(
            "ignored-target".into(),
            "ignored-command".into(),
            socket.clone(),
            None,
            None,
            1,
            None,
            program.clone(),
        )
        .expect("start zero-worker bridge");
        let _stream = UnixStream::connect(&socket).expect("local stream");
        let wait_deadline = Instant::now() + Duration::from_secs(1);
        while bridge
            .workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
            && Instant::now() < wait_deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !bridge
                .workers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_empty(),
            "test did not admit a bridge worker"
        );

        bridge.shutdown_and_join();

        assert_eq!(
            bridge
                .workers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len(),
            0,
            "source switch/drop teardown must leave no tracked bridge workers"
        );
        assert!(bridge.accept_thread.is_none());
        let _ = std::fs::remove_file(socket);
        let _ = std::fs::remove_file(program);
    }

    #[test]
    fn bridge_teardown_is_bounded_when_remote_child_never_exits() {
        let socket = std::env::temp_dir().join(format!(
            "herdr-bridge-bounded-test-{}.sock",
            std::process::id()
        ));
        let program = bridge_test_program("exec sleep 30", "bounded");
        let bridge = SshStdioBridge::start_with_bridge_command(
            "ignored-target".into(),
            "ignored-command".into(),
            socket.clone(),
            None,
            None,
            1,
            None,
            program.clone(),
        )
        .expect("start bounded bridge");
        let _stream = UnixStream::connect(&socket).expect("local stream");
        let wait_deadline = Instant::now() + Duration::from_secs(1);
        while bridge
            .workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty()
            && Instant::now() < wait_deadline
        {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!bridge
            .workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_empty());

        let started = Instant::now();
        drop(bridge);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "bridge teardown exceeded bounded deadline: {:?}",
            started.elapsed()
        );
        let _ = std::fs::remove_file(socket);
        let _ = std::fs::remove_file(program);
    }

    #[test]
    fn projection_bridge_command_uses_explicit_no_start_subcommand() {
        assert_eq!(
            remote_bridge_command_for_shell_path(
                "/opt/herdr",
                "default",
                REMOTE_CLIENT_BRIDGE_NO_START_SUBCOMMAND,
            ),
            "exec /opt/herdr remote-client-bridge-no-start"
        );
    }

    #[test]
    fn bare_bridge_ssh_options_do_not_add_connect_timeout() {
        let mut command = Command::new("ssh");
        apply_bridge_ssh_options(&mut command, None, None);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(args, vec!["-T"]);
    }

    #[test]
    fn configured_host_bridge_ssh_options_add_connect_timeout() {
        let mut command = Command::new("ssh");
        apply_bridge_ssh_options(&mut command, None, Some(20));
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(args, vec!["-T", "-o", "ConnectTimeout=20"]);
    }

    #[test]
    fn bridge_ssh_options_still_apply_managed_config() {
        let managed = ManagedSshOptions {
            config_path: PathBuf::from("/tmp/herdr-bridge-test-config"),
            control_path: PathBuf::from("/tmp/herdr-bridge-test-control"),
        };
        let mut command = Command::new("ssh");
        apply_bridge_ssh_options(&mut command, Some(&managed), Some(30));
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            args,
            vec![
                "-F",
                "/tmp/herdr-bridge-test-config",
                "-S",
                "/tmp/herdr-bridge-test-control",
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPersist=yes",
                "-T",
                "-o",
                "ConnectTimeout=30",
            ]
        );
    }

    #[test]
    fn remote_bridge_socket_target_selects_client_and_api_paths() {
        let (client_path, client_description) =
            remote_bridge_socket_target(RemoteBridgeSocketKind::Client);
        assert_eq!(
            client_path,
            crate::server::socket_paths::client_socket_path()
        );
        assert_eq!(client_description, "remote Herdr client socket");

        let (api_path, api_description) = remote_bridge_socket_target(RemoteBridgeSocketKind::Api);
        assert_eq!(api_path, crate::api::socket_path());
        assert_eq!(api_description, "remote Herdr API socket");
    }

    #[test]
    fn remote_api_ping_args_accept_one_target() {
        let args = vec!["jafar".to_string()];

        assert_eq!(
            parse_remote_api_probe_target(&args, REMOTE_API_PING_SUBCOMMAND).unwrap(),
            "jafar"
        );
    }

    #[test]
    fn remote_api_ping_args_reject_missing_target() {
        let args = Vec::new();

        assert_eq!(
            parse_remote_api_probe_target(&args, REMOTE_API_PING_SUBCOMMAND).unwrap_err(),
            "expected exactly one SSH target"
        );
    }

    #[test]
    fn remote_api_ping_args_reject_extra_args() {
        let args = vec!["jafar".to_string(), "extra".to_string()];

        assert_eq!(
            parse_remote_api_probe_target(&args, REMOTE_API_PING_SUBCOMMAND).unwrap_err(),
            "expected exactly one SSH target"
        );
    }

    #[test]
    fn remote_api_ping_args_reject_dash_target() {
        let args = vec!["-oProxyCommand=x".to_string()];

        assert_eq!(
            parse_remote_api_probe_target(&args, REMOTE_API_PING_SUBCOMMAND).unwrap_err(),
            "remote-api-ping target must not start with '-'"
        );
    }

    #[test]
    fn remote_api_agent_list_args_accept_one_target() {
        let args = vec!["jafar".to_string()];

        assert_eq!(
            parse_remote_api_probe_target(&args, REMOTE_API_AGENT_LIST_SUBCOMMAND).unwrap(),
            "jafar"
        );
    }

    #[test]
    fn remote_api_agent_list_args_reject_missing_target() {
        let args = Vec::new();

        assert_eq!(
            parse_remote_api_probe_target(&args, REMOTE_API_AGENT_LIST_SUBCOMMAND).unwrap_err(),
            "expected exactly one SSH target"
        );
    }

    #[test]
    fn remote_api_agent_list_args_reject_extra_args() {
        let args = vec!["jafar".to_string(), "extra".to_string()];

        assert_eq!(
            parse_remote_api_probe_target(&args, REMOTE_API_AGENT_LIST_SUBCOMMAND).unwrap_err(),
            "expected exactly one SSH target"
        );
    }

    #[test]
    fn remote_api_agent_list_args_reject_dash_target() {
        let args = vec!["-oProxyCommand=x".to_string()];

        assert_eq!(
            parse_remote_api_probe_target(&args, REMOTE_API_AGENT_LIST_SUBCOMMAND).unwrap_err(),
            "remote-api-agent-list target must not start with '-'"
        );
    }

    #[test]
    fn remote_api_ping_request_uses_ping_method() {
        let request = remote_api_ping_request();

        assert_eq!(request.id, "remote-api-ping");
        assert!(matches!(
            request.method,
            crate::api::schema::Method::Ping(_)
        ));
    }

    #[test]
    fn remote_api_agent_list_request_uses_agent_list_method() {
        let request = remote_api_agent_list_request();

        assert_eq!(request.id, "remote-api-agent-list");
        assert!(matches!(
            request.method,
            crate::api::schema::Method::AgentList(_)
        ));
    }

    #[test]
    fn remote_api_request_writer_emits_newline_terminated_ping_request() {
        let mut buffer = Vec::new();

        write_remote_api_request(&mut buffer, &remote_api_ping_request()).unwrap();

        assert!(buffer.ends_with(b"\n"));
        let request: crate::api::schema::Request = serde_json::from_slice(&buffer).unwrap();
        assert_eq!(request.id, "remote-api-ping");
        assert!(matches!(
            request.method,
            crate::api::schema::Method::Ping(_)
        ));
    }

    #[test]
    fn remote_api_request_writer_emits_newline_terminated_agent_list_request() {
        let mut buffer = Vec::new();

        write_remote_api_request(&mut buffer, &remote_api_agent_list_request()).unwrap();

        assert!(buffer.ends_with(b"\n"));
        let request: crate::api::schema::Request = serde_json::from_slice(&buffer).unwrap();
        assert_eq!(request.id, "remote-api-agent-list");
        assert!(matches!(
            request.method,
            crate::api::schema::Method::AgentList(_)
        ));
    }

    #[test]
    fn remote_api_response_reader_trims_newline_and_crlf() {
        let mut newline = std::io::Cursor::new(b"{\"ok\":true}\n");
        assert_eq!(
            read_remote_api_response_line(&mut newline).unwrap(),
            "{\"ok\":true}"
        );

        let mut crlf = std::io::Cursor::new(b"{\"ok\":true}\r\n");
        assert_eq!(
            read_remote_api_response_line(&mut crlf).unwrap(),
            "{\"ok\":true}"
        );
    }

    #[test]
    fn remote_api_response_reader_rejects_empty_or_whitespace_only_input() {
        let mut empty = std::io::Cursor::new(Vec::<u8>::new());
        assert_eq!(
            read_remote_api_response_line(&mut empty)
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );

        let mut whitespace = std::io::Cursor::new(b"   \t\n");
        assert_eq!(
            read_remote_api_response_line(&mut whitespace)
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn remote_status_probe_reports_old_command_as_invalid_data() {
        let host = crate::remote_target::RemoteHostConfig::new("jafar", "jafar", "default", true);
        let output = process_output(256, "", "unknown command: remote-api-status");

        let err = parse_remote_api_status_output(&host, &output).unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("unknown command"));
    }

    #[test]
    fn remote_failure_classification_distinguishes_update_transport_and_unknown() {
        let invalid_data = io::Error::new(
            io::ErrorKind::InvalidData,
            "remote API ping did not advertise federation support",
        );
        let not_found = io::Error::new(
            io::ErrorKind::NotFound,
            "compatible herdr binary was not found",
        );
        let timed_out = io::Error::new(io::ErrorKind::TimedOut, "connection timed out");
        let ssh_255 = io::Error::other("remote platform detection failed: exit status: 255");
        let unknown = io::Error::other("unexpected local parse failure");

        assert_eq!(
            classify_remote_failure(&invalid_data),
            RemoteFailureClass::NeedsUpdate
        );
        assert_eq!(
            classify_remote_failure(&not_found),
            RemoteFailureClass::NeedsUpdate
        );
        assert_eq!(
            classify_remote_failure(&timed_out),
            RemoteFailureClass::Unreachable
        );
        assert_eq!(
            classify_remote_failure(&ssh_255),
            RemoteFailureClass::Unreachable
        );
        assert_eq!(
            classify_remote_failure(&unknown),
            RemoteFailureClass::Unknown
        );
    }

    #[test]
    fn remote_federation_capabilities_probe_parses_success() {
        let host = crate::remote_target::RemoteHostConfig::new("jafar", "jafar", "default", true);
        let output = process_output(
            0,
            r#"{"methods":["remote_api_bridge","agent_send","terminal_attach"]}"#,
            "",
        );

        let capabilities =
            parse_remote_federation_capabilities_probe_output(&host, &output).unwrap();

        assert!(capabilities
            .supports_method(crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE));
        assert!(
            capabilities.supports_method(crate::api::schema::FederationCapabilities::AGENT_SEND)
        );
        assert!(capabilities
            .supports_method(crate::api::schema::FederationCapabilities::TERMINAL_ATTACH));
    }

    #[test]
    fn remote_federation_capabilities_probe_reports_old_command() {
        let host = crate::remote_target::RemoteHostConfig::new("jafar", "jafar", "default", true);
        let output = process_output(256, "", "unknown command: remote-federation-capabilities");

        let err = parse_remote_federation_capabilities_probe_output(&host, &output).unwrap_err();
        let message = err.to_string();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(message.contains(
            "remote host jafar has a Herdr binary that does not advertise federation support"
        ));
        assert!(message.contains("install/update Herdr on the remote host"));
        assert!(message.contains("unknown command"));
    }

    #[test]
    fn remote_api_ping_validation_rejects_missing_federation() {
        let host = crate::remote_target::RemoteHostConfig::new("jafar", "jafar", "default", true);
        let response = serde_json::to_string(&crate::api::schema::SuccessResponse {
            id: "remote-api-ping".to_string(),
            result: crate::api::schema::ResponseResult::Pong {
                version: env!("CARGO_PKG_VERSION").to_string(),
                protocol: crate::protocol::PROTOCOL_VERSION,
                capabilities: Some(crate::api::schema::ServerCapabilities {
                    live_handoff: true,
                    detached_server_daemon: true,
                    federation: None,
                }),
            },
        })
        .unwrap();

        let err = validate_remote_api_ping_capabilities(
            &host,
            &response,
            &[crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE],
        )
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err
            .to_string()
            .contains("does not advertise federation support"));
    }

    #[test]
    fn route_method_validation_rejects_missing_method() {
        let host = crate::remote_target::RemoteHostConfig::new("jafar", "jafar", "default", true);
        let capabilities = crate::api::schema::FederationCapabilities {
            methods: vec![crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE.into()],
        };

        let err = validate_federation_capabilities(
            &host,
            &capabilities,
            &[
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::AGENT_SEND,
            ],
        )
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err
            .to_string()
            .contains("does not advertise federation method agent_send"));
    }

    #[test]
    fn federation_method_for_api_method_maps_agent_teardown() {
        // Load-bearing lock-test: AgentTeardown MUST map to AGENT_TEARDOWN. The
        // catch-all `_ => None` would otherwise only require remote_api_bridge,
        // letting an older remote silently accept a teardown it cannot perform.
        let request =
            crate::api::schema::Method::AgentTeardown(crate::api::schema::AgentTeardownParams {
                target: "jafar/codex".to_string(),
                confirm: true,
            });

        assert_eq!(
            federation_method_for_api_method(&request),
            Some(crate::api::schema::FederationCapabilities::AGENT_TEARDOWN)
        );
    }

    #[test]
    fn required_federation_methods_include_remote_agent_method() {
        let request = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::AgentSend(crate::api::schema::AgentSendParams {
                target: "jafar/codex".to_string(),
                text: "continue".to_string(),
            }),
        };

        let methods = required_federation_methods_for_request(&request);

        assert_eq!(
            methods,
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::AGENT_SEND
            ]
        );
    }

    #[test]
    fn required_federation_methods_include_remote_agent_submit_method() {
        let request = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::AgentSubmit(
                crate::api::schema::AgentSubmitParams {
                    target: "jafar/codex".to_string(),
                    text: "continue".to_string(),
                },
            ),
        };

        let methods = required_federation_methods_for_request(&request);

        assert_eq!(
            methods,
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::AGENT_SUBMIT
            ]
        );
    }

    #[test]
    fn federation_capabilities_current_advertises_agent_submit() {
        assert!(crate::api::schema::FederationCapabilities::current()
            .supports_method(crate::api::schema::FederationCapabilities::AGENT_SUBMIT));
    }

    #[test]
    fn federation_capabilities_current_advertises_agent_teardown() {
        assert!(crate::api::schema::FederationCapabilities::current()
            .supports_method(crate::api::schema::FederationCapabilities::AGENT_TEARDOWN,));
    }

    #[test]
    fn federation_method_for_api_method_maps_pane_split() {
        let request = crate::api::schema::Method::PaneSplit(crate::api::schema::PaneSplitParams {
            workspace_id: Some("remote-ws".to_string()),
            target_pane_id: Some("remote-pane".to_string()),
            direction: crate::api::schema::SplitDirection::Right,
            ratio: None,
            cwd: None,
            focus: false,
            env: Default::default(),
        });

        assert_eq!(
            federation_method_for_api_method(&request),
            Some(crate::api::schema::FederationCapabilities::PANE_SPLIT)
        );
    }

    #[test]
    fn federation_method_for_api_method_maps_pane_close() {
        let request = crate::api::schema::Method::PaneClose(crate::api::schema::PaneCloseParams {
            pane_id: "remote-pane".to_string(),
            confirm: true,
        });

        assert_eq!(
            federation_method_for_api_method(&request),
            Some(crate::api::schema::FederationCapabilities::PANE_CLOSE)
        );
    }

    #[test]
    fn required_federation_methods_include_remote_pane_split_method() {
        let request = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::PaneSplit(crate::api::schema::PaneSplitParams {
                workspace_id: Some("remote-ws".to_string()),
                target_pane_id: Some("remote-pane".to_string()),
                direction: crate::api::schema::SplitDirection::Down,
                ratio: Some(0.4),
                cwd: Some("/remote/project".to_string()),
                focus: true,
                env: Default::default(),
            }),
        };

        let methods = required_federation_methods_for_request(&request);

        assert_eq!(
            methods,
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::PANE_SPLIT
            ]
        );
    }

    #[test]
    fn federation_capabilities_current_advertises_pane_split() {
        assert!(crate::api::schema::FederationCapabilities::current()
            .supports_method(crate::api::schema::FederationCapabilities::PANE_SPLIT));
    }

    #[test]
    fn federation_method_for_api_method_maps_tab_mutations() {
        assert_eq!(
            federation_method_for_api_method(&crate::api::schema::Method::TabCreate(
                crate::api::schema::TabCreateParams {
                    workspace_id: Some("remote-ws".to_string()),
                    cwd: None,
                    focus: true,
                    label: None,
                    env: Default::default(),
                },
            )),
            Some(crate::api::schema::FederationCapabilities::TAB_CREATE)
        );
        assert_eq!(
            federation_method_for_api_method(&crate::api::schema::Method::TabFocus(
                crate::api::schema::TabTarget {
                    tab_id: "remote-tab".to_string(),
                },
            )),
            Some(crate::api::schema::FederationCapabilities::TAB_FOCUS)
        );
        assert_eq!(
            federation_method_for_api_method(&crate::api::schema::Method::TabClose(
                crate::api::schema::TabCloseParams {
                    tab_id: "remote-tab".to_string(),
                    confirm: true,
                },
            )),
            Some(crate::api::schema::FederationCapabilities::TAB_CLOSE)
        );
    }

    #[test]
    fn required_federation_methods_include_remote_pane_close_method() {
        let request = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::PaneClose(crate::api::schema::PaneCloseParams {
                pane_id: "remote-pane".to_string(),
                confirm: true,
            }),
        };

        let methods = required_federation_methods_for_request(&request);

        assert_eq!(
            methods,
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::PANE_CLOSE
            ]
        );
    }

    #[test]
    fn federation_capabilities_current_advertises_pane_close() {
        assert!(crate::api::schema::FederationCapabilities::current()
            .supports_method(crate::api::schema::FederationCapabilities::PANE_CLOSE));
    }

    #[test]
    fn validate_federation_capabilities_rejects_missing_pane_split_without_fallback() {
        let host = crate::remote_target::RemoteHostConfig::new("jafar", "jafar", "default", true);
        let capabilities = crate::api::schema::FederationCapabilities {
            methods: vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE.into(),
                crate::api::schema::FederationCapabilities::AGENT_SEND.into(),
            ],
        };

        let err = validate_federation_capabilities(
            &host,
            &capabilities,
            &[
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::PANE_SPLIT,
            ],
        )
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err
            .to_string()
            .contains("does not advertise federation method pane_split"));
    }

    #[test]
    fn validate_federation_capabilities_rejects_missing_pane_close_without_fallback() {
        let host = crate::remote_target::RemoteHostConfig::new("jafar", "jafar", "default", true);
        let capabilities = crate::api::schema::FederationCapabilities {
            methods: vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE.into(),
                crate::api::schema::FederationCapabilities::PANE_SPLIT.into(),
            ],
        };

        let err = validate_federation_capabilities(
            &host,
            &capabilities,
            &[
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::PANE_CLOSE,
            ],
        )
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err
            .to_string()
            .contains("does not advertise federation method pane_close"));
    }

    #[test]
    fn validate_federation_capabilities_rejects_missing_agent_submit_without_fallback() {
        let host = crate::remote_target::RemoteHostConfig::new("jafar", "jafar", "default", true);
        // An older remote advertises remote_api_bridge and agent_send but not
        // agent_submit. Submit must fail clearly instead of degrading to send.
        let capabilities = crate::api::schema::FederationCapabilities {
            methods: vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE.into(),
                crate::api::schema::FederationCapabilities::AGENT_SEND.into(),
            ],
        };

        let err = validate_federation_capabilities(
            &host,
            &capabilities,
            &[
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::AGENT_SUBMIT,
            ],
        )
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err
            .to_string()
            .contains("does not advertise federation method agent_submit"));
    }

    #[test]
    fn required_federation_methods_include_remote_agent_teardown_method() {
        let request = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::AgentTeardown(
                crate::api::schema::AgentTeardownParams {
                    target: "jafar/codex".to_string(),
                    confirm: true,
                },
            ),
        };

        let methods = required_federation_methods_for_request(&request);

        assert_eq!(
            methods,
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::AGENT_TEARDOWN
            ]
        );
    }

    #[test]
    fn validate_federation_capabilities_rejects_missing_agent_teardown_without_fallback() {
        let host = crate::remote_target::RemoteHostConfig::new("jafar", "jafar", "default", true);
        // An older remote advertises remote_api_bridge and agent_send but not
        // agent_teardown. Teardown must fail clearly instead of degrading into a
        // plain bridge-only request the remote cannot service.
        let capabilities = crate::api::schema::FederationCapabilities {
            methods: vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE.into(),
                crate::api::schema::FederationCapabilities::AGENT_SEND.into(),
            ],
        };

        let err = validate_federation_capabilities(
            &host,
            &capabilities,
            &[
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::AGENT_TEARDOWN,
            ],
        )
        .unwrap_err();

        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err
            .to_string()
            .contains("does not advertise federation method agent_teardown"));
    }

    #[test]
    fn required_federation_methods_include_remote_workspace_list_local_method() {
        let request = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::WorkspaceListLocal(
                crate::api::schema::EmptyParams::default(),
            ),
        };

        let methods = required_federation_methods_for_request(&request);

        assert_eq!(
            methods,
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::WORKSPACE_LIST_LOCAL
            ]
        );
    }

    #[test]
    fn required_federation_methods_include_remote_workspace_create_method() {
        let request = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::WorkspaceCreate(
                crate::api::schema::WorkspaceCreateParams {
                    cwd: None,
                    focus: true,
                    label: None,
                    env: std::collections::HashMap::new(),
                },
            ),
        };

        let methods = required_federation_methods_for_request(&request);

        assert_eq!(
            methods,
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::WORKSPACE_CREATE
            ]
        );
    }

    #[test]
    fn required_federation_methods_include_remote_tab_list_method() {
        let request = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::TabList(
                crate::api::schema::TabListParams::default(),
            ),
        };

        let methods = required_federation_methods_for_request(&request);

        assert_eq!(
            methods,
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::TAB_LIST
            ]
        );
    }

    #[test]
    fn required_federation_methods_include_remote_tab_create_focus_and_close_methods() {
        let create = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::TabCreate(crate::api::schema::TabCreateParams {
                workspace_id: Some("remote-ws".to_string()),
                cwd: Some("/remote".to_string()),
                focus: true,
                label: None,
                env: Default::default(),
            }),
        };
        assert_eq!(
            required_federation_methods_for_request(&create),
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::TAB_CREATE
            ]
        );

        let focus = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::TabFocus(crate::api::schema::TabTarget {
                tab_id: "remote-tab".to_string(),
            }),
        };
        assert_eq!(
            required_federation_methods_for_request(&focus),
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::TAB_FOCUS
            ]
        );

        let close = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::TabClose(crate::api::schema::TabCloseParams {
                tab_id: "remote-tab".to_string(),
                confirm: true,
            }),
        };
        assert_eq!(
            required_federation_methods_for_request(&close),
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::TAB_CLOSE
            ]
        );
    }

    #[test]
    fn federation_capabilities_current_advertises_tab_mutations() {
        let capabilities = crate::api::schema::FederationCapabilities::current();
        assert!(
            capabilities.supports_method(crate::api::schema::FederationCapabilities::TAB_CREATE)
        );
        assert!(capabilities.supports_method(crate::api::schema::FederationCapabilities::TAB_FOCUS));
        assert!(capabilities.supports_method(crate::api::schema::FederationCapabilities::TAB_CLOSE));
    }

    #[test]
    fn required_federation_methods_include_remote_layout_export_method() {
        let request = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::LayoutExport(
                crate::api::schema::LayoutExportParams {
                    tab_id: Some("w1:1".to_string()),
                    pane_id: None,
                },
            ),
        };

        let methods = required_federation_methods_for_request(&request);

        assert_eq!(
            methods,
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::LAYOUT_EXPORT
            ]
        );
    }

    #[test]
    fn federation_method_for_api_method_maps_rename_and_focus_methods() {
        assert_eq!(
            federation_method_for_api_method(&crate::api::schema::Method::WorkspaceRename(
                crate::api::schema::WorkspaceRenameParams {
                    workspace_id: "ws1".to_string(),
                    label: "renamed".to_string(),
                },
            )),
            Some(crate::api::schema::FederationCapabilities::WORKSPACE_RENAME)
        );
        assert_eq!(
            federation_method_for_api_method(&crate::api::schema::Method::TabRename(
                crate::api::schema::TabRenameParams {
                    tab_id: "tab1".to_string(),
                    label: "renamed".to_string(),
                },
            )),
            Some(crate::api::schema::FederationCapabilities::TAB_RENAME)
        );
        assert_eq!(
            federation_method_for_api_method(&crate::api::schema::Method::PaneRename(
                crate::api::schema::PaneRenameParams {
                    pane_id: "pane1".to_string(),
                    label: Some("renamed".to_string()),
                },
            )),
            Some(crate::api::schema::FederationCapabilities::PANE_RENAME)
        );
        assert_eq!(
            federation_method_for_api_method(&crate::api::schema::Method::PaneFocus(
                crate::api::schema::PaneTarget {
                    pane_id: "pane1".to_string(),
                },
            )),
            Some(crate::api::schema::FederationCapabilities::PANE_FOCUS)
        );
        assert_eq!(
            federation_method_for_api_method(&crate::api::schema::Method::PaneFocusDirection(
                crate::api::schema::PaneFocusDirectionParams {
                    pane_id: None,
                    direction: crate::api::schema::PaneDirection::Right,
                },
            )),
            Some(crate::api::schema::FederationCapabilities::PANE_FOCUS_DIRECTION)
        );
    }

    #[test]
    fn required_federation_methods_include_workspace_rename_method() {
        let request = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::WorkspaceRename(
                crate::api::schema::WorkspaceRenameParams {
                    workspace_id: "ws1".to_string(),
                    label: "renamed".to_string(),
                },
            ),
        };

        let methods = required_federation_methods_for_request(&request);

        assert_eq!(
            methods,
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::WORKSPACE_RENAME
            ]
        );
    }

    #[test]
    fn required_federation_methods_include_tab_rename_method() {
        let request = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::TabRename(crate::api::schema::TabRenameParams {
                tab_id: "tab1".to_string(),
                label: "renamed".to_string(),
            }),
        };

        let methods = required_federation_methods_for_request(&request);

        assert_eq!(
            methods,
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::TAB_RENAME
            ]
        );
    }

    #[test]
    fn required_federation_methods_include_pane_rename_method() {
        let request = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::PaneRename(crate::api::schema::PaneRenameParams {
                pane_id: "pane1".to_string(),
                label: None,
            }),
        };

        let methods = required_federation_methods_for_request(&request);

        assert_eq!(
            methods,
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::PANE_RENAME
            ]
        );
    }

    #[test]
    fn required_federation_methods_include_pane_focus_method() {
        let request = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::PaneFocus(crate::api::schema::PaneTarget {
                pane_id: "pane1".to_string(),
            }),
        };

        let methods = required_federation_methods_for_request(&request);

        assert_eq!(
            methods,
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::PANE_FOCUS
            ]
        );
    }

    #[test]
    fn required_federation_methods_include_pane_focus_direction_method() {
        let request = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::PaneFocusDirection(
                crate::api::schema::PaneFocusDirectionParams {
                    pane_id: None,
                    direction: crate::api::schema::PaneDirection::Down,
                },
            ),
        };

        let methods = required_federation_methods_for_request(&request);

        assert_eq!(
            methods,
            vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE,
                crate::api::schema::FederationCapabilities::PANE_FOCUS_DIRECTION
            ]
        );
    }

    #[test]
    fn federation_capabilities_current_advertises_rename_and_focus_methods() {
        let caps = crate::api::schema::FederationCapabilities::current();
        assert!(caps.supports_method(crate::api::schema::FederationCapabilities::WORKSPACE_RENAME));
        assert!(caps.supports_method(crate::api::schema::FederationCapabilities::TAB_RENAME));
        assert!(caps.supports_method(crate::api::schema::FederationCapabilities::PANE_RENAME));
        assert!(caps.supports_method(crate::api::schema::FederationCapabilities::PANE_FOCUS));
        assert!(
            caps.supports_method(crate::api::schema::FederationCapabilities::PANE_FOCUS_DIRECTION)
        );
    }

    #[test]
    fn managed_ssh_config_includes_user_config_then_fallback() {
        use std::os::unix::fs::PermissionsExt;

        let managed_config = write_managed_ssh_config().expect("write managed config");
        let path = managed_config.options.config_path.clone();
        let control_path = managed_config.options.control_path.clone();
        let contents = std::fs::read_to_string(&path).expect("read keepalive config");

        // herdr's fallback transport settings are present...
        assert!(
            contents.contains("Host *"),
            "config should add a Host * fallback block: {contents}"
        );
        assert!(
            contents.contains("ServerAliveInterval 15"),
            "config should set the keepalive interval: {contents}"
        );
        assert!(
            contents.contains("ServerAliveCountMax 4"),
            "config should set the keepalive count: {contents}"
        );
        assert!(!contents.contains("ControlMaster"));
        assert!(!contents.contains("ControlPersist"));
        assert!(!contents.contains("ControlPath"));
        // ...and any user config is Included (quoted) BEFORE it so
        // first-value-wins keeps the user's own settings.
        if let Some(home) = std::env::var_os("HOME") {
            let user_config = PathBuf::from(home).join(".ssh").join("config");
            if user_config.is_file() {
                let include = format!(
                    "Include {}",
                    ssh_config_quote(&user_config.to_string_lossy())
                );
                let include_at = contents.find(&include).expect("user config Included");
                let fallback_at = contents.find("Host *").expect("fallback present");
                assert!(
                    include_at < fallback_at,
                    "user config must be Included before herdr's fallback: {contents}"
                );
            }
        }

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, BRIDGE_SOCKET_PERMISSION_MODE,
            "keepalive config must be user-only"
        );
        // The config lives in a private 0700 dir, not a predictable temp path.
        let dir = path.parent().expect("config has a parent dir");
        let dir_mode = std::fs::metadata(dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "ssh config dir must be user-only");
        assert!(
            fits_unix_socket_path(&control_path),
            "control socket path must fit portable Unix socket limits"
        );

        drop(managed_config);
    }

    #[test]
    fn ssh_config_quote_wraps_path_with_spaces() {
        assert_eq!(
            ssh_config_quote("/home/a b/.ssh/config"),
            "\"/home/a b/.ssh/config\""
        );
    }

    #[test]
    fn remote_ssh_command_uses_managed_config_when_present() {
        let managed_config = write_managed_ssh_config().expect("write managed config");
        let config_path = managed_config.options.config_path.clone();
        let control_path = managed_config.options.control_path.clone();
        let ssh = RemoteSsh {
            target: "example".to_string(),
            managed_config: Some(managed_config),
            connect_timeout_secs: None,
        };

        let command = ssh.command();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            args,
            vec![
                "-F".to_string(),
                config_path.to_string_lossy().into_owned(),
                "-S".to_string(),
                control_path.to_string_lossy().into_owned(),
                "-o".to_string(),
                "ControlMaster=auto".to_string(),
                "-o".to_string(),
                "ControlPersist=yes".to_string(),
                "-T".to_string(),
                "example".to_string(),
            ]
        );
    }

    #[test]
    fn remote_ssh_command_is_plain_without_managed_config() {
        let ssh = RemoteSsh {
            target: "example".to_string(),
            managed_config: None,
            connect_timeout_secs: None,
        };

        let command = ssh.command();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(args, vec!["-T".to_string(), "example".to_string()]);
    }

    #[test]
    fn noninteractive_ssh_command_uses_batch_mode_and_timeouts() {
        let ssh = RemoteSsh {
            target: "jafar".to_string(),
            managed_config: None,
            connect_timeout_secs: Some(crate::remote_target::DEFAULT_CONNECT_TIMEOUT_SECS),
        };
        let command = ssh.command_with_mode(SshInvocationMode::Noninteractive);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            args,
            vec![
                "-T",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "BatchMode=yes",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=2",
                "jafar",
            ]
        );
    }

    #[test]
    fn noninteractive_ssh_command_falls_back_to_default_connect_timeout_for_bare_target() {
        let ssh = RemoteSsh {
            target: "jafar".to_string(),
            managed_config: None,
            connect_timeout_secs: None,
        };
        let command = ssh.command_with_mode(SshInvocationMode::Noninteractive);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            args,
            vec![
                "-T",
                "-o",
                "ConnectTimeout=10",
                "-o",
                "BatchMode=yes",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=2",
                "jafar",
            ]
        );
    }

    #[test]
    fn noninteractive_ssh_command_uses_custom_connect_timeout_and_keeps_static_options() {
        let ssh = RemoteSsh {
            target: "jafar".to_string(),
            managed_config: None,
            connect_timeout_secs: Some(45),
        };
        let command = ssh.command_with_mode(SshInvocationMode::Noninteractive);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            args,
            vec![
                "-T",
                "-o",
                "ConnectTimeout=45",
                "-o",
                "BatchMode=yes",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveCountMax=2",
                "jafar",
            ]
        );
    }

    #[test]
    fn interactive_ssh_command_does_not_force_batch_mode() {
        let ssh = RemoteSsh {
            target: "jafar".to_string(),
            managed_config: None,
            connect_timeout_secs: None,
        };
        let command = ssh.command_with_mode(SshInvocationMode::Interactive);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(args, vec!["-T", "jafar"]);
    }

    #[test]
    fn interactive_ssh_command_uses_custom_configured_host_connect_timeout() {
        let ssh = RemoteSsh {
            target: "jafar".to_string(),
            managed_config: None,
            connect_timeout_secs: Some(20),
        };
        let command = ssh.command_with_mode(SshInvocationMode::Interactive);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(args, vec!["-T", "-o", "ConnectTimeout=20", "jafar"]);
    }

    #[test]
    fn remote_ssh_new_for_target_has_no_connect_timeout_for_bare_target() {
        let ssh = RemoteSsh::new_for_target("jafar".to_string(), false);
        assert_eq!(ssh.connect_timeout_secs, None);
    }

    #[test]
    fn remote_ssh_new_for_host_uses_configured_connect_timeout() {
        let ssh = RemoteSsh::new_for_host("jafar".to_string(), false, 42);
        assert_eq!(ssh.connect_timeout_secs, Some(42));
    }

    #[test]
    fn remote_ssh_for_host_uses_host_connect_timeout() {
        let host = crate::remote_target::RemoteHostConfig::new("jafar", "jafar", "default", true)
            .with_connect_timeout_secs(37);
        let ssh = remote_ssh_for_host(&host);
        assert_eq!(ssh.connect_timeout_secs, Some(37));
    }

    #[test]
    fn extract_remote_args_removes_space_form() {
        let args = vec![
            "herdr".into(),
            "--remote".into(),
            "dev".into(),
            "--help".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr", "--help"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert_eq!(remote.keybindings, RemoteKeybindings::Local);
    }

    #[test]
    fn extract_remote_args_removes_equals_form() {
        let args = vec!["herdr".into(), "--remote=user@host".into()];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "user@host");
        assert_eq!(remote.keybindings, RemoteKeybindings::Local);
    }

    #[test]
    fn extract_remote_args_accepts_remote_keybindings_server() {
        let args = vec![
            "herdr".into(),
            "--remote".into(),
            "dev".into(),
            "--remote-keybindings=server".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert_eq!(remote.keybindings, RemoteKeybindings::Server);
    }

    #[test]
    fn extract_remote_args_accepts_remote_keybindings_space_form() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--remote-keybindings".into(),
            "server".into(),
        ];
        let (cleaned, remote) = extract_remote_args(&args).unwrap();
        assert_eq!(cleaned, vec!["herdr"]);
        assert_eq!(remote.unwrap().keybindings, RemoteKeybindings::Server);
    }

    #[test]
    fn extract_remote_args_accepts_explicit_handoff() {
        let args = vec!["herdr".into(), "--remote=dev".into(), "--handoff".into()];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, vec!["herdr"]);
        let remote = remote.unwrap();
        assert_eq!(remote.target, "dev");
        assert!(remote.live_handoff);
    }

    #[test]
    fn extract_remote_args_preserves_child_remote_options_after_separator() {
        let args = vec![
            "herdr".into(),
            "agent".into(),
            "start".into(),
            "repro".into(),
            "--".into(),
            "child".into(),
            "--remote".into(),
            "dev".into(),
            "--remote-keybindings=server".into(),
            "--handoff".into(),
        ];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, args);
        assert!(remote.is_none());
    }

    #[test]
    fn extract_remote_args_preserves_handoff_without_remote() {
        let args = vec!["herdr".into(), "update".into(), "--handoff".into()];

        let (cleaned, remote) = extract_remote_args(&args).unwrap();

        assert_eq!(cleaned, args);
        assert!(remote.is_none());
    }

    #[test]
    fn extract_remote_args_rejects_remote_keybindings_without_remote() {
        let args = vec!["herdr".into(), "--remote-keybindings=server".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote-keybindings requires --remote");
    }

    #[test]
    fn extract_remote_args_rejects_duplicate_remote_keybindings() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--remote-keybindings=local".into(),
            "--remote-keybindings=server".into(),
        ];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote-keybindings can only be specified once");
    }

    #[test]
    fn extract_remote_args_requires_value() {
        let args = vec!["herdr".into(), "--remote".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "missing value for --remote");
    }

    #[test]
    fn extract_remote_args_rejects_empty_value() {
        let args = vec!["herdr".into(), "--remote=".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "missing value for --remote");
    }

    #[test]
    fn extract_remote_args_rejects_duplicate_values() {
        let args = vec![
            "herdr".into(),
            "--remote=dev".into(),
            "--remote=prod".into(),
        ];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote can only be specified once");
    }

    #[test]
    fn extract_remote_args_rejects_option_like_target() {
        let args = vec!["herdr".into(), "--remote".into(), "-oProxyCommand=x".into()];
        let err = extract_remote_args(&args).unwrap_err();
        assert_eq!(err, "--remote target must not start with '-'");
    }

    #[test]
    fn sanitize_path_component_removes_shell_sensitive_chars() {
        assert_eq!(sanitize_path_component("user@host:22"), "user-host-22");
    }

    #[test]
    fn remote_platform_maps_uname_values() {
        assert_eq!(
            RemotePlatform::from_uname("Linux", "amd64")
                .unwrap()
                .asset_key(),
            "linux-x86_64"
        );
        assert_eq!(
            RemotePlatform::from_uname("Darwin", "arm64")
                .unwrap()
                .asset_key(),
            "macos-aarch64"
        );
        assert!(RemotePlatform::from_uname("FreeBSD", "x86_64").is_none());
    }

    #[test]
    fn reattach_command_includes_remote_and_session() {
        assert_eq!(
            reattach_command(
                "target/release/herdr",
                "user@host",
                "work",
                RemoteKeybindings::Local,
                false,
            ),
            "target/release/herdr --remote user@host --session work"
        );
        assert_eq!(
            reattach_command(
                "herdr",
                "host name",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Local,
                false,
            ),
            "herdr --remote 'host name'"
        );
        assert_eq!(
            reattach_command(
                "herdr",
                "host",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Server,
                false,
            ),
            "herdr --remote host --remote-keybindings server"
        );
        assert_eq!(
            reattach_command(
                "herdr",
                "host",
                crate::session::DEFAULT_SESSION_NAME,
                RemoteKeybindings::Local,
                true,
            ),
            "herdr --remote host --handoff"
        );
    }

    #[test]
    fn remote_bridge_command_uses_installed_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME),
            "exec \"$HOME/.local/bin/herdr\" remote-client-bridge"
        );
    }

    #[test]
    fn remote_bridge_command_uses_named_session() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        assert_eq!(
            remote_bridge_command(&remote_herdr, "fed-api"),
            "exec \"$HOME/.local/bin/herdr\" --session fed-api remote-client-bridge"
        );
    }

    #[test]
    fn remote_api_bridge_command_uses_api_subcommand() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        assert_eq!(
            remote_bridge_command_for(
                &remote_herdr,
                crate::session::DEFAULT_SESSION_NAME,
                REMOTE_API_BRIDGE_SUBCOMMAND,
            ),
            "exec \"$HOME/.local/bin/herdr\" remote-api-bridge"
        );
    }

    #[test]
    fn remote_api_bridge_command_quotes_named_session() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        assert_eq!(
            remote_bridge_command_for(&remote_herdr, "fed api", REMOTE_API_BRIDGE_SUBCOMMAND),
            "exec \"$HOME/.local/bin/herdr\" --session 'fed api' remote-api-bridge"
        );
    }

    #[test]
    fn remote_federation_capabilities_command_quotes_named_session() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        assert_eq!(
            remote_bridge_command_for(
                &remote_herdr,
                "fed api",
                REMOTE_FEDERATION_CAPABILITIES_SUBCOMMAND
            ),
            "exec \"$HOME/.local/bin/herdr\" --session 'fed api' remote-federation-capabilities"
        );
    }

    #[test]
    fn remote_api_bridge_command_for_host_uses_host_session() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let host = crate::remote_target::RemoteHostConfig::new(
            "jafar",
            "user@jafar:2222",
            "fed api",
            true,
        );

        assert_eq!(
            remote_api_bridge_command_for_host(&remote_herdr, &host),
            "exec \"$HOME/.local/bin/herdr\" --session 'fed api' remote-api-bridge"
        );
    }

    #[test]
    fn remote_api_bridge_command_for_host_omits_default_session() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let host = crate::remote_target::RemoteHostConfig::new(
            "jafar",
            "user@jafar:2222",
            crate::session::DEFAULT_SESSION_NAME,
            true,
        );

        assert_eq!(
            remote_api_bridge_command_for_host(&remote_herdr, &host),
            "exec \"$HOME/.local/bin/herdr\" remote-api-bridge"
        );
    }

    #[test]
    fn remote_path_discovery_uses_path_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr = remote_herdr_from_path_discovery(&remote_herdr, "/usr/bin/herdr\n")
            .expect("path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME),
            "exec /usr/bin/herdr remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_discovery_quotes_discovered_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr =
            remote_herdr_from_path_discovery(&remote_herdr, "/opt/herdr bin/herdr\n")
                .expect("path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME),
            "exec '/opt/herdr bin/herdr' remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_discovery_uses_macos_path_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "macos",
            arch: "aarch64",
        });
        let remote_herdr =
            remote_herdr_from_path_discovery(&remote_herdr, "/opt/homebrew/bin/herdr\n")
                .expect("path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME),
            "exec /opt/homebrew/bin/herdr remote-client-bridge"
        );
        assert_eq!(remote_herdr.platform.asset_key(), "macos-aarch64");
    }

    #[test]
    fn remote_path_discovery_reads_multiple_absolute_paths() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let candidates = remote_herdrs_from_path_discovery(
            &remote_herdr,
            "/usr/bin/herdr\nbin/herdr\n /opt/herdr bin/herdr\n",
        );

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].shell_path, "/usr/bin/herdr");
        assert_eq!(candidates[1].shell_path, "'/opt/herdr bin/herdr'");
    }

    #[test]
    fn remote_path_discovery_ignores_mise_shims() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let candidates = remote_herdrs_from_path_discovery(
            &remote_herdr,
            "/home/can/.local/share/mise/shims/herdr\n/home/can/.local/share/mise/installs/herdr/0.7.1/bin/herdr\n",
        );

        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].shell_path,
            "/home/can/.local/share/mise/installs/herdr/0.7.1/bin/herdr"
        );
    }

    #[test]
    fn known_remote_binary_candidate_script_includes_mise_and_nix_paths() {
        let script = known_remote_binary_candidate_script(&RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });

        assert!(script.contains("emit \"$home/.local/bin/herdr\""));
        assert!(!script.contains("mise/shims/herdr"));
        assert!(script.contains(&format!("version={}", shell_quote(&current_version()))));
        assert!(
            script.contains("emit \"$home/.local/share/mise/installs/herdr/$version/bin/herdr\"")
        );
        assert!(script.contains(
            "emit \"$home/.local/share/mise/installs/github-ogulcancelik-herdr/$version/herdr\""
        ));
        assert!(script.contains("emit \"$home/.nix-profile/bin/herdr\""));
        assert!(script.contains("emit \"/etc/profiles/per-user/$user/bin/herdr\""));
        assert!(script.contains("emit \"/run/current-system/sw/bin/herdr\""));
        assert!(script.contains("emit \"/home/linuxbrew/.linuxbrew/bin/herdr\""));
        assert!(!script.contains("emit \"/opt/homebrew/bin/herdr\""));
    }

    #[test]
    fn known_remote_binary_candidate_script_includes_macos_homebrew_paths() {
        let script = known_remote_binary_candidate_script(&RemotePlatform {
            os: "macos",
            arch: "aarch64",
        });

        assert!(script.contains("emit \"/opt/homebrew/bin/herdr\""));
        assert!(script.contains("emit \"/usr/local/bin/herdr\""));
        assert!(!script.contains("emit \"/home/linuxbrew/.linuxbrew/bin/herdr\""));
    }

    #[test]
    fn remote_path_discovery_quotes_single_quotes_in_discovered_binary() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr =
            remote_herdr_from_path_discovery(&remote_herdr, "/opt/herdr's/bin/herdr\n")
                .expect("path binary");

        assert_eq!(
            remote_bridge_command(&remote_herdr, crate::session::DEFAULT_SESSION_NAME),
            "exec '/opt/herdr'\\''s/bin/herdr' remote-client-bridge"
        );
    }

    #[test]
    fn remote_path_discovery_ignores_relative_paths() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr = remote_herdr_from_path_discovery(&remote_herdr, "bin/herdr\n");

        assert!(remote_herdr.is_none());
    }

    #[test]
    fn remote_path_discovery_ignores_empty_output() {
        let remote_herdr = RemoteHerdr::for_platform(RemotePlatform {
            os: "linux",
            arch: "x86_64",
        });
        let remote_herdr = remote_herdr_from_path_discovery(&remote_herdr, "\n");

        assert!(remote_herdr.is_none());
    }

    #[test]
    fn remote_shell_path_warning_accepts_managed_install() {
        assert!(remote_shell_resolves_managed_install(
            "/home/can/.local/bin/herdr\n"
        ));
        assert!(remote_shell_resolves_managed_install(
            "/Users/can/.local/bin/herdr\n"
        ));
        assert!(!remote_shell_resolves_managed_install(
            "/usr/local/bin/herdr\n"
        ));
        assert!(!remote_shell_resolves_managed_install(""));
    }

    #[test]
    fn parse_client_status_json_reads_protocol() {
        assert_eq!(
            parse_client_status_json(r#"{"version":"x","protocol":8,"binary":"/bin/herdr"}"#)
                .map(|status| status.protocol),
            Some(8)
        );
        assert!(parse_client_status_json(r#"{"protocol":"unknown"}"#).is_none());
    }

    #[test]
    fn parse_remote_server_status_json_reads_running_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"running","running":true,"version":"0.6.0","protocol":8,"capabilities":{"live_handoff":true,"detached_server_daemon":true}}"#
            )
            .unwrap(),
            RemoteServerStatus::Running {
                version: Some("0.6.0".into()),
                protocol: Some(8),
                live_handoff: true,
                detached_server_daemon: true
            }
        );
    }

    #[test]
    fn parse_remote_server_status_json_treats_missing_capability_as_old_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"running","running":true,"version":"0.6.0","protocol":8}"#
            )
            .unwrap(),
            RemoteServerStatus::Running {
                version: Some("0.6.0".into()),
                protocol: Some(8),
                live_handoff: false,
                detached_server_daemon: false
            }
        );
    }

    #[test]
    fn parse_remote_server_status_json_reads_stopped_server() {
        assert_eq!(
            parse_remote_server_status_json(
                r#"{"status":"not_running","running":false,"version":null,"protocol":null}"#
            )
            .unwrap(),
            RemoteServerStatus::NotRunning
        );
    }

    #[test]
    fn remote_update_manifest_uses_root_assets_for_latest_version() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.3",
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "releases": {
                    "1.2.3": {
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest
                .release_for_version("1.2.3")
                .and_then(|release| release.assets.get("linux-x86_64"))
                .map(RemoteAssetRef::url),
            Some("https://example.com/latest")
        );
    }

    #[test]
    fn remote_update_manifest_reads_archived_release_assets() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.4",
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "releases": {
                    "1.2.3": {
                        "notes": "ignored",
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest
                .release_for_version("1.2.3")
                .and_then(|release| release.assets.get("linux-x86_64"))
                .map(RemoteAssetRef::url),
            Some("https://example.com/archive")
        );
    }

    #[test]
    fn remote_update_manifest_uses_archived_release_protocol() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.4",
                "protocol": 42,
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "releases": {
                    "1.2.3": {
                        "notes": "ignored",
                        "protocol": 41,
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest
                .release_for_version("1.2.3")
                .and_then(|release| release.protocol),
            Some(41)
        );
    }

    #[test]
    fn remote_update_manifest_does_not_inherit_latest_protocol_for_archived_assets() {
        let manifest: RemoteUpdateManifest = serde_json::from_str(
            r#"{
                "version": "1.2.4",
                "protocol": 42,
                "assets": {
                    "linux-x86_64": "https://example.com/latest"
                },
                "releases": {
                    "1.2.3": {
                        "notes": "ignored",
                        "assets": {
                            "linux-x86_64": "https://example.com/archive"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest
                .release_for_version("1.2.3")
                .and_then(|release| release.protocol),
            None
        );
    }

    #[test]
    fn remote_preview_manifest_falls_back_to_archived_exact_build_assets() {
        let manifest: RemotePreviewManifest = serde_json::from_str(
            r#"{
                "build_id": "2026-06-06-new",
                "protocol": 12,
                "assets": {
                    "linux-x86_64": {
                        "url": "https://example.com/new",
                        "sha256": "new"
                    }
                },
                "builds": {
                    "2026-06-02-old": {
                        "protocol": 11,
                        "assets": {
                            "linux-x86_64": {
                                "url": "https://example.com/old",
                                "sha256": "old"
                            }
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let (protocol, assets) =
            preview_assets_for_build(&manifest, "2026-06-02-old").expect("archived build");
        let asset = assets.get("linux-x86_64").expect("asset");
        assert_eq!(protocol, 11);
        assert_eq!(asset.url(), "https://example.com/old");
        assert_eq!(asset.sha256(), Some("old"));
    }

    #[test]
    fn remote_server_restart_reason_requires_stop_for_protocol_mismatch() {
        assert_eq!(
            remote_server_restart_reason(Some(&current_version()), Some(0), true, false),
            Some(RemoteServerRestartReason::ProtocolMismatch)
        );
    }

    #[test]
    fn remote_server_restart_reason_allows_unchanged_compatible_server() {
        assert_eq!(
            remote_server_restart_reason(
                Some(&current_version()),
                Some(CURRENT_PROTOCOL),
                true,
                false
            ),
            None
        );
    }

    #[test]
    fn remote_server_restart_reason_requires_restart_for_old_daemon() {
        assert_eq!(
            remote_server_restart_reason(
                Some(&current_version()),
                Some(CURRENT_PROTOCOL),
                false,
                false
            ),
            Some(RemoteServerRestartReason::DaemonDetachMissing)
        );
    }

    #[test]
    fn remote_server_restart_reason_requires_restart_after_helper_update() {
        assert_eq!(
            remote_server_restart_reason(
                Some(&current_version()),
                Some(CURRENT_PROTOCOL),
                true,
                true
            ),
            Some(RemoteServerRestartReason::BinaryUpdated)
        );
    }

    #[test]
    fn remote_server_restart_reason_offers_restart_for_version_mismatch() {
        assert_eq!(
            remote_server_restart_reason(Some("0.0.0"), Some(CURRENT_PROTOCOL), true, false),
            Some(RemoteServerRestartReason::VersionMismatch)
        );
        assert_eq!(
            remote_server_restart_reason(None, Some(CURRENT_PROTOCOL), true, false),
            Some(RemoteServerRestartReason::VersionMismatch)
        );
    }

    #[test]
    fn remote_server_restart_reason_allows_current_server() {
        assert_eq!(
            remote_server_restart_reason(
                Some(&current_version()),
                Some(CURRENT_PROTOCOL),
                true,
                false
            ),
            None
        );
    }

    #[test]
    fn remote_install_plan_keeps_compatible_running_server() {
        assert_eq!(
            remote_install_running_server_plan(
                Some(&current_version()),
                Some(CURRENT_PROTOCOL),
                true,
                false,
                false,
                false
            ),
            RemoteInstallRunningServerPlan::KeepRunning
        );
    }

    #[test]
    fn remote_install_plan_requires_stop_for_old_daemon() {
        assert_eq!(
            remote_install_running_server_plan(
                Some(&current_version()),
                Some(CURRENT_PROTOCOL),
                false,
                true,
                false,
                false
            ),
            RemoteInstallRunningServerPlan::StopRequired(
                RemoteServerRestartReason::DaemonDetachMissing
            )
        );
    }

    #[test]
    fn remote_install_plan_requires_stop_after_helper_update() {
        assert_eq!(
            remote_install_running_server_plan(
                Some(&current_version()),
                Some(CURRENT_PROTOCOL),
                true,
                true,
                false,
                false
            ),
            RemoteInstallRunningServerPlan::StopRequired(RemoteServerRestartReason::BinaryUpdated)
        );
    }

    #[test]
    fn remote_install_plan_requires_stop_for_incompatible_running_server() {
        assert_eq!(
            remote_install_running_server_plan(
                Some("0.0.0"),
                Some(CURRENT_PROTOCOL),
                true,
                true,
                false,
                false
            ),
            RemoteInstallRunningServerPlan::StopRequired(
                RemoteServerRestartReason::VersionMismatch
            )
        );
    }

    #[test]
    fn remote_install_plan_uses_live_handoff_for_incompatible_running_server() {
        assert_eq!(
            remote_install_running_server_plan(
                Some("0.0.0"),
                Some(CURRENT_PROTOCOL),
                true,
                true,
                true,
                true
            ),
            RemoteInstallRunningServerPlan::LiveHandoff
        );
    }

    #[test]
    fn install_source_description_uses_override_binary() {
        let platform = RemotePlatform {
            os: "linux",
            arch: "aarch64",
        };
        assert_eq!(
            install_source_description_for(&platform, Some(Path::new("/tmp/herdr-aarch64")), false),
            "HERDR_REMOTE_BINARY (/tmp/herdr-aarch64)"
        );
    }

    #[test]
    fn install_source_description_uses_local_binary_when_allowed() {
        let platform = RemotePlatform::local();

        assert_eq!(
            install_source_description_for(&platform, None, true),
            "the current local herdr binary"
        );
    }

    #[test]
    fn install_source_description_uses_release_asset_when_local_binary_cannot_seed_remote() {
        let platform = RemotePlatform::local();

        assert_eq!(
            install_source_description_for(&platform, None, false),
            format!(
                "the {} {} asset for {}",
                current_version(),
                current_channel(),
                platform.asset_key()
            )
        );
    }

    #[test]
    fn resolve_install_source_uses_override_binary_without_temporary_cleanup() {
        let platform = RemotePlatform {
            os: "linux",
            arch: "aarch64",
        };
        let source = resolve_install_source(&platform, Some(PathBuf::from("/tmp/herdr-aarch64")))
            .expect("override source");
        assert_eq!(source.path, PathBuf::from("/tmp/herdr-aarch64"));
        assert!(source.temporary_dir.is_none());
    }

    fn remote_env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    fn socket_path_byte_len(path: &Path) -> usize {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().len()
    }

    #[test]
    fn local_forward_socket_path_uses_readable_name_when_it_fits() {
        let _guard = remote_env_lock().lock().unwrap();
        // Short target + session leave plenty of room — keep the human-
        // readable form so the socket path stays grep-friendly.
        let path = local_forward_socket_path("dev", "default");
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        assert!(
            filename.starts_with("herdr-remote-"),
            "expected readable name, got {filename}"
        );
        assert!(filename.contains("-dev-default."), "got {filename}");
        assert!(
            fits_unix_socket_path(&path),
            "socket path too long: {} ({} bytes)",
            path.display(),
            socket_path_byte_len(&path)
        );
    }

    #[test]
    fn local_forward_socket_path_fits_in_sun_path() {
        let _guard = remote_env_lock().lock().unwrap();
        // Worst case for the readable form: macOS-style 49-char TMPDIR +
        // max-length sanitized components. Should fall back to the hashed
        // short name, which fits under TMPDIR.
        let target = "longish-host.example.com";
        let session = "a-fairly-long-session-name-here";
        let path = local_forward_socket_path(target, session);
        assert!(
            fits_unix_socket_path(&path),
            "socket path too long for sun_path: {} ({} bytes)",
            path.display(),
            socket_path_byte_len(&path)
        );
    }

    #[test]
    fn local_forward_socket_path_falls_back_to_tmp_when_dir_is_long() {
        let _guard = remote_env_lock().lock().unwrap();
        // Force a TMPDIR long enough that even the hashed short name cannot
        // fit inside it. The fallback should drop to /tmp.
        let prior = std::env::var_os("TMPDIR");
        let long_dir = std::env::temp_dir().join("a".repeat(80));
        let _ = fs::create_dir_all(&long_dir);
        std::env::set_var("TMPDIR", &long_dir);

        let path = local_forward_socket_path("longish-host.example.com", "default");
        let fits = fits_unix_socket_path(&path);
        let parent = path.parent().map(Path::to_path_buf);
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        match prior {
            Some(v) => std::env::set_var("TMPDIR", v),
            None => std::env::remove_var("TMPDIR"),
        }
        let _ = fs::remove_dir_all(&long_dir);

        assert!(fits, "fallback path still overflows: {}", path.display());
        assert_eq!(parent.as_deref(), Some(Path::new("/tmp")));
        assert!(
            filename.starts_with("herdr-r-"),
            "expected hashed fallback, got {filename}"
        );
    }

    #[test]
    fn install_source_cleanup_removes_temporary_directory() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-install-source-cleanup-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).expect("create temp dir");
        let path = dir.join("herdr.tmp");
        fs::write(&path, b"test").expect("write temp file");

        InstallSource::temporary(path, dir.clone()).cleanup();

        assert!(!dir.exists());
    }

    #[test]
    fn prepared_state_helper_rejects_missing_required_federation_method() {
        // C5/test 1: the prepared-state helper validates cached full federation
        // capabilities locally using the same method mapping as the current
        // bridge path. A request whose required method is missing must be
        // rejected before any SSH work, so this test never spawns ssh.
        let host = crate::remote_target::RemoteHostConfig::new(
            "jafar",
            "user@jafar:2222",
            crate::session::DEFAULT_SESSION_NAME,
            true,
        );
        // Capabilities advertise remote_api_bridge but NOT agent_read.
        let state = RemoteApiBridgeState {
            shell_path: "\"$HOME/.local/bin/herdr\"".to_string(),
            capabilities: crate::api::schema::FederationCapabilities {
                methods: vec![
                    crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE.to_string(),
                ],
            },
        };
        let request = crate::api::schema::Request {
            id: "req".to_string(),
            method: crate::api::schema::Method::AgentRead(crate::api::schema::AgentReadParams {
                target: "term-1".to_string(),
                source: crate::api::schema::ReadSource::Recent,
                lines: None,
                format: crate::api::schema::ReadFormat::Text,
                strip_ansi: true,
            }),
        };

        let err = send_remote_api_request_with_prepared_state(&host, &state, &request).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("agent_read"));
        assert!(err.to_string().contains("jafar"));
    }

    #[test]
    fn prepared_state_helper_accepts_cached_required_methods_via_same_mapping() {
        // The prepared-state path reuses the exact required-method mapping of
        // the full bridge path. With full capabilities advertised, every routed
        // agent method's required methods validate locally (no SSH). This proves
        // acceptance mirrors the current bridge path without spawning ssh.
        let host = crate::remote_target::RemoteHostConfig::new(
            "jafar",
            "user@jafar:2222",
            crate::session::DEFAULT_SESSION_NAME,
            true,
        );
        let capabilities = crate::api::schema::FederationCapabilities::current();

        let methods = [
            crate::api::schema::Method::AgentRead(crate::api::schema::AgentReadParams {
                target: "t".to_string(),
                source: crate::api::schema::ReadSource::Recent,
                lines: None,
                format: crate::api::schema::ReadFormat::Text,
                strip_ansi: true,
            }),
            crate::api::schema::Method::AgentFocus(crate::api::schema::AgentTarget {
                target: "t".to_string(),
            }),
            crate::api::schema::Method::AgentStart(crate::api::schema::AgentStartParams {
                host: None,
                name: "codex".to_string(),
                cwd: None,
                workspace_id: None,
                tab_id: None,
                split: None,
                focus: false,
                new_workspace: false,
                argv: vec!["codex".to_string()],
                env: Default::default(),
            }),
        ];
        for method in methods {
            let request = crate::api::schema::Request {
                id: "req".to_string(),
                method,
            };
            let required = required_federation_methods_for_request(&request);
            validate_federation_capabilities(&host, &capabilities, &required)
                .unwrap_or_else(|err| panic!("expected local validation to pass: {err}"));
        }
    }

    #[test]
    fn remote_bridge_command_for_shell_path_matches_host_shape() {
        // The prepared-state path builds the bridge command from a cached shell
        // path string only; it must match the shape the full path produces.
        assert_eq!(
            remote_bridge_command_for_shell_path(
                "\"$HOME/.local/bin/herdr\"",
                crate::session::DEFAULT_SESSION_NAME,
                REMOTE_API_BRIDGE_SUBCOMMAND,
            ),
            "exec \"$HOME/.local/bin/herdr\" remote-api-bridge"
        );
        assert_eq!(
            remote_bridge_command_for_shell_path(
                "/usr/bin/herdr",
                "fed api",
                REMOTE_API_BRIDGE_SUBCOMMAND,
            ),
            "exec /usr/bin/herdr --session 'fed api' remote-api-bridge"
        );
    }

    // ===============================
    // Phase G.10 persistent bridge pool
    // ===============================

    use std::cell::RefCell;
    use std::sync::atomic::AtomicUsize;

    /// Unique temp directory per test so concurrent socket paths never collide.
    fn persistent_bridge_test_dir(label: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "herdr-g10-{}-{}-{}",
            label,
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    fn sample_host() -> crate::remote_target::RemoteHostConfig {
        crate::remote_target::RemoteHostConfig::new(
            "jafar",
            "user@jafar:2222",
            crate::session::DEFAULT_SESSION_NAME,
            true,
        )
    }

    fn sample_state(shell_path: &str) -> RemoteApiBridgeState {
        RemoteApiBridgeState {
            shell_path: shell_path.to_string(),
            capabilities: crate::api::schema::FederationCapabilities::current(),
        }
    }

    fn sample_read_request(id: &str) -> crate::api::schema::Request {
        crate::api::schema::Request {
            id: id.to_string(),
            method: crate::api::schema::Method::AgentRead(crate::api::schema::AgentReadParams {
                target: "term-1".to_string(),
                source: crate::api::schema::ReadSource::Recent,
                lines: None,
                format: crate::api::schema::ReadFormat::Text,
                strip_ansi: true,
            }),
        }
    }

    // ---- Fake persistent bridge connection + starter (no real SSH) ----
    // `dispatch_via_remote_bridge_pool` runs synchronously on the calling
    // thread, so a thread-local fixture is deterministic and parallel-safe:
    // each test thread owns its own fake state, and each worker thread in a
    // concurrency test owns its own. The `PersistentRemoteBridgeStarter` is a
    // bare `fn` pointer (no captured state), so per-thread behavior is wired
    // through this thread-local.
    struct FakeBridgeState {
        response: String,
        fail_write: bool,
        fail_read: bool,
        starts: usize,
        writes: usize,
    }
    impl Default for FakeBridgeState {
        fn default() -> Self {
            Self {
                response: String::from("ok"),
                fail_write: false,
                fail_read: false,
                starts: 0,
                writes: 0,
            }
        }
    }
    thread_local! {
        static FAKE_BRIDGE: RefCell<FakeBridgeState> = RefCell::new(FakeBridgeState::default());
    }
    fn reset_fake_bridge(response: &str) {
        FAKE_BRIDGE.with(|f| {
            let mut f = f.borrow_mut();
            f.response = response.to_string();
            f.fail_write = false;
            f.fail_read = false;
            f.starts = 0;
            f.writes = 0;
        });
    }
    fn set_fake_bridge_failure(fail_write: bool, fail_read: bool) {
        FAKE_BRIDGE.with(|f| {
            let mut f = f.borrow_mut();
            f.fail_write = fail_write;
            f.fail_read = fail_read;
        });
    }
    fn fake_bridge_starts() -> usize {
        FAKE_BRIDGE.with(|f| f.borrow().starts)
    }
    fn fake_bridge_writes() -> usize {
        FAKE_BRIDGE.with(|f| f.borrow().writes)
    }

    struct FakePersistentBridge;
    impl PersistentRemoteBridgeConnection for FakePersistentBridge {
        fn write_request(&mut self, request: &crate::api::schema::Request) -> io::Result<()> {
            let mut buf = Vec::new();
            write_remote_api_request(&mut buf, request)?;
            let fail = FAKE_BRIDGE.with(|f| {
                let mut f = f.borrow_mut();
                f.writes += 1;
                f.fail_write
            });
            if fail {
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "fake bridge write failure",
                ))
            } else {
                Ok(())
            }
        }
        fn read_response(&mut self) -> io::Result<String> {
            FAKE_BRIDGE.with(|f| {
                let f = f.borrow();
                if f.fail_read {
                    Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "fake bridge read EOF",
                    ))
                } else {
                    Ok(f.response.clone())
                }
            })
        }
        fn is_alive(&mut self) -> bool {
            true
        }
    }

    fn fake_bridge_starter(
        _host: &crate::remote_target::RemoteHostConfig,
        _state: &RemoteApiBridgeState,
    ) -> io::Result<Box<dyn PersistentRemoteBridgeConnection>> {
        FAKE_BRIDGE.with(|f| f.borrow_mut().starts += 1);
        Ok(Box::new(FakePersistentBridge))
    }

    fn failing_bridge_starter(
        _host: &crate::remote_target::RemoteHostConfig,
        _state: &RemoteApiBridgeState,
    ) -> io::Result<Box<dyn PersistentRemoteBridgeConnection>> {
        FAKE_BRIDGE.with(|f| f.borrow_mut().starts += 1);
        Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "fake bridge start failure",
        ))
    }

    /// Spawn a nonblocking fake Herdr API server on `socket_path`: for each
    /// accepted connection it reads one request line and writes one response
    /// line. Returns an accept counter plus a `running` flag; clear the flag and
    /// join the handle to stop the server.
    fn spawn_fake_api_server(
        socket_path: &Path,
        response: String,
    ) -> (
        Arc<AtomicUsize>,
        Arc<AtomicBool>,
        std::thread::JoinHandle<()>,
    ) {
        let listener = UnixListener::bind(socket_path).expect("bind fake api socket");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let accepts = Arc::new(AtomicUsize::new(0));
        let running = Arc::new(AtomicBool::new(true));
        let (accepts_c, running_c) = (accepts.clone(), running.clone());
        let handle = thread::spawn(move || {
            while running_c.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        accepts_c.fetch_add(1, Ordering::SeqCst);
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                        let mut line = String::new();
                        let mut reader = io::BufReader::new(&stream);
                        let _ = reader.read_line(&mut line);
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.write_all(b"\n");
                        let _ = stream.flush();
                    }
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        (accepts, running, handle)
    }

    #[test]
    fn g10_pool_max_matches_app_layer_limiter() {
        // The remote-layer pool cap must stay no greater than the app-layer
        // per-(host, session) in-flight limiter so `active + idle` never exceeds
        // the limiter that gates dispatch before the pool is consulted. The
        // cross-layer equality is pinned in `agents_deferred`'s test module
        // (it can reach both consts); here we pin the pool's own cap and that
        // the process-global pool is constructed from it.
        assert_eq!(REMOTE_AGENT_BRIDGE_POOL_MAX_PER_HOST, 4);
        assert_eq! {
            remote_agent_bridge_pool().max_per_key(),
            REMOTE_AGENT_BRIDGE_POOL_MAX_PER_HOST
        }
    }

    #[test]
    fn g10_persistent_bridge_mode_routes_on_persistent_flag() {
        // Bare `remote-api-bridge` (no flag, or any unrecognized arg) stays on
        // the one-shot path; only `--persistent` selects the long-lived loop.
        assert_eq!(remote_api_bridge_mode(&[]), RemoteApiBridgeMode::OneShot);
        assert_eq! {
            remote_api_bridge_mode(&["--persistent".to_string()]),
            RemoteApiBridgeMode::Persistent
        }
        assert_eq! {
            remote_api_bridge_mode(&["--weird".to_string()]),
            RemoteApiBridgeMode::OneShot
        }
        assert_eq! {
            remote_api_bridge_mode(&[
                "remote".to_string(),
                "--persistent".to_string()
            ]),
            RemoteApiBridgeMode::Persistent
        }
    }

    #[test]
    fn g10_persistent_bridge_command_appends_persistent_flag() {
        // The pooled bridge command is the one-shot shape plus the persistent
        // flag; nothing else about the prepared-state command shape changes.
        assert_eq! {
            remote_persistent_api_bridge_command_for_shell_path(
                "\"$HOME/.local/bin/herdr\"",
                crate::session::DEFAULT_SESSION_NAME,
            ),
            "exec \"$HOME/.local/bin/herdr\" remote-api-bridge --persistent"
        }
        assert_eq! {
            remote_persistent_api_bridge_command_for_shell_path("/usr/bin/herdr", "fed api"),
            "exec /usr/bin/herdr --session 'fed api' remote-api-bridge --persistent"
        }
    }

    #[test]
    fn g10_persistent_bridge_loop_serves_multiple_requests_with_fresh_socket_each() {
        // Drives the real persistent-loop core against a fake Herdr API socket.
        // Two request lines must produce two response lines and open TWO
        // separate API socket connections (one fresh socket per request, no
        // multiplexing), then exit cleanly on stdin EOF.
        let dir = persistent_bridge_test_dir("loop-multi");
        let socket_path = dir.join("api.sock");
        let (accepts, running, server) =
            spawn_fake_api_server(&socket_path, "{\"id\":\"req\",\"result\":{}}".to_string());

        let mut input: &[u8] =
            b"{\"id\":\"req\",\"method\":\"agent.read\"}\n{\"id\":\"req\",\"method\":\"agent.read\"}\n";
        let mut stdout = Vec::new();
        run_persistent_api_bridge_loop_io(&mut input, &mut stdout, &socket_path, "test api")
            .expect("persistent loop exits cleanly on EOF");

        running.store(false, Ordering::SeqCst);
        server.join().expect("fake api server thread");

        assert_eq!(
            accepts.load(Ordering::SeqCst),
            2,
            "one fresh socket per request"
        );
        let out = String::from_utf8(stdout).expect("utf8");
        let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2, "one response line per request");
        assert!(lines.iter().all(|l| l.contains("\"result\"")));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn g10_persistent_bridge_loop_survives_api_socket_connect_failure() {
        // A per-request API-socket connect failure (the request never reached
        // the API socket) must emit one structured `remote_request_failed`
        // response line for that request and keep the loop alive for the next
        // one, instead of killing the bridge.
        let dir = persistent_bridge_test_dir("loop-connfail");
        let missing_socket = dir.join("does-not-exist.sock");

        let mut input: &[u8] = b"{\"id\":\"req-1\",\"method\":\"agent.read\"}\n{\"id\":\"req-2\",\"method\":\"agent.read\"}\n";
        let mut stdout = Vec::new();
        run_persistent_api_bridge_loop_io(&mut input, &mut stdout, &missing_socket, "test api")
            .expect("loop survives per-request connect failure");

        let out = String::from_utf8(stdout).expect("utf8");
        let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 2, "one structured error per request");
        assert!(lines[0].contains("\"req-1\""));
        assert!(lines[0].contains("remote_request_failed"));
        assert!(lines[1].contains("\"req-2\""));
        assert!(lines[1].contains("remote_request_failed"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn g10_persistent_bridge_error_envelope_round_trips_through_api_error_schema() {
        // The bridge-authored `remote_request_failed` envelope is the only line
        // the bridge itself authors that the local dispatcher must parse (happy
        // path API responses pass through verbatim). Round-trip it through the
        // actual API schema error type AND the local response parser so request
        // correlation (id) and error mapping (code) are exact, not just
        // string-contains.
        use crate::api::client::{parse_response_value, ApiClientError};
        use crate::api::schema::ErrorResponse;

        let request_line = r#"{"id":"req-7","method":"agent.read","params":{}}"#;
        let mut buf = Vec::<u8>::new();
        write_persistent_bridge_error_response(
            &mut buf,
            request_line,
            "failed to connect to test api socket: connection refused".to_string(),
        )
        .expect("write error envelope");

        let encoded = String::from_utf8(buf).expect("envelope is utf8");
        let line = encoded.trim_end_matches('\n');

        // 1. Deserializes through the actual API schema error type with exact
        //    id, code, and message preservation.
        let parsed: ErrorResponse =
            serde_json::from_str(line).expect("envelope is a valid ErrorResponse");
        assert_eq!(parsed.id, "req-7");
        assert_eq!(parsed.error.code, "remote_request_failed");
        assert_eq! {
            parsed.error.message,
            "failed to connect to test api socket: connection refused"
        };

        // 2. The local dispatcher reader maps it to the error branch with the
        //    exact id and code preserved (not a success, not a parse failure).
        let value: serde_json::Value = serde_json::from_str(line).expect("envelope is json");
        match parse_response_value(value) {
            Err(ApiClientError::ErrorResponse(err)) => {
                assert_eq!(err.id, "req-7");
                assert_eq!(err.error.code, "remote_request_failed");
            }
            other => panic!("expected ErrorResponse mapping, got {other:?}"),
        }
    }

    #[test]
    fn g10_pool_starts_new_connection_when_no_idle_matches() {
        // Cold pool: no reusable idle entry, so the starter is invoked once, the
        // request is served, and the connection is parked for reuse.
        let pool = RemoteAgentBridgePool::new(4, PERSISTENT_BRIDGE_IDLE_TTL);
        let host = sample_host();
        let state = sample_state("\"$HOME/.local/bin/herdr\"");
        let request = sample_read_request("req-1");
        let key = crate::remote_source::RemoteHostKey::new(&host.name, &host.session);
        reset_fake_bridge("resp-1");

        let response =
            dispatch_via_remote_bridge_pool(&pool, &host, &state, &request, fake_bridge_starter)
                .expect("dispatch ok");
        assert_eq!(response.as_deref(), Some("resp-1"));
        assert_eq!(fake_bridge_starts(), 1, "cold pool starts one connection");
        assert_eq!(fake_bridge_writes(), 1);
        assert_eq!(
            pool.idle_for(&key),
            1,
            "served connection is parked for reuse"
        );
        assert_eq!(pool.active_for(&key), 0);
    }

    #[test]
    fn g10_pool_reuses_idle_bridge_before_starting_new() {
        // After one dispatch parks a connection, an identical second dispatch
        // must REUSE it: the starter is not called again and the same
        // connection writes again (pool hit, not a fresh start).
        let pool = RemoteAgentBridgePool::new(4, PERSISTENT_BRIDGE_IDLE_TTL);
        let host = sample_host();
        let state = sample_state("\"$HOME/.local/bin/herdr\"");
        let request = sample_read_request("req-1");
        let key = crate::remote_source::RemoteHostKey::new(&host.name, &host.session);
        reset_fake_bridge("resp-1");

        dispatch_via_remote_bridge_pool(&pool, &host, &state, &request, fake_bridge_starter)
            .expect("prime ok");
        assert_eq!(fake_bridge_starts(), 1);
        assert_eq!(pool.idle_for(&key), 1);

        let response =
            dispatch_via_remote_bridge_pool(&pool, &host, &state, &request, fake_bridge_starter)
                .expect("reuse ok");
        assert_eq!(response.as_deref(), Some("resp-1"));
        assert_eq!(fake_bridge_starts(), 1, "reused, not started");
        assert_eq!(fake_bridge_writes(), 2, "same connection served both");
        assert_eq!(pool.idle_for(&key), 1);
        assert_eq!(pool.active_for(&key), 0);
    }

    #[test]
    fn g10_pool_returns_ok_none_when_start_fails_before_write() {
        // Pre-write start failure: the reservation is released and the caller
        // gets Ok(None) so it may safely fall back to the one-shot prepared
        // path (still pre-write; no delivery happened).
        let pool = RemoteAgentBridgePool::new(4, PERSISTENT_BRIDGE_IDLE_TTL);
        let host = sample_host();
        let state = sample_state("\"$HOME/.local/bin/herdr\"");
        let request = sample_read_request("req-1");
        let key = crate::remote_source::RemoteHostKey::new(&host.name, &host.session);
        reset_fake_bridge("resp-1");

        let response =
            dispatch_via_remote_bridge_pool(&pool, &host, &state, &request, failing_bridge_starter)
                .expect("start failure is Ok(None), not Err");
        assert!(response.is_none(), "pre-write failure falls back");
        assert_eq!(fake_bridge_starts(), 1);
        assert_eq!(pool.active_for(&key), 0, "reservation released");
        assert_eq!(pool.idle_for(&key), 0);
    }

    #[test]
    fn g10_pool_read_failure_after_write_is_err_no_retry_no_fallback() {
        // LOAD-BEARING non-idempotent-safety rule: once a write attempt has
        // begun on a pooled bridge, ANY later failure (read EOF/IO here) maps
        // to Err, the bridge is discarded, and the caller must NOT retry or
        // fall back to the one-shot path. Uniform for every routed method.
        let pool = RemoteAgentBridgePool::new(4, PERSISTENT_BRIDGE_IDLE_TTL);
        let host = sample_host();
        let state = sample_state("\"$HOME/.local/bin/herdr\"");
        let request = sample_read_request("req-1");
        let key = crate::remote_source::RemoteHostKey::new(&host.name, &host.session);
        reset_fake_bridge("resp-1");
        set_fake_bridge_failure(false, true); // write ok, read fails

        let result =
            dispatch_via_remote_bridge_pool(&pool, &host, &state, &request, fake_bridge_starter);
        assert!(result.is_err(), "post-write failure is Err, not Ok(None)");
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(pool.idle_for(&key), 0, "discarded bridge is not parked");
        assert_eq!(pool.active_for(&key), 0, "active slot released");
    }

    #[test]
    fn g10_pool_write_failure_is_err_no_retry_no_fallback() {
        // Same boundary, reached on a broken-pipe/partial write: the request
        // may or may not have been delivered, so it is treated as
        // delivered-and-failed. Err, discard, no retry/fallback.
        let pool = RemoteAgentBridgePool::new(4, PERSISTENT_BRIDGE_IDLE_TTL);
        let host = sample_host();
        let state = sample_state("\"$HOME/.local/bin/herdr\"");
        let request = sample_read_request("req-1");
        let key = crate::remote_source::RemoteHostKey::new(&host.name, &host.session);
        reset_fake_bridge("resp-1");
        set_fake_bridge_failure(true, false);

        let result =
            dispatch_via_remote_bridge_pool(&pool, &host, &state, &request, fake_bridge_starter);
        assert!(result.is_err());
        assert_eq!(pool.idle_for(&key), 0);
        assert_eq!(pool.active_for(&key), 0);
    }

    #[test]
    fn g10_pool_returns_ok_none_when_full_without_starting() {
        // With active + idle == max, reserve_new refuses and the dispatch
        // returns Ok(None) WITHOUT calling the starter. The limiter-before-pool
        // ordering means a saturated pool falls back rather than spawning past
        // the cap.
        let pool = RemoteAgentBridgePool::new(2, PERSISTENT_BRIDGE_IDLE_TTL);
        let host = sample_host();
        let state = sample_state("\"$HOME/.local/bin/herdr\"");
        let request = sample_read_request("req-1");
        let key = crate::remote_source::RemoteHostKey::new(&host.name, &host.session);
        reset_fake_bridge("resp-1");

        assert!(pool.reserve_new(&key).is_some());
        assert!(pool.reserve_new(&key).is_some());
        assert!(pool.reserve_new(&key).is_none(), "cap saturated");

        let response =
            dispatch_via_remote_bridge_pool(&pool, &host, &state, &request, fake_bridge_starter)
                .expect("full pool is Ok(None)");
        assert!(response.is_none(), "pool full -> fall back, no spawn");
        assert_eq!(fake_bridge_starts(), 0, "starter never called");

        pool.release_active(&key);
        pool.release_active(&key);
    }

    #[test]
    fn g10_pool_requires_persistent_capability() {
        // The pool is used only when capabilities advertise the persistent
        // method (and the request's normal required methods). Without it the
        // dispatch fails locally, exactly like the one-shot prepared path, and
        // before any checkout/start.
        let pool = RemoteAgentBridgePool::new(4, PERSISTENT_BRIDGE_IDLE_TTL);
        let host = sample_host();
        let mut caps = crate::api::schema::FederationCapabilities::current();
        caps.methods.retain(|m| {
            m != crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE_PERSISTENT
        });
        let state = RemoteApiBridgeState {
            shell_path: "\"$HOME/.local/bin/herdr\"".to_string(),
            capabilities: caps,
        };
        let request = sample_read_request("req-1");
        reset_fake_bridge("resp-1");

        let result =
            dispatch_via_remote_bridge_pool(&pool, &host, &state, &request, fake_bridge_starter);
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("remote_api_bridge_persistent"));
        assert_eq!(
            fake_bridge_starts(),
            0,
            "validated before any checkout/start"
        );
    }

    #[test]
    fn g10_pool_invalidate_host_prevents_idle_reuse() {
        // A non-connected transition marks idle entries non-reusable; the next
        // dispatch must start a fresh connection instead of reusing the retired
        // one, and the retired entry is reaped at checkout.
        let pool = RemoteAgentBridgePool::new(4, PERSISTENT_BRIDGE_IDLE_TTL);
        let host = sample_host();
        let state = sample_state("\"$HOME/.local/bin/herdr\"");
        let request = sample_read_request("req-1");
        let key = crate::remote_source::RemoteHostKey::new(&host.name, &host.session);
        reset_fake_bridge("resp-1");

        dispatch_via_remote_bridge_pool(&pool, &host, &state, &request, fake_bridge_starter)
            .expect("prime ok");
        assert_eq!(pool.idle_for(&key), 1);
        assert_eq!(fake_bridge_starts(), 1);

        pool.invalidate_host(&key);

        let response =
            dispatch_via_remote_bridge_pool(&pool, &host, &state, &request, fake_bridge_starter)
                .expect("post-invalidate dispatch ok");
        assert_eq!(response.as_deref(), Some("resp-1"));
        assert_eq!(
            fake_bridge_starts(),
            2,
            "retired entry not reused; started new"
        );
        assert_eq!(
            pool.idle_for(&key),
            1,
            "new entry parked; retired one reaped"
        );
    }

    #[test]
    fn g10_pool_invalidate_host_drops_active_returned_connection() {
        // Generation/epoch invalidation: a bridge checked out (reserved +
        // started) DURING a disconnect/non-connected transition must NOT be
        // parked when it later returns via `return_connection(reusable=true)`.
        // Its captured generation is stale after `invalidate_host` advanced the
        // host generation, so it is reaped outside the pool lock instead of
        // being parked with the old identity. `active` is still released
        // exactly once. Then the next dispatch from the now-empty pool must
        // start a fresh bridge.
        let pool = RemoteAgentBridgePool::new(4, PERSISTENT_BRIDGE_IDLE_TTL);
        let host = sample_host();
        let state = sample_state("\"$HOME/.local/bin/herdr\"");
        let identity = PersistentBridgeIdentity::from_host_state(&host, &state);
        let key = crate::remote_source::RemoteHostKey::new(&host.name, &host.session);

        // Reserve an active slot, capturing the host generation at reserve time
        // (the invariant a real dispatch relies on).
        let checked_out_generation = pool.reserve_new(&key).expect("reserve ok under the cap");
        assert_eq!(pool.active_for(&key), 1);

        // Disconnect/non-connected transition advances the host generation.
        pool.invalidate_host(&key);

        // The active worker returns its connection as `reusable: true`. The stale
        // captured generation must prevent parking: the connection is reaped,
        // idle stays 0, and the active slot is released.
        let conn: Box<dyn PersistentRemoteBridgeConnection> = Box::new(FakePersistentBridge);
        pool.return_connection(
            &key,
            identity,
            conn,
            /* reusable */ true,
            checked_out_generation,
        );
        assert_eq!(
            pool.idle_for(&key),
            0,
            "stale-generation connection is not parked"
        );
        assert_eq!(
            pool.active_for(&key),
            0,
            "active slot released exactly once"
        );

        // The next dispatch from the now-empty pool must start a fresh bridge
        // (no stale child is reused), then park the fresh one normally.
        reset_fake_bridge("resp-after");
        let request = sample_read_request("req-after");
        let response =
            dispatch_via_remote_bridge_pool(&pool, &host, &state, &request, fake_bridge_starter)
                .expect("post-invalidate dispatch ok");
        assert_eq!(response.as_deref(), Some("resp-after"));
        assert_eq!(
            fake_bridge_starts(),
            1,
            "started a fresh bridge, not reused"
        );
        assert_eq!(pool.idle_for(&key), 1, "fresh connection parked normally");
        assert_eq!(pool.active_for(&key), 0);
    }

    #[test]
    fn g10_pool_drain_removes_idle_and_prevents_stale_active_park() {
        // Shutdown drain: every idle entry is reaped (removed) and every host
        // generation advances, so an active connection returning after the drain
        // is NOT parked. Covers Reviewer B's medium finding: the process-global
        // pool's idle `SshPersistentBridge` entries are explicitly drained on
        // shutdown rather than left to OS pipe-close cascade.
        let pool = RemoteAgentBridgePool::new(4, PERSISTENT_BRIDGE_IDLE_TTL);
        let host = sample_host();
        let state = sample_state("\"$HOME/.local/bin/herdr\"");
        let identity = PersistentBridgeIdentity::from_host_state(&host, &state);
        let key = crate::remote_source::RemoteHostKey::new(&host.name, &host.session);
        let request = sample_read_request("req-1");
        reset_fake_bridge("resp-1");

        // Prime an idle entry.
        dispatch_via_remote_bridge_pool(&pool, &host, &state, &request, fake_bridge_starter)
            .expect("prime ok");
        assert_eq!(pool.idle_for(&key), 1);

        // Simulate an in-flight dispatch still active across the drain.
        let checked_out_generation = pool.reserve_new(&key).expect("reserve ok");
        assert_eq!(pool.active_for(&key), 1);

        // Drain at shutdown.
        pool.drain();
        assert_eq!(pool.idle_for(&key), 0, "drain removed idle entries");
        assert_eq!(
            pool.active_for(&key),
            1,
            "in-flight active slot still held across drain"
        );

        // The in-flight connection returns after drain: stale generation must
        // prevent parking (reaped outside the lock), and active is released.
        let conn: Box<dyn PersistentRemoteBridgeConnection> = Box::new(FakePersistentBridge);
        pool.return_connection(
            &key,
            identity,
            conn,
            /* reusable */ true,
            checked_out_generation,
        );
        assert_eq!(pool.idle_for(&key), 0, "post-drain return is not parked");
        assert_eq!(pool.active_for(&key), 0, "active slot released");
    }

    #[test]
    fn g10_pool_drain_host_targets_one_host_and_leaves_others_untouched() {
        // Per-host lifecycle drain (disconnect/reconnect): `drain_host` removes
        // ONLY the named host's idle bridges under the pool lock and advances
        // ONLY that host's generation, so an active connection returning after
        // the drain is NOT parked (stale generation). Other hosts are left
        // untouched. Distinct from the mark-only `invalidate_host` (lazy) and
        // the process-wide `drain` (shutdown).
        let pool = RemoteAgentBridgePool::new(4, PERSISTENT_BRIDGE_IDLE_TTL);
        let host_a = sample_host();
        let host_b = crate::remote_target::RemoteHostConfig::new(
            "work",
            "user@work:2222",
            crate::session::DEFAULT_SESSION_NAME,
            true,
        );
        let state = sample_state("\"$HOME/.local/bin/herdr\"");
        let identity_a = PersistentBridgeIdentity::from_host_state(&host_a, &state);
        let key_a = crate::remote_source::RemoteHostKey::new(&host_a.name, &host_a.session);
        let key_b = crate::remote_source::RemoteHostKey::new(&host_b.name, &host_b.session);
        let request = sample_read_request("req-1");
        reset_fake_bridge("resp-1");

        // Prime an idle entry for each of two hosts.
        dispatch_via_remote_bridge_pool(&pool, &host_a, &state, &request, fake_bridge_starter)
            .expect("prime host_a ok");
        dispatch_via_remote_bridge_pool(&pool, &host_b, &state, &request, fake_bridge_starter)
            .expect("prime host_b ok");
        assert_eq!(pool.idle_for(&key_a), 1);
        assert_eq!(pool.idle_for(&key_b), 1);

        // An in-flight dispatch on host_a is still active across the drain.
        let checked_out_generation = pool.reserve_new(&key_a).expect("reserve ok");
        assert_eq!(pool.active_for(&key_a), 1);

        // Per-host drain of host_a only.
        let drained = pool.drain_host(&key_a);
        assert_eq!(
            drained.len(),
            1,
            "drain_host returned host_a's one idle connection"
        );
        assert_eq!(pool.idle_for(&key_a), 0, "host_a idle drained");
        assert_eq!(
            pool.idle_for(&key_b),
            1,
            "host_b idle untouched by a per-host drain"
        );
        assert_eq!(
            pool.active_for(&key_a),
            1,
            "in-flight active slot still held across the per-host drain"
        );

        // The in-flight host_a connection returns after the drain: stale
        // generation must prevent parking (reaped), and active is released.
        let conn: Box<dyn PersistentRemoteBridgeConnection> = Box::new(FakePersistentBridge);
        pool.return_connection(
            &key_a,
            identity_a,
            conn,
            /* reusable */ true,
            checked_out_generation,
        );
        assert_eq!(pool.idle_for(&key_a), 0, "post-drain return is not parked");
        assert_eq!(pool.active_for(&key_a), 0, "active slot released");
    }

    #[test]
    fn g10_pool_drain_completion_guard_complete_then_drop_emits_once() {
        // The disconnect off-loop reap guard must report completion exactly
        // once even if dropping drained connections would panic. Normal
        // `complete()` sends the completion and disarms the Drop fallback, so
        // exactly one RemoteSourcePoolDrainCompleted event is emitted.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::events::AppEvent>(4);
        let host = crate::remote_source::RemoteHostKey::new("jafar", "default");
        {
            let mut guard = PoolDrainCompletionGuard::new(host.clone(), 11, tx);
            guard.complete();
            // Idempotent: a second complete is a no-op.
            guard.complete();
        }
        match rx.blocking_recv().expect("normal completion emitted") {
            crate::events::AppEvent::RemoteSourcePoolDrainCompleted {
                host: ev_host,
                generation,
            } => {
                assert_eq!(ev_host, host);
                assert_eq!(generation, 11);
            }
            other => panic!("expected PoolDrainCompleted, got {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "no fallback after normal complete (exactly once)"
        );
    }

    #[test]
    fn g10_pool_drain_completion_guard_drop_without_complete_fires_fallback() {
        // A panic while dropping drained connections must still report
        // completion so the disconnect responder is never stranded. Dropping
        // the guard without calling complete simulates that panic path.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::events::AppEvent>(4);
        let host = crate::remote_source::RemoteHostKey::new("jafar", "default");
        {
            let _guard = PoolDrainCompletionGuard::new(host.clone(), 23, tx);
            // no complete -> drop fires fallback
        }
        match rx.blocking_recv().expect("fallback completion emitted") {
            crate::events::AppEvent::RemoteSourcePoolDrainCompleted {
                host: ev_host,
                generation,
            } => {
                assert_eq!(ev_host, host);
                assert_eq!(generation, 23);
            }
            other => panic!("expected PoolDrainCompleted, got {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "exactly one completion from the guard"
        );
    }

    #[test]
    fn g10_drain_remote_bridge_pool_is_safe_noop_on_dormant_pool() {
        // The crate-level shutdown drain is safe to call when the process-global
        // pool was never used for a real dispatch (the common case for most
        // Herdr runs): it does not panic and leaves the pool usable. nextest
        // isolates this in its own process, so the global pool is private here.
        drain_remote_bridge_pool();
        drain_remote_bridge_pool();
        // The global pool is still constructible and correctly capped.
        assert_eq! {
            remote_agent_bridge_pool().max_per_key(),
            REMOTE_AGENT_BRIDGE_POOL_MAX_PER_HOST
        }
    }

    #[test]
    fn g10_pool_stale_identity_is_not_reused() {
        // A prepared-state/config change (here: shell path) changes the pooled
        // identity; a mismatched idle entry is not reused and a fresh connection
        // is started, so a stale child is never reused across a prepared-state
        // change.
        let pool = RemoteAgentBridgePool::new(4, PERSISTENT_BRIDGE_IDLE_TTL);
        let host = sample_host();
        let request = sample_read_request("req-1");
        let key = crate::remote_source::RemoteHostKey::new(&host.name, &host.session);
        reset_fake_bridge("resp-1");

        dispatch_via_remote_bridge_pool(
            &pool,
            &host,
            &sample_state("\"/opt/herdr-a\""),
            &request,
            fake_bridge_starter,
        )
        .expect("prime ok");
        assert_eq!(pool.idle_for(&key), 1);
        assert_eq!(fake_bridge_starts(), 1);

        dispatch_via_remote_bridge_pool(
            &pool,
            &host,
            &sample_state("\"/opt/herdr-b\""),
            &request,
            fake_bridge_starter,
        )
        .expect("identity-mismatched dispatch ok");
        assert_eq!(
            fake_bridge_starts(),
            2,
            "mismatched identity -> started new"
        );
        // The stale identity-mismatched idle entry is pruned at checkout rather
        // than left to fill the pool until TTL; only the just-parked entry for
        // the new identity remains.
        assert_eq!(
            pool.idle_for(&key),
            1,
            "stale mismatched idle entry pruned, not retained"
        );
    }

    #[test]
    fn g10_pool_idle_ttl_expiry_prunes_at_checkout() {
        // An idle entry older than the TTL is pruned at checkout rather than
        // reused; the next dispatch starts a fresh connection.
        let pool = RemoteAgentBridgePool::new(4, Duration::from_millis(1));
        let host = sample_host();
        let state = sample_state("\"$HOME/.local/bin/herdr\"");
        let request = sample_read_request("req-1");
        reset_fake_bridge("resp-1");

        dispatch_via_remote_bridge_pool(&pool, &host, &state, &request, fake_bridge_starter)
            .expect("prime ok");
        let key = crate::remote_source::RemoteHostKey::new(&host.name, &host.session);
        assert_eq!(pool.idle_for(&key), 1);

        std::thread::sleep(Duration::from_millis(8));

        dispatch_via_remote_bridge_pool(&pool, &host, &state, &request, fake_bridge_starter)
            .expect("post-ttl dispatch ok");
        assert_eq!(fake_bridge_starts(), 2, "expired entry pruned, not reused");
    }

    #[test]
    fn g10_pool_serves_concurrent_dispatches_without_corruption() {
        // The pool is shared across worker threads behind a Mutex. Concurrent
        // dispatches must all complete with their own correct response and leave
        // the pool clean (active 0, idle <= max). One-active-per-bridge is
        // structurally guaranteed: checkout MOVES the Box<dyn connection> out
        // under the Mutex, so two workers can never hold the same connection.
        // This test pins thread-safety under real contention.
        use std::sync::Barrier;

        let max = 4usize;
        let workers = 4usize;
        let per_worker = 3usize;
        let pool = Arc::new(RemoteAgentBridgePool::new(max, PERSISTENT_BRIDGE_IDLE_TTL));
        let host = Arc::new(sample_host());
        let state = Arc::new(sample_state("\"$HOME/.local/bin/herdr\""));
        let start = Arc::new(Barrier::new(workers));

        let oks: Vec<usize> = thread::scope(|s| {
            let handles: Vec<_> = (0..workers)
                .map(|w| {
                    let pool = pool.clone();
                    let host = host.clone();
                    let state = state.clone();
                    let start = start.clone();
                    s.spawn(move || -> usize {
                        let worker_response = format!("resp-{w}");
                        // Each worker thread owns its OWN thread-local fake.
                        reset_fake_bridge(&worker_response);
                        start.wait();
                        let mut ok = 0usize;
                        for i in 0..per_worker {
                            let request = sample_read_request(&format!("{worker_response}-{i}"));
                            let r = dispatch_via_remote_bridge_pool(
                                &pool,
                                &host,
                                &state,
                                &request,
                                fake_bridge_starter,
                            )
                            .expect("concurrent dispatch ok");
                            if r.as_deref() == Some(worker_response.as_str()) {
                                ok += 1;
                            }
                        }
                        ok
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().expect("worker"))
                .collect()
        });

        let key = crate::remote_source::RemoteHostKey::new(&host.name, &host.session);
        assert_eq! {
            oks.iter().sum::<usize>(),
            workers * per_worker,
            "every dispatch returned the worker's own response"
        }
        assert_eq!(pool.active_for(&key), 0, "all active slots returned");
        assert!(pool.idle_for(&key) <= max, "idle within cap");
    }

    #[test]
    fn g10_reap_child_returns_quickly_for_exited_process() {
        // A child that has already exited is reaped immediately on the fast
        // path (first try_wait succeeds), which is the common teardown case
        // for a persistent loop that exits on stdin EOF.
        let mut child = Command::new("true").spawn().expect("spawn true");
        std::thread::sleep(Duration::from_millis(20)); // let it exit
        let started = Instant::now();
        reap_child(&mut child).expect("reap exited child");
        assert! {
            started.elapsed() < Duration::from_millis(500),
            "exited child reaped on the fast path, not the grace window"
        }
    }

    #[test]
    fn g10_reap_child_kills_a_stuck_child() {
        // A child that refuses to exit within the grace window is force-killed
        // and reaped so teardown cannot leak a process (a wedged remote loop).
        let mut child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep 30");
        let started = Instant::now();
        reap_child(&mut child).expect("reap kills stuck child");
        let elapsed = started.elapsed();
        assert! {
            elapsed >= PERSISTENT_BRIDGE_REAP_GRACE,
            "waited the grace window before killing: {elapsed:?}"
        }
        assert! {
            elapsed < PERSISTENT_BRIDGE_REAP_GRACE + Duration::from_secs(3),
            "killed promptly after the grace window: {elapsed:?}"
        }
        match child.try_wait() {
            Ok(Some(_)) => {}
            other => panic!("expected a reaped child, got {other:?}"),
        }
    }

    /// FIX-4 (diff review round 4, high 4 — round-3 finding 6 only partially
    /// resolved): a `ReapOnDrop`-wrapped child that is dropped WITHOUT ever
    /// having `.wait()` called on it (exactly what happens on
    /// `send_remote_api_request_with_mode`'s early `?` return sites for
    /// stdin/stdout acquisition and write failures) must still be reaped —
    /// no leaked live process, no zombie left behind. Verified via
    /// `/proc/<pid>`: a leaked-but-still-running child keeps its `/proc`
    /// entry, and so does a leaked ZOMBIE (killed/exited but never
    /// `wait()`ed, State `Z`) — only a genuinely reaped child's entry is
    /// removed by the kernel, so this test catches both failure modes a bare
    /// `kill(pid, 0)` liveness check would miss (`kill(pid, 0)` returns
    /// success for a zombie too, so it cannot distinguish "still running"
    /// from "killed but never reaped"). Linux-only (diff review round 5,
    /// blocker 2): `ReapOnDrop`/`reap_child` themselves are genuinely
    /// cross-platform Unix code (exercised on macOS in production, and
    /// indirectly by `g10_reap_child_*` above, which use the portable
    /// `try_wait()`/`wait()` API), but `/proc` itself does not exist on
    /// macOS — gated here rather than replacing the check with a weaker
    /// portable one that could not tell "leaked" from "reaped".
    #[cfg(target_os = "linux")]
    #[test]
    fn reap_on_drop_reaps_a_real_child_dropped_before_an_explicit_wait() {
        let child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn real child");
        let pid = child.id();
        assert!(
            std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "sanity: the child must be alive before the guard drops"
        );

        {
            // Constructed exactly like `send_remote_api_request_with_mode`'s
            // early exit paths: the guard is dropped here, at the end of
            // this scope, without ever calling `.wait()` on it directly.
            let _guard = ReapOnDrop(child);
        }

        let deadline = Instant::now() + PERSISTENT_BRIDGE_REAP_GRACE + Duration::from_secs(3);
        loop {
            if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "ReapOnDrop must reap the child on an early-return exit path (pid {pid} leaked or zombied)"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}
