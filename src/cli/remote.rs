use std::io;
use std::path::Path;

use crate::remote_target::{
    RemoteConnectionPolicy, RemoteHostConfig, RemoteHostRegistry, DEFAULT_CONNECT_TIMEOUT_SECS,
};
use crate::session::DEFAULT_SESSION_NAME;

pub(super) fn run_remote_command(args: &[String]) -> io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_remote_help();
        return Ok(2);
    };

    match subcommand {
        "list" => remote_list(&args[1..]),
        "status" => remote_status(&args[1..]),
        "check" => remote_check(&args[1..]),
        "add" => remote_add(&args[1..]),
        "remove" => remote_remove(&args[1..]),
        "help" | "--help" | "-h" => {
            print_remote_help();
            Ok(0)
        }
        _ => {
            print_remote_help();
            Ok(2)
        }
    }
}

fn remote_list(args: &[String]) -> io::Result<i32> {
    let hosts = match configured_remote_hosts(args, "herdr remote list [HOST]")? {
        RemoteHostSelection::Hosts(hosts) => hosts,
        RemoteHostSelection::Exit(code) => return Ok(code),
    };

    println!(
        "{:<20} {:<18} {:<14} {:<10} timeout",
        "host", "target", "session", "policy"
    );
    for host in &hosts {
        print_list_row(&remote_list_row(host));
    }
    Ok(0)
}

fn remote_status(args: &[String]) -> io::Result<i32> {
    let hosts = match configured_remote_hosts(args, "herdr remote status [HOST]")? {
        RemoteHostSelection::Hosts(hosts) => hosts,
        RemoteHostSelection::Exit(code) => return Ok(code),
    };

    println!("{:<20} {:<18} {:<14} details", "host", "status", "session");
    for host in hosts {
        let status = probe_remote_status(&host);
        println!(
            "{:<20} {:<18} {:<14} {}",
            host.name,
            status.kind.label(),
            host.session,
            status.detail
        );
    }
    Ok(0)
}

fn remote_check(args: &[String]) -> io::Result<i32> {
    let hosts = match configured_remote_hosts(args, "herdr remote check [HOST]")? {
        RemoteHostSelection::Hosts(hosts) => hosts,
        RemoteHostSelection::Exit(code) => return Ok(code),
    };

    println!("{:<20} {:<12} {:<14} details", "host", "check", "status");
    for host in hosts {
        print_remote_check(&host);
    }
    Ok(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteHostSelection {
    Hosts(Vec<crate::remote_target::RemoteHostConfig>),
    Exit(i32),
}

fn configured_remote_hosts(args: &[String], usage: &str) -> io::Result<RemoteHostSelection> {
    let host_filter = match parse_remote_host_filter(args) {
        Ok(host_filter) => host_filter,
        Err(()) => {
            eprintln!("usage: {usage}");
            return Ok(RemoteHostSelection::Exit(2));
        }
    };

    let loaded = crate::config::Config::load();
    let remote = &loaded.config.remote;
    if !remote.enabled {
        println!("remote federation is disabled (set remote.enabled = true to configure remotes).");
        return Ok(RemoteHostSelection::Exit(0));
    }

    let registry = match remote_status_registry(&remote.hosts) {
        Ok(registry) => registry,
        Err(err) => {
            eprintln!("invalid remote host config: {err}");
            return Ok(RemoteHostSelection::Exit(1));
        }
    };
    let hosts = match remote_status_hosts(&registry, host_filter) {
        Ok(hosts) => hosts.into_iter().cloned().collect::<Vec<_>>(),
        Err(RemoteStatusHostError::UnknownHost(host)) => {
            eprintln!("unknown remote host: {host}");
            return Ok(RemoteHostSelection::Exit(1));
        }
    };
    if hosts.is_empty() {
        println!("remote federation is enabled, but no remote hosts are configured.");
        return Ok(RemoteHostSelection::Exit(0));
    }
    Ok(RemoteHostSelection::Hosts(hosts))
}

fn print_remote_check(host: &crate::remote_target::RemoteHostConfig) {
    let prepared = match crate::remote::prepare_remote_binary_to_host_noninteractive(host) {
        Ok(remote_herdr) => {
            print_check_row(
                host,
                &remote_check_binary_row(Ok(remote_herdr.shell_path().to_string())),
            );
            remote_herdr
        }
        Err(err) => {
            print_check_row(host, &remote_check_binary_row(Err(err)));
            return;
        }
    };

    let federation = remote_check_federation_row(
        crate::remote::remote_federation_capabilities_for_prepared_host_noninteractive(
            host, &prepared,
        ),
    );
    print_check_row(host, &federation);
    if !federation.should_continue() {
        return;
    }

    let api = remote_check_api_row(
        host,
        crate::remote::remote_api_status_for_prepared_host_noninteractive(host, &prepared),
    );
    print_check_row(host, &api);
}

/// Inventory row for `remote list`. Built from a configured host without
/// probing: it surfaces the alias, SSH target, remote session, connection
/// policy (via [`RemoteConnectionPolicy::as_toml_str`]), and the SSH connect
/// timeout. It opens no bridge and touches no remote host.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteListRow {
    host: String,
    target: String,
    session: String,
    policy: &'static str,
    timeout: String,
}

fn remote_list_row(host: &crate::remote_target::RemoteHostConfig) -> RemoteListRow {
    RemoteListRow {
        host: host.name.clone(),
        target: host.target.clone(),
        session: host.session.clone(),
        policy: host.connection_policy.as_toml_str(),
        timeout: format!("{}s", host.connect_timeout_secs),
    }
}

fn print_list_row(row: &RemoteListRow) {
    println!(
        "{:<20} {:<18} {:<14} {:<10} {}",
        row.host, row.target, row.session, row.policy, row.timeout
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteCheckRow {
    check: &'static str,
    status: &'static str,
    detail: String,
}

impl RemoteCheckRow {
    fn should_continue(&self) -> bool {
        self.status == "ok"
    }
}

fn remote_check_binary_row(result: io::Result<String>) -> RemoteCheckRow {
    match result {
        Ok(path) => RemoteCheckRow {
            check: "ssh/binary",
            status: "ok",
            detail: format!("compatible Herdr binary at {path}"),
        },
        Err(err) => remote_check_error_row("ssh/binary", &err),
    }
}

fn remote_check_federation_row(
    result: io::Result<crate::api::schema::FederationCapabilities>,
) -> RemoteCheckRow {
    match result {
        Ok(capabilities) => RemoteCheckRow {
            check: "federation",
            status: "ok",
            detail: format!("methods: {}", capabilities.methods.join(",")),
        },
        Err(err) => remote_check_error_row("federation", &err),
    }
}

fn remote_check_api_row(
    host: &crate::remote_target::RemoteHostConfig,
    result: io::Result<crate::remote::RemoteApiStatusResponse>,
) -> RemoteCheckRow {
    match result {
        Ok(status) => {
            let classified = classify_remote_api_status(host, &status);
            RemoteCheckRow {
                check: "api",
                status: classified.kind.label(),
                detail: classified.detail,
            }
        }
        Err(err) => remote_check_error_row("api", &err),
    }
}

fn remote_check_error_row(check: &'static str, err: &io::Error) -> RemoteCheckRow {
    let status = classify_remote_status_error(err);
    RemoteCheckRow {
        check,
        status: status.kind.label(),
        detail: status.detail,
    }
}

fn print_check_row(host: &crate::remote_target::RemoteHostConfig, row: &RemoteCheckRow) {
    println!(
        "{:<20} {:<12} {:<14} {}",
        host.name, row.check, row.status, row.detail
    );
}

fn remote_status_registry(
    hosts: &[crate::remote_target::RemoteHostConfig],
) -> Result<crate::remote_target::RemoteHostRegistry, crate::remote_target::RemoteHostConfigError> {
    crate::remote_target::RemoteHostRegistry::from_configs(hosts.to_vec())
}

fn parse_remote_host_filter(args: &[String]) -> Result<Option<&str>, ()> {
    match args {
        [] => Ok(None),
        [host] => Ok(Some(host.as_str())),
        _ => Err(()),
    }
}

fn remote_status_hosts<'a>(
    registry: &'a crate::remote_target::RemoteHostRegistry,
    host_filter: Option<&str>,
) -> Result<Vec<&'a crate::remote_target::RemoteHostConfig>, RemoteStatusHostError> {
    match host_filter {
        Some(host) => registry
            .get(host)
            .map(|host| vec![host])
            .ok_or_else(|| RemoteStatusHostError::UnknownHost(host.to_string())),
        None => Ok(registry.list()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteStatusHostError {
    UnknownHost(String),
}

fn probe_remote_status(host: &crate::remote_target::RemoteHostConfig) -> RemoteStatus {
    match crate::remote::remote_api_status_to_host_noninteractive(host) {
        Ok(status) => classify_remote_api_status(host, &status),
        Err(err) => classify_remote_status_error(&err),
    }
}

fn classify_remote_api_status(
    host: &crate::remote_target::RemoteHostConfig,
    status: &crate::remote::RemoteApiStatusResponse,
) -> RemoteStatus {
    match status.state {
        crate::remote::RemoteApiStatusState::NotRunning => RemoteStatus {
            kind: RemoteStatusKind::NotRunning,
            detail: format!(
                "remote Herdr API is not running for the configured session; {}",
                remote_start_guidance(host)
            ),
        },
        crate::remote::RemoteApiStatusState::Running => classify_running_remote_api_status(status),
    }
}

fn classify_running_remote_api_status(
    status: &crate::remote::RemoteApiStatusResponse,
) -> RemoteStatus {
    if status.protocol != Some(crate::protocol::PROTOCOL_VERSION) {
        return RemoteStatus {
            kind: RemoteStatusKind::Incompatible,
            detail: with_update_guidance(format!(
                "running server protocol {}; local protocol is {}",
                protocol_label(status.protocol),
                crate::protocol::PROTOCOL_VERSION
            )),
        };
    }

    let Some(federation) = status
        .capabilities
        .as_ref()
        .and_then(|capabilities| capabilities.federation.as_ref())
    else {
        return RemoteStatus {
            kind: RemoteStatusKind::Incompatible,
            detail: with_update_guidance(
                "running server does not advertise federation support".to_string(),
            ),
        };
    };
    if !federation.supports_method(crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE) {
        return RemoteStatus {
            kind: RemoteStatusKind::Incompatible,
            detail: with_update_guidance(
                "running server does not advertise federation method remote_api_bridge".to_string(),
            ),
        };
    }

    RemoteStatus {
        kind: RemoteStatusKind::Connected,
        detail: format!(
            "running v{} protocol {}; federation advertised",
            status.version.as_deref().unwrap_or("unknown"),
            protocol_label(status.protocol)
        ),
    }
}

fn protocol_label(protocol: Option<u32>) -> String {
    protocol
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn remote_start_guidance(host: &crate::remote_target::RemoteHostConfig) -> String {
    format!(
        "run herdr --remote {} --session {} interactively to start it",
        shell_quote(&host.target),
        shell_quote(&host.session)
    )
}

fn shell_quote(value: &str) -> String {
    if value.is_empty()
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '\'' | '"' | '\\' | '$' | '`' | '!' | '|'))
    {
        format!("'{}'", value.replace('\'', "'\\''"))
    } else {
        value.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteStatus {
    kind: RemoteStatusKind,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemoteStatusKind {
    Connected,
    NotRunning,
    Incompatible,
    Unreachable,
    Unknown,
}

impl RemoteStatusKind {
    fn label(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::NotRunning => "not running",
            Self::Incompatible => "needs update",
            Self::Unreachable => "unreachable",
            Self::Unknown => "error",
        }
    }
}

fn classify_remote_status_error(err: &io::Error) -> RemoteStatus {
    let message = normalize_status_detail(&err.to_string());
    match crate::remote::classify_remote_failure(err) {
        crate::remote::RemoteFailureClass::NeedsUpdate => RemoteStatus {
            kind: RemoteStatusKind::Incompatible,
            detail: with_update_guidance(message.clone()),
        },
        crate::remote::RemoteFailureClass::Unreachable => RemoteStatus {
            kind: RemoteStatusKind::Unreachable,
            detail: with_ssh_guidance(&message),
        },
        crate::remote::RemoteFailureClass::Unknown => RemoteStatus {
            kind: RemoteStatusKind::Unknown,
            detail: message,
        },
    }
}

fn normalize_status_detail(detail: &str) -> String {
    detail.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn with_update_guidance(detail: String) -> String {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("install/update herdr on the remote host") {
        detail
    } else {
        format!("{detail}; install/update Herdr on the remote host")
    }
}

fn with_ssh_guidance(detail: &str) -> String {
    if detail.to_ascii_lowercase().starts_with("check ssh access:") {
        detail.to_string()
    } else {
        format!("check SSH access: {detail}")
    }
}

const REMOTE_ADD_USAGE: &str =
    "usage: herdr remote add <alias> --target <ssh-target> [--session <session>] [--connection-policy auto|on_demand|manual] [--connect-timeout-secs N]";
const REMOTE_REMOVE_USAGE: &str = "usage: herdr remote remove <alias> --confirm";

fn remote_add(args: &[String]) -> io::Result<i32> {
    if is_help_request(args) {
        eprintln!("{REMOTE_ADD_USAGE}");
        eprintln!(
            "  writes a [[remote.hosts]] entry to local config only; opens no SSH bridge and probes nothing"
        );
        eprintln!(
            "  run `herdr server reload-config` afterwards if a local server should apply it"
        );
        return Ok(0);
    }

    let params = match parse_remote_add_args(args) {
        Ok(params) => params,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("{REMOTE_ADD_USAGE}");
            return Ok(2);
        }
    };

    // Resolve and validate parsed inputs before touching config.
    let policy = match params.connection_policy.as_deref() {
        None => RemoteConnectionPolicy::Auto,
        Some(value) => match parse_connection_policy(value) {
            Ok(policy) => policy,
            Err(message) => {
                eprintln!("{message}");
                eprintln!("{REMOTE_ADD_USAGE}");
                return Ok(2);
            }
        },
    };
    let connect_timeout_secs = match params.connect_timeout_secs.as_deref() {
        None => DEFAULT_CONNECT_TIMEOUT_SECS,
        Some(value) => match value.parse::<u32>() {
            Ok(number) => number,
            Err(_) => {
                eprintln!(
                    "invalid connect-timeout-secs '{value}' (expected a whole number of seconds)"
                );
                eprintln!("{REMOTE_ADD_USAGE}");
                return Ok(2);
            }
        },
    };
    let session = params
        .session
        .unwrap_or_else(|| DEFAULT_SESSION_NAME.to_string());

    let new_host = RemoteHostConfig::from_explicit_fields(
        params.alias.clone(),
        params.target.clone(),
        session.clone(),
        policy,
        connect_timeout_secs,
    );

    let path = crate::config::config_path();
    let (content, existing_hosts) = match load_remote_config_source(&path) {
        Ok(source) => source,
        Err(message) => {
            eprintln!("{message}");
            return Ok(1);
        }
    };

    // Validate the combined set (existing + new) before writing. This catches
    // duplicate aliases, invalid aliases, empty/leading-dash targets, empty
    // sessions, and out-of-range timeouts via the existing registry rules.
    let mut combined = existing_hosts;
    combined.push(new_host.clone());
    if let Err(err) = RemoteHostRegistry::from_configs(combined) {
        eprintln!("cannot add remote host: {err}");
        eprintln!("config at {} is unchanged.", path.display());
        return Ok(1);
    }

    // Mutate content line-preserving: enable federation, then append the host
    // block. Re-validate the resulting text so a round-trip through TOML plus
    // the combined registry must succeed before the file is touched.
    let enabled = crate::config::ensure_remote_enabled(&content);
    let block = format_remote_host_block(&new_host);
    let updated = crate::config::append_remote_host_block(&enabled, &block);
    if let Err(message) = validate_remote_add(&updated) {
        eprintln!("{message}");
        eprintln!("config at {} is unchanged.", path.display());
        return Ok(1);
    }

    if let Err(err) = write_remote_config(&path, &updated) {
        eprintln!("failed to write config: {err}");
        return Ok(1);
    }

    println!(
        "Added remote host {} (target {}, session {}) to {}.",
        new_host.name,
        new_host.target,
        new_host.session,
        path.display()
    );
    println!("Remote federation is enabled ([remote] enabled = true).");
    println!("If a Herdr server is running, run `herdr server reload-config` to apply this now.");
    println!("This command edited local config only; it did not SSH, probe, install, or start anything on the remote host.");
    Ok(0)
}

fn remote_remove(args: &[String]) -> io::Result<i32> {
    if is_help_request(args) {
        eprintln!("{REMOTE_REMOVE_USAGE}");
        eprintln!(
            "  removes the [[remote.hosts]] entry from local config only; --confirm is required"
        );
        eprintln!(
            "  does not stop the remote Herdr server, kill processes, close panes, or delete remote state"
        );
        return Ok(0);
    }

    let (alias, confirm) = match parse_remote_remove_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("{REMOTE_REMOVE_USAGE}");
            return Ok(2);
        }
    };
    if !confirm {
        eprintln!("remote remove is destructive; pass --confirm to proceed");
        eprintln!("{REMOTE_REMOVE_USAGE}");
        return Ok(2);
    }

    let path = crate::config::config_path();
    let (content, existing_hosts) = match load_remote_config_source(&path) {
        Ok(source) => source,
        Err(message) => {
            eprintln!("{message}");
            return Ok(1);
        }
    };

    // Reject unknown alias and invalid/unsafe config before writing.
    let registry = match RemoteHostRegistry::from_configs(existing_hosts) {
        Ok(registry) => registry,
        Err(err) => {
            eprintln!("invalid remote host config: {err}");
            eprintln!("config at {} is unchanged.", path.display());
            return Ok(1);
        }
    };
    if registry.get(&alias).is_none() {
        eprintln!("unknown remote host: {alias}");
        eprintln!("config at {} is unchanged.", path.display());
        return Ok(1);
    }

    let updated = match crate::config::remove_remote_host_block(&content, &alias) {
        Ok(Some(updated)) => updated,
        Ok(None) => {
            eprintln!("unknown remote host: {alias}");
            eprintln!("config at {} is unchanged.", path.display());
            return Ok(1);
        }
        Err(message) => {
            eprintln!(
                "could not safely remove remote host {alias} from {}; edit the [[remote.hosts]] block manually.",
                path.display()
            );
            eprintln!("{message}");
            eprintln!("config at {} is unchanged.", path.display());
            return Ok(1);
        }
    };
    if let Err(message) = validate_remote_config_text(&updated) {
        eprintln!("{message}");
        eprintln!("config at {} is unchanged.", path.display());
        return Ok(1);
    }

    if let Err(err) = write_remote_config(&path, &updated) {
        eprintln!("failed to write config: {err}");
        return Ok(1);
    }

    println!("Removed remote host {} from {}.", alias, path.display());
    println!("If a Herdr server is running, run `herdr server reload-config` so it drops local aggregation/bridge state for this host.");
    println!("This command edited local config only; it did not stop the remote Herdr server, kill processes, close panes, or delete remote state.");
    Ok(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteAddParams {
    alias: String,
    target: String,
    session: Option<String>,
    connection_policy: Option<String>,
    connect_timeout_secs: Option<String>,
}

fn parse_remote_add_args(args: &[String]) -> Result<RemoteAddParams, String> {
    let mut alias = None;
    let mut target = None;
    let mut session = None;
    let mut connection_policy = None;
    let mut connect_timeout_secs = None;

    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let (flag, inline) = split_inline_value(arg);
        match flag {
            "--target" => {
                let value = take_flag_value(args, &mut index, inline, "--target")?;
                set_once(&mut target, "--target", value)?;
            }
            "--session" => {
                let value = take_flag_value(args, &mut index, inline, "--session")?;
                set_once(&mut session, "--session", value)?;
            }
            "--connection-policy" => {
                let value = take_flag_value(args, &mut index, inline, "--connection-policy")?;
                set_once(&mut connection_policy, "--connection-policy", value)?;
            }
            "--connect-timeout-secs" => {
                let value = take_flag_value(args, &mut index, inline, "--connect-timeout-secs")?;
                set_once(&mut connect_timeout_secs, "--connect-timeout-secs", value)?;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option: {other}"));
            }
            other => {
                set_once(&mut alias, "<alias>", other.to_string())?;
                index += 1;
            }
        }
    }

    let Some(alias) = alias else {
        return Err("missing required <alias>".to_string());
    };
    let Some(target) = target else {
        return Err("missing required --target <ssh-target>".to_string());
    };

    Ok(RemoteAddParams {
        alias,
        target,
        session,
        connection_policy,
        connect_timeout_secs,
    })
}

fn parse_remote_remove_args(args: &[String]) -> Result<(String, bool), String> {
    let mut alias = None;
    let mut confirm = false;

    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let (flag, inline) = split_inline_value(arg);
        match flag {
            "--confirm" => {
                if inline.is_some() {
                    return Err("--confirm takes no value".to_string());
                }
                confirm = true;
                index += 1;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option: {other}"));
            }
            other => {
                set_once(&mut alias, "<alias>", other.to_string())?;
                index += 1;
            }
        }
    }

    let Some(alias) = alias else {
        return Err("missing required <alias>".to_string());
    };
    Ok((alias, confirm))
}

fn parse_connection_policy(value: &str) -> Result<RemoteConnectionPolicy, String> {
    match value {
        "auto" => Ok(RemoteConnectionPolicy::Auto),
        "on_demand" => Ok(RemoteConnectionPolicy::OnDemand),
        "manual" => Ok(RemoteConnectionPolicy::Manual),
        _ => Err(format!(
            "invalid connection-policy '{value}' (expected auto, on_demand, or manual)"
        )),
    }
}

/// Read the local config text plus the currently configured remote hosts.
///
/// Returns an error (without touching the file) if the config is missing or
/// invalid TOML, or if the `[[remote.hosts]]` entries fail to deserialize.
/// This is stricter than `Config::load`, which silently falls back to defaults:
/// a mutating command must leave the file unchanged on parse failure.
fn load_remote_config_source(path: &Path) -> Result<(String, Vec<RemoteHostConfig>), String> {
    let content = if path.exists() {
        std::fs::read_to_string(path)
            .map_err(|err| format!("cannot read config at {}: {err}", path.display()))?
    } else {
        String::new()
    };
    let config: crate::config::Config = toml::from_str(&content).map_err(|err| {
        format!(
            "config at {} is invalid TOML: {err}; leaving config unchanged",
            path.display()
        )
    })?;
    Ok((content, config.remote.hosts))
}

/// Re-parse mutated config text and validate the combined remote host registry.
/// Proves a line-preserving mutation still round-trips through `Config` and
/// `RemoteHostRegistry` before the file is written.
fn validate_remote_config_text(content: &str) -> Result<(), String> {
    let config: crate::config::Config = toml::from_str(content).map_err(|err| {
        format!("resulting config would be invalid TOML: {err}; leaving config unchanged")
    })?;
    RemoteHostRegistry::from_configs(config.remote.hosts)
        .map(|_| ())
        .map_err(|err| {
            format!(
                "resulting config has invalid remote host config: {err}; leaving config unchanged"
            )
        })
}

/// Validate the config text produced by `remote add`: it must re-parse as TOML,
/// the combined host registry must be valid, AND `remote.enabled` must be true
/// (the command always enables federation). Any failure returns an error
/// message so the caller leaves the file unchanged.
///
/// The `remote.enabled == true` postcondition backstops `ensure_remote_enabled`:
/// if a table-header edge case (e.g. a header with an inline comment) ever made
/// the in-place enable miss, `remote add` reports failure and leaves the config
/// unchanged instead of writing a config that claims success but leaves
/// federation disabled.
fn validate_remote_add(content: &str) -> Result<(), String> {
    let config: crate::config::Config = toml::from_str(content).map_err(|err| {
        format!("resulting config would be invalid TOML: {err}; leaving config unchanged")
    })?;
    if !config.remote.enabled {
        return Err(
            "resulting config would not enable remote federation (remote.enabled != true); leaving config unchanged"
                .to_string(),
        );
    }
    RemoteHostRegistry::from_configs(config.remote.hosts).map_err(|err| {
        format!("resulting config has invalid remote host config: {err}; leaving config unchanged")
    })?;
    Ok(())
}

fn write_remote_config(path: &Path, content: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

fn format_remote_host_block(host: &RemoteHostConfig) -> String {
    let mut lines = Vec::with_capacity(6);
    lines.push("[[remote.hosts]]".to_string());
    lines.push(format!("name = {}", toml_basic_string(&host.name)));
    lines.push(format!("target = {}", toml_basic_string(&host.target)));
    lines.push(format!("session = {}", toml_basic_string(&host.session)));
    lines.push(format!(
        "connection_policy = {}",
        toml_basic_string(host.connection_policy.as_toml_str())
    ));
    lines.push(format!(
        "connect_timeout_secs = {}",
        host.connect_timeout_secs
    ));
    lines.join("\n")
}

/// Render a value as a TOML basic (double-quoted) string with the minimal
/// required escapes. Always produces valid TOML for any `&str`, so generated
/// `[[remote.hosts]]` blocks round-trip through `Config` even for names/targets
/// containing quotes, backslashes, or control characters.
fn toml_basic_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            c if c < ' ' => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn is_help_request(args: &[String]) -> bool {
    matches!(
        args.first().map(|arg| arg.as_str()),
        Some("help") | Some("--help") | Some("-h")
    )
}

/// Split a `--flag=value` token into `("--flag", Some("value"))`, or return
/// `("--flag", None)` for a bare `--flag`. Both spellings are accepted.
fn split_inline_value(arg: &str) -> (&str, Option<&str>) {
    if let Some((flag, value)) = arg.split_once('=') {
        if flag.starts_with("--") {
            return (flag, Some(value));
        }
    }
    (arg, None)
}

fn take_flag_value(
    args: &[String],
    index: &mut usize,
    inline: Option<&str>,
    flag: &str,
) -> Result<String, String> {
    match inline {
        Some(value) => {
            *index += 1;
            Ok(value.to_string())
        }
        None => {
            let Some(value) = args.get(*index + 1) else {
                return Err(format!("missing value for {flag}"));
            };
            // Do not swallow a following flag as this flag's value. A separate
            // value that itself starts with '-' is left for later validation
            // (e.g. a leading-dash SSH target is rejected by the registry).
            if value.starts_with("--") {
                return Err(format!("missing value for {flag}"));
            }
            *index += 2;
            Ok(value.clone())
        }
    }
}

fn set_once(slot: &mut Option<String>, label: &str, value: String) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("{label} specified more than once"));
    }
    *slot = Some(value);
    Ok(())
}

fn print_remote_help() {
    eprintln!("herdr remote commands:");
    eprintln!("  herdr remote list [HOST]");
    eprintln!("  herdr remote status [HOST]");
    eprintln!("  herdr remote check [HOST]");
    eprintln!("  herdr remote add <alias> --target <ssh-target> [--session <session>] [--connection-policy auto|on_demand|manual] [--connect-timeout-secs N]");
    eprintln!("  herdr remote remove <alias> --confirm");
    eprintln!();
    eprintln!("  `remote add`/`remove` edit local config only; they open no SSH bridge and");
    eprintln!("  probe/start/install nothing on the remote host.");
    eprintln!("  runtime bridge lifecycle commands `remote connect`/`reconnect`/`disconnect`");
    eprintln!("  are future work and are not implemented yet.");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, target: &str, session: &str) -> crate::remote_target::RemoteHostConfig {
        crate::remote_target::RemoteHostConfig::new(name, target, session, true)
    }

    fn registry() -> crate::remote_target::RemoteHostRegistry {
        crate::remote_target::RemoteHostRegistry::from_configs(vec![
            host("jafar", "jafar", "default"),
            host("work", "work-host", "agents"),
        ])
        .unwrap()
    }

    fn running_response(
        capabilities: Option<crate::api::schema::ServerCapabilities>,
    ) -> crate::remote::RemoteApiStatusResponse {
        crate::remote::RemoteApiStatusResponse {
            state: crate::remote::RemoteApiStatusState::Running,
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            protocol: Some(crate::protocol::PROTOCOL_VERSION),
            capabilities,
        }
    }

    #[test]
    fn remote_status_classifies_connected() {
        let status = classify_running_remote_api_status(&running_response(Some(
            crate::api::schema::ServerCapabilities::current(),
        )));

        assert_eq!(status.kind, RemoteStatusKind::Connected);
        assert!(status.detail.contains("federation advertised"));
    }

    #[test]
    fn remote_status_filter_accepts_no_host_or_one_host() {
        assert_eq!(parse_remote_host_filter(&[]).unwrap(), None);
        assert_eq!(
            parse_remote_host_filter(&["jafar".to_string()]).unwrap(),
            Some("jafar")
        );
    }

    #[test]
    fn remote_status_filter_rejects_too_many_args() {
        assert_eq!(
            parse_remote_host_filter(&["jafar".to_string(), "extra".to_string()]),
            Err(())
        );
    }

    #[test]
    fn remote_status_hosts_returns_all_hosts_without_filter() {
        let registry = registry();

        let hosts = remote_status_hosts(&registry, None).unwrap();

        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].name, "jafar");
        assert_eq!(hosts[1].name, "work");
    }

    #[test]
    fn remote_status_hosts_filters_to_configured_alias() {
        let registry = registry();

        let hosts = remote_status_hosts(&registry, Some("work")).unwrap();

        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].name, "work");
        assert_eq!(hosts[0].target, "work-host");
    }

    #[test]
    fn remote_status_hosts_rejects_unknown_host_before_probe() {
        let registry = registry();

        let err = remote_status_hosts(&registry, Some("missing")).unwrap_err();

        assert_eq!(
            err,
            RemoteStatusHostError::UnknownHost("missing".to_string())
        );
    }

    #[test]
    fn remote_status_config_validation_rejects_invalid_remote_config_before_probe() {
        let hosts = vec![host("bad", "-oProxyCommand=x", "default")];

        let err = remote_status_registry(&hosts).unwrap_err();

        assert_eq!(
            err,
            crate::remote_target::RemoteHostConfigError::SshTargetStartsWithDash
        );
    }

    #[test]
    fn remote_list_row_surfaces_inventory_fields_without_probing() {
        // remote_list_row builds the inventory view from a configured host
        // directly: alias, SSH target, remote session, connection policy via
        // `as_toml_str()`, and the connect timeout. It performs no IO and never
        // reaches an SSH/probe helper.
        let host = host("jafar", "user@jafar", "agents");

        let row = remote_list_row(&host);

        assert_eq!(
            row,
            RemoteListRow {
                host: "jafar".to_string(),
                target: "user@jafar".to_string(),
                session: "agents".to_string(),
                policy: "auto",
                timeout: "10s".to_string(),
            }
        );
    }

    #[test]
    fn remote_list_row_surfaces_connection_policy_via_as_toml_str() {
        use crate::remote_target::RemoteConnectionPolicy;

        let base = host("jafar", "jafar", "default");
        assert_eq!(
            remote_list_row(
                &base
                    .clone()
                    .with_connection_policy(RemoteConnectionPolicy::Auto)
            )
            .policy,
            RemoteConnectionPolicy::Auto.as_toml_str()
        );
        assert_eq!(
            remote_list_row(
                &base
                    .clone()
                    .with_connection_policy(RemoteConnectionPolicy::OnDemand)
            )
            .policy,
            RemoteConnectionPolicy::OnDemand.as_toml_str()
        );
        assert_eq!(
            remote_list_row(&base.with_connection_policy(RemoteConnectionPolicy::Manual)).policy,
            RemoteConnectionPolicy::Manual.as_toml_str()
        );
    }

    #[test]
    fn remote_list_row_surfaces_custom_connect_timeout_secs() {
        let host = host("jafar", "jafar", "default").with_connect_timeout_secs(45);

        let row = remote_list_row(&host);

        assert_eq!(row.timeout, "45s");
    }

    #[test]
    fn remote_list_routes_through_config_only_without_probing() {
        // `remote list` shares `configured_remote_hosts` with `remote status`
        // and `remote check`. That helper loads config, validates the registry,
        // and resolves the optional host filter WITHOUT opening an SSH bridge,
        // probing a remote server, or starting anything. Drive it under a
        // temporary config to assert list inherits the
        // disabled/no-hosts/unknown-host/invalid-config exit behavior of
        // status/check and returns valid hosts without probing.
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let prior = std::env::var_os(crate::config::CONFIG_PATH_ENV_VAR);
        let temp = std::env::temp_dir().join(format!(
            "herdr-remote-list-cfg-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));

        fn load_at(path: &std::path::Path, contents: &str) {
            std::fs::write(path, contents).unwrap();
            std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, path);
        }

        // Disabled remote federation -> exit 0 with no hosts returned.
        load_at(&temp, "[remote]\nenabled = false\n");
        assert_eq!(
            configured_remote_hosts(&[], "herdr remote list [HOST]").unwrap(),
            RemoteHostSelection::Exit(0)
        );

        // Enabled but no hosts configured -> exit 0.
        load_at(&temp, "[remote]\nenabled = true\n");
        assert_eq!(
            configured_remote_hosts(&[], "herdr remote list [HOST]").unwrap(),
            RemoteHostSelection::Exit(0)
        );

        // Enabled with a valid host, but an unknown host filter -> exit 1
        // before any probe.
        load_at(
            &temp,
            "[remote]\nenabled = true\n[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\n",
        );
        assert_eq!(
            configured_remote_hosts(&["missing".to_string()], "herdr remote list [HOST]").unwrap(),
            RemoteHostSelection::Exit(1)
        );

        // Invalid config (leading-dash SSH target) -> exit 1 before any probe.
        load_at(
            &temp,
            "[remote]\nenabled = true\n[[remote.hosts]]\nname = \"bad\"\ntarget = \"-oProxyCommand=x\"\nsession = \"default\"\n",
        );
        assert_eq!(
            configured_remote_hosts(&[], "herdr remote list [HOST]").unwrap(),
            RemoteHostSelection::Exit(1)
        );

        // Valid configured host -> returned WITHOUT probing (no SSH/bridge/server).
        load_at(
            &temp,
            "[remote]\nenabled = true\n[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\n",
        );
        let hosts = match configured_remote_hosts(&[], "herdr remote list [HOST]").unwrap() {
            RemoteHostSelection::Hosts(hosts) => hosts,
            other => panic!("expected hosts for valid config, got {other:?}"),
        };
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].name, "jafar");

        match prior {
            Some(value) => std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, value),
            None => std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR),
        }
        let _ = std::fs::remove_file(&temp);
    }

    #[test]
    fn remote_check_binary_row_reports_compatible_binary() {
        let row = remote_check_binary_row(Ok("$HOME/.local/bin/herdr".to_string()));

        assert_eq!(
            row,
            RemoteCheckRow {
                check: "ssh/binary",
                status: "ok",
                detail: "compatible Herdr binary at $HOME/.local/bin/herdr".to_string(),
            }
        );
        assert!(row.should_continue());
    }

    #[test]
    fn remote_check_binary_row_classifies_missing_binary_as_needs_update() {
        let row = remote_check_binary_row(Err(io::Error::new(
            io::ErrorKind::NotFound,
            "compatible herdr was not found on remote host jafar",
        )));

        assert_eq!(row.check, "ssh/binary");
        assert_eq!(row.status, "needs update");
        assert!(row.detail.contains("compatible herdr"));
        assert!(row
            .detail
            .contains("install/update Herdr on the remote host"));
        assert!(!row.should_continue());
    }

    #[test]
    fn remote_check_federation_row_lists_advertised_methods() {
        let row = remote_check_federation_row(Ok(crate::api::schema::FederationCapabilities {
            methods: vec![
                crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE.to_string(),
                crate::api::schema::FederationCapabilities::AGENT_SEND.to_string(),
            ],
        }));

        assert_eq!(row.check, "federation");
        assert_eq!(row.status, "ok");
        assert_eq!(row.detail, "methods: remote_api_bridge,agent_send");
        assert!(row.should_continue());
    }

    #[test]
    fn remote_check_federation_row_classifies_old_command_as_needs_update() {
        let row = remote_check_federation_row(Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote host jafar has a Herdr binary that does not advertise federation support",
        )));

        assert_eq!(row.check, "federation");
        assert_eq!(row.status, "needs update");
        assert!(row.detail.contains("does not advertise federation support"));
        assert!(!row.should_continue());
    }

    #[test]
    fn remote_check_api_row_reports_not_running_without_starting_it() {
        let row = remote_check_api_row(
            &host("jafar", "jafar", "fed-agents"),
            Ok(crate::remote::RemoteApiStatusResponse {
                state: crate::remote::RemoteApiStatusState::NotRunning,
                version: None,
                protocol: None,
                capabilities: None,
            }),
        );

        assert_eq!(row.check, "api");
        assert_eq!(row.status, "not running");
        assert!(row.detail.contains("not running"));
        assert!(row
            .detail
            .contains("run herdr --remote jafar --session fed-agents interactively"));
        assert!(!row.should_continue());
    }

    #[test]
    fn remote_check_api_row_reports_running_connected() {
        let row = remote_check_api_row(
            &host("jafar", "jafar", "default"),
            Ok(running_response(Some(
                crate::api::schema::ServerCapabilities::current(),
            ))),
        );

        assert_eq!(row.check, "api");
        assert_eq!(row.status, "connected");
        assert!(row.detail.contains("federation advertised"));
    }

    #[test]
    fn remote_status_classifies_not_running_status() {
        let status = classify_remote_api_status(
            &host("jafar", "jafar", "fed-agents"),
            &crate::remote::RemoteApiStatusResponse {
                state: crate::remote::RemoteApiStatusState::NotRunning,
                version: None,
                protocol: None,
                capabilities: None,
            },
        );

        assert_eq!(status.kind, RemoteStatusKind::NotRunning);
        assert!(status.detail.contains("not running"));
        assert!(status
            .detail
            .contains("run herdr --remote jafar --session fed-agents interactively"));
    }

    #[test]
    fn remote_status_not_running_guidance_quotes_shell_arguments() {
        let status = classify_remote_api_status(
            &host("jafar", "host name", "fed agents"),
            &crate::remote::RemoteApiStatusResponse {
                state: crate::remote::RemoteApiStatusState::NotRunning,
                version: None,
                protocol: None,
                capabilities: None,
            },
        );

        assert!(status
            .detail
            .contains("run herdr --remote 'host name' --session 'fed agents' interactively"));
    }

    #[test]
    fn remote_status_classifies_running_missing_federation_as_incompatible() {
        let status = classify_running_remote_api_status(&running_response(Some(
            crate::api::schema::ServerCapabilities {
                live_handoff: true,
                detached_server_daemon: false,
                federation: None,
            },
        )));

        assert_eq!(status.kind, RemoteStatusKind::Incompatible);
        assert!(status
            .detail
            .contains("does not advertise federation support"));
        assert!(status
            .detail
            .contains("install/update Herdr on the remote host"));
    }

    #[test]
    fn remote_status_classifies_missing_federation_as_incompatible() {
        let err = io::Error::new(
            io::ErrorKind::InvalidData,
            "remote host jafar has a Herdr binary that does not advertise federation support",
        );

        let status = classify_remote_status_error(&err);

        assert_eq!(status.kind, RemoteStatusKind::Incompatible);
        assert!(status
            .detail
            .contains("does not advertise federation support"));
        assert!(status
            .detail
            .contains("install/update Herdr on the remote host"));
    }

    #[test]
    fn remote_status_classifies_unknown_command_invalid_data_as_incompatible() {
        let err = io::Error::new(
            io::ErrorKind::InvalidData,
            "remote host jafar API status probe failed: unknown command: remote-api-status",
        );

        let status = classify_remote_status_error(&err);

        assert_eq!(status.kind, RemoteStatusKind::Incompatible);
        assert!(status.detail.contains("unknown command"));
        assert!(status
            .detail
            .contains("install/update Herdr on the remote host"));
    }

    #[test]
    fn remote_status_classifies_missing_binary_as_incompatible() {
        let err = io::Error::new(
            io::ErrorKind::NotFound,
            "compatible herdr was not found on remote host jafar",
        );

        let status = classify_remote_status_error(&err);

        assert_eq!(status.kind, RemoteStatusKind::Incompatible);
        assert!(status.detail.contains("compatible herdr"));
        assert!(status
            .detail
            .contains("install/update Herdr on the remote host"));
    }

    #[test]
    fn remote_status_classifies_empty_stderr_transport_255_as_unreachable() {
        let err = io::Error::other("remote platform detection failed: exit status: 255");

        let status = classify_remote_status_error(&err);

        assert_eq!(status.kind, RemoteStatusKind::Unreachable);
        assert!(status.detail.starts_with("check SSH access:"));
        assert!(status.detail.contains("exit status: 255"));
    }

    #[test]
    fn remote_status_classifies_transient_transport_as_unreachable() {
        let err = io::Error::new(io::ErrorKind::TimedOut, "connection timed out");

        let status = classify_remote_status_error(&err);

        assert_eq!(status.kind, RemoteStatusKind::Unreachable);
        assert!(status.detail.starts_with("check SSH access:"));
        assert!(status.detail.contains("timed out"));
    }

    #[test]
    fn remote_status_classifies_ssh_other_error_as_unreachable() {
        let err =
            io::Error::other("remote platform detection failed: Permission denied (publickey)");

        let status = classify_remote_status_error(&err);

        assert_eq!(status.kind, RemoteStatusKind::Unreachable);
        assert!(status.detail.starts_with("check SSH access:"));
        assert!(status.detail.contains("Permission denied"));
    }

    #[test]
    fn remote_status_classifies_unknown_error_as_unknown() {
        let err = io::Error::other("unexpected parse failure");

        let status = classify_remote_status_error(&err);

        assert_eq!(status.kind, RemoteStatusKind::Unknown);
        assert!(status.detail.contains("unexpected parse failure"));
    }

    #[test]
    fn remote_status_normalizes_multiline_detail_for_table() {
        let err = io::Error::new(
            io::ErrorKind::InvalidData,
            "remote host jafar has a Herdr binary that does not advertise federation support: unknown command: remote-federation-capabilities\nrun 'herdr --help' for usage",
        );

        let status = classify_remote_status_error(&err);

        assert_eq!(status.kind, RemoteStatusKind::Incompatible);
        assert!(!status.detail.contains('\n'));
        assert_eq!(
            status.detail,
            "remote host jafar has a Herdr binary that does not advertise federation support: unknown command: remote-federation-capabilities run 'herdr --help' for usage; install/update Herdr on the remote host"
        );
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    /// Run a closure with `HERDR_CONFIG_PATH` pointed at a unique temp file
    /// (optionally seeded with `initial`), then restore the prior env and remove
    /// the temp file. The real main config is never touched because
    /// `config_path()` honors `HERDR_CONFIG_PATH` first.
    fn with_temp_config<F: FnOnce(&std::path::Path)>(initial: Option<&str>, body: F) {
        let _guard = crate::config::test_config_env_lock().lock().unwrap();
        let prior = std::env::var_os(crate::config::CONFIG_PATH_ENV_VAR);
        let temp = std::env::temp_dir().join(format!(
            "herdr-remote-mut-cfg-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        match initial {
            Some(contents) => std::fs::write(&temp, contents).unwrap(),
            None => {
                let _ = std::fs::remove_file(&temp);
            }
        }
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &temp);
        body(&temp);
        match prior {
            Some(value) => std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, value),
            None => std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR),
        }
        let _ = std::fs::remove_file(&temp);
    }

    fn read_config_hosts(path: &std::path::Path) -> Vec<String> {
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let config: crate::config::Config = toml::from_str(&content).unwrap_or_default();
        config
            .remote
            .hosts
            .into_iter()
            .map(|host| host.name)
            .collect()
    }

    #[test]
    fn parse_remote_add_args_accepts_required_and_optional_flags() {
        let params = parse_remote_add_args(&args(&[
            "jafar",
            "--target",
            "user@jafar",
            "--session",
            "agents",
            "--connection-policy",
            "on_demand",
            "--connect-timeout-secs",
            "25",
        ]))
        .unwrap();
        assert_eq!(params.alias, "jafar");
        assert_eq!(params.target, "user@jafar");
        assert_eq!(params.session.as_deref(), Some("agents"));
        assert_eq!(params.connection_policy.as_deref(), Some("on_demand"));
        assert_eq!(params.connect_timeout_secs.as_deref(), Some("25"));
    }

    #[test]
    fn parse_remote_add_args_accepts_inline_flag_values() {
        let params = parse_remote_add_args(&args(&[
            "jafar",
            "--target=user@jafar",
            "--connection-policy=manual",
        ]))
        .unwrap();
        assert_eq!(params.alias, "jafar");
        assert_eq!(params.target, "user@jafar");
        assert_eq!(params.connection_policy.as_deref(), Some("manual"));
        assert!(params.session.is_none());
    }

    #[test]
    fn parse_remote_add_args_rejects_missing_alias() {
        assert!(parse_remote_add_args(&args(&["--target", "x"]))
            .unwrap_err()
            .contains("missing required <alias>"));
    }

    #[test]
    fn parse_remote_add_args_rejects_missing_target() {
        assert!(parse_remote_add_args(&args(&["jafar"]))
            .unwrap_err()
            .contains("missing required --target"));
    }

    #[test]
    fn parse_remote_add_args_rejects_unknown_flag() {
        assert!(
            parse_remote_add_args(&args(&["jafar", "--target", "x", "--bogus"]))
                .unwrap_err()
                .contains("unknown option: --bogus")
        );
    }

    #[test]
    fn parse_remote_add_args_rejects_leading_dash_alias_as_unknown_option() {
        // A leading-dash alias looks like a flag and is rejected before any
        // config write; registry validation also rejects such aliases.
        assert!(parse_remote_add_args(&args(&["-bad", "--target", "x"]))
            .unwrap_err()
            .contains("unknown option"));
    }

    #[test]
    fn parse_remote_add_args_rejects_repeated_flag() {
        assert!(
            parse_remote_add_args(&args(&["jafar", "--target", "a", "--target", "b"]))
                .unwrap_err()
                .contains("specified more than once")
        );
    }

    #[test]
    fn parse_remote_add_args_does_not_swallow_next_flag_as_value() {
        assert!(
            parse_remote_add_args(&args(&["jafar", "--target", "--session"]))
                .unwrap_err()
                .contains("missing value for --target")
        );
    }

    #[test]
    fn parse_connection_policy_accepts_known_values() {
        assert_eq!(
            parse_connection_policy("auto").unwrap(),
            RemoteConnectionPolicy::Auto
        );
        assert_eq!(
            parse_connection_policy("on_demand").unwrap(),
            RemoteConnectionPolicy::OnDemand
        );
        assert_eq!(
            parse_connection_policy("manual").unwrap(),
            RemoteConnectionPolicy::Manual
        );
    }

    #[test]
    fn parse_connection_policy_rejects_unknown_value() {
        assert!(parse_connection_policy("always")
            .unwrap_err()
            .contains("invalid connection-policy"));
    }

    #[test]
    fn parse_remote_remove_args_requires_alias_and_confirm() {
        let (alias, confirm) = parse_remote_remove_args(&args(&["jafar", "--confirm"])).unwrap();
        assert_eq!(alias, "jafar");
        assert!(confirm);
    }

    #[test]
    fn parse_remote_remove_args_confirm_takes_no_value() {
        assert!(parse_remote_remove_args(&args(&["jafar", "--confirm=yes"]))
            .unwrap_err()
            .contains("--confirm takes no value"));
    }

    #[test]
    fn remote_add_writes_host_enables_federation_and_round_trips() {
        with_temp_config(None, |path| {
            let code = remote_add(&args(&["jafar", "--target", "user@jafar"])).unwrap();
            assert_eq!(code, 0);

            let content = std::fs::read_to_string(path).unwrap();
            assert!(content.contains("[remote]"));
            assert!(content.contains("enabled = true"));
            assert!(content.contains("[[remote.hosts]]"));
            assert!(content.contains("name = \"jafar\""));
            assert!(content.contains("target = \"user@jafar\""));
            assert!(content.contains("session = \"default\""));
            assert!(content.contains("connection_policy = \"auto\""));
            assert!(content.contains("connect_timeout_secs = 10"));
            assert_eq!(read_config_hosts(path), vec!["jafar".to_string()]);

            // `remote list` resolves the just-added host without probing.
            assert_eq!(remote_list(&args(&[])).unwrap(), 0);
        });
    }

    #[test]
    fn remote_add_applies_optional_overrides() {
        with_temp_config(None, |path| {
            let code = remote_add(&args(&[
                "work",
                "--target",
                "work",
                "--session",
                "agents",
                "--connection-policy",
                "manual",
                "--connect-timeout-secs",
                "45",
            ]))
            .unwrap();
            assert_eq!(code, 0);
            let content = std::fs::read_to_string(path).unwrap();
            assert!(content.contains("session = \"agents\""));
            assert!(content.contains("connection_policy = \"manual\""));
            assert!(content.contains("connect_timeout_secs = 45"));
        });
    }

    #[test]
    fn remote_add_creates_config_file_when_absent() {
        with_temp_config(None, |path| {
            assert!(!path.exists());
            let code = remote_add(&args(&["jafar", "--target", "jafar"])).unwrap();
            assert_eq!(code, 0);
            assert!(path.exists());
        });
    }

    #[test]
    fn remote_add_preserves_existing_config_and_comments() {
        let initial =
            "# keep me\n[theme]\nname = \"catppuccin\"\n\n[remote]\nmanage_ssh_config = true\n";
        with_temp_config(Some(initial), |path| {
            let code = remote_add(&args(&["jafar", "--target", "jafar"])).unwrap();
            assert_eq!(code, 0);
            let content = std::fs::read_to_string(path).unwrap();
            assert!(content.contains("# keep me"));
            assert!(content.contains("[theme]\nname = \"catppuccin\""));
            assert!(content.contains("manage_ssh_config = true"));
            assert!(content.contains("enabled = true"));
        });
    }

    #[test]
    fn remote_add_rejects_duplicate_alias_without_changing_config() {
        let initial = "[remote]\nenabled = true\n\n[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\n";
        with_temp_config(Some(initial), |path| {
            let before = std::fs::read_to_string(path).unwrap();
            let code = remote_add(&args(&["jafar", "--target", "other"])).unwrap();
            assert_eq!(code, 1);
            // Config unchanged: still exactly one jafar, no second entry.
            let after = std::fs::read_to_string(path).unwrap();
            assert_eq!(before, after);
            assert_eq!(read_config_hosts(path), vec!["jafar".to_string()]);
        });
    }

    #[test]
    fn remote_add_rejects_invalid_alias_without_changing_config() {
        with_temp_config(None, |path| {
            let code = remote_add(&args(&["ja/far", "--target", "x"])).unwrap();
            assert_eq!(code, 1);
            assert!(!path.exists());
        });
    }

    #[test]
    fn remote_add_rejects_leading_dash_target_without_changing_config() {
        with_temp_config(None, |path| {
            let code = remote_add(&args(&["bad", "--target", "-oProxyCommand=x"])).unwrap();
            assert_eq!(code, 1);
            assert!(!path.exists());
        });
    }

    #[test]
    fn remote_add_rejects_invalid_policy_and_timeout_as_usage_errors() {
        with_temp_config(None, |path| {
            assert_eq!(
                remote_add(&args(&[
                    "jafar",
                    "--target",
                    "x",
                    "--connection-policy",
                    "always"
                ]))
                .unwrap(),
                2
            );
            assert_eq!(
                remote_add(&args(&[
                    "jafar",
                    "--target",
                    "x",
                    "--connect-timeout-secs",
                    "abc"
                ]))
                .unwrap(),
                2
            );
            // Out-of-range timeout (0 / too large) is caught by registry rules.
            assert_eq!(
                remote_add(&args(&[
                    "jafar",
                    "--target",
                    "x",
                    "--connect-timeout-secs",
                    "0"
                ]))
                .unwrap(),
                1
            );
            assert_eq!(
                remote_add(&args(&[
                    "jafar",
                    "--target",
                    "x",
                    "--connect-timeout-secs",
                    "99999"
                ]))
                .unwrap(),
                1
            );
            assert!(!path.exists());
        });
    }

    #[test]
    fn remote_add_rejects_malformed_args_as_usage_errors() {
        with_temp_config(None, |path| {
            // Missing --target.
            assert_eq!(remote_add(&args(&["jafar"])).unwrap(), 2);
            // Missing alias.
            assert_eq!(remote_add(&args(&["--target", "x"])).unwrap(), 2);
            // Unknown flag.
            assert_eq!(
                remote_add(&args(&["jafar", "--target", "x", "--bogus"])).unwrap(),
                2
            );
            assert!(!path.exists());
        });
    }

    #[test]
    fn remote_add_help_returns_zero_without_writing() {
        with_temp_config(None, |path| {
            assert_eq!(remote_add(&args(&["--help"])).unwrap(), 0);
            assert!(!path.exists());
        });
    }

    #[test]
    fn remote_remove_requires_confirm_without_changing_config() {
        let initial = "[remote]\nenabled = true\n\n[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\n";
        with_temp_config(Some(initial), |path| {
            let before = std::fs::read_to_string(path).unwrap();
            assert_eq!(remote_remove(&args(&["jafar"])).unwrap(), 2);
            assert_eq!(std::fs::read_to_string(path).unwrap(), before);
        });
    }

    #[test]
    fn remote_remove_rejects_unknown_alias_without_changing_config() {
        let initial = "[remote]\nenabled = true\n\n[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\n";
        with_temp_config(Some(initial), |path| {
            let before = std::fs::read_to_string(path).unwrap();
            assert_eq!(remote_remove(&args(&["missing", "--confirm"])).unwrap(), 1);
            assert_eq!(std::fs::read_to_string(path).unwrap(), before);
        });
    }

    #[test]
    fn remote_remove_removes_one_host_of_many_preserving_others() {
        let initial = "[remote]\nenabled = true\n\n# jafar entry\n[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\n\n[[remote.hosts]]\nname = \"work\"\ntarget = \"work\"\nsession = \"agents\"\n";
        with_temp_config(Some(initial), |path| {
            let code = remote_remove(&args(&["jafar", "--confirm"])).unwrap();
            assert_eq!(code, 0);
            let content = std::fs::read_to_string(path).unwrap();
            assert!(!content.contains("name = \"jafar\""));
            assert!(content.contains("name = \"work\""));
            assert!(content.contains("[remote]\nenabled = true"));
            assert_eq!(read_config_hosts(path), vec!["work".to_string()]);
        });
    }

    #[test]
    fn remote_remove_rejects_invalid_config_before_writing() {
        // Duplicate aliases in config fail registry validation; config unchanged.
        let initial = "[remote]\nenabled = true\n\n[[remote.hosts]]\nname = \"dup\"\ntarget = \"a\"\nsession = \"default\"\n\n[[remote.hosts]]\nname = \"dup\"\ntarget = \"b\"\nsession = \"default\"\n";
        with_temp_config(Some(initial), |path| {
            let before = std::fs::read_to_string(path).unwrap();
            assert_eq!(remote_remove(&args(&["dup", "--confirm"])).unwrap(), 1);
            assert_eq!(std::fs::read_to_string(path).unwrap(), before);
        });
    }

    #[test]
    fn remote_add_then_remove_round_trip_smoke() {
        // Focused CLI smoke with an explicit temp HERDR_CONFIG_PATH: add -> list
        // sees the host -> duplicate add fails unchanged -> remove without
        // confirm fails -> remove --confirm succeeds. The real main config/state
        // are never touched because config_path() honors HERDR_CONFIG_PATH.
        with_temp_config(None, |path| {
            // add jafar
            assert_eq!(
                remote_add(&args(&["jafar", "--target", "jafar"])).unwrap(),
                0
            );
            assert_eq!(read_config_hosts(path), vec!["jafar".to_string()]);
            assert_eq!(remote_list(&args(&[])).unwrap(), 0);

            // duplicate add fails, config unchanged
            let before = std::fs::read_to_string(path).unwrap();
            assert_eq!(
                remote_add(&args(&["jafar", "--target", "other"])).unwrap(),
                1
            );
            assert_eq!(std::fs::read_to_string(path).unwrap(), before);

            // remove without confirm fails
            assert_eq!(remote_remove(&args(&["jafar"])).unwrap(), 2);
            assert_eq!(std::fs::read_to_string(path).unwrap(), before);

            // remove --confirm succeeds
            assert_eq!(remote_remove(&args(&["jafar", "--confirm"])).unwrap(), 0);
            assert!(read_config_hosts(path).is_empty());
            let after = std::fs::read_to_string(path).unwrap();
            assert!(after.contains("[remote]"));
        });
    }

    #[test]
    fn remote_add_preserves_theme_with_trailing_comment_header() {
        // Reviewer B case (c): a [theme] table header carrying a trailing inline
        // comment must be detected as a section boundary. `remote add` must
        // enable federation inside [remote] (remote.enabled == true) and must
        // NOT mutate the theme table's `enabled = false`. Asserted via
        // toml::Value so the (serde-ignored) theme.enabled key is still checked.
        let initial =
            "[remote]\n# maybe existing config\n\n[theme] # inline table comment\nenabled = false\n";
        with_temp_config(Some(initial), |path| {
            let code = remote_add(&args(&["jafar", "--target", "jafar"])).unwrap();
            assert_eq!(code, 0);
            let content = std::fs::read_to_string(path).unwrap();
            let value: toml::Value = toml::from_str(&content).expect("resulting config must parse");
            assert_eq!(
                value["remote"]["enabled"].as_bool(),
                Some(true),
                "remote.enabled must be true; got config:\n{content}"
            );
            assert_eq!(
                value["theme"]["enabled"].as_bool(),
                Some(false),
                "theme.enabled must stay false; got config:\n{content}"
            );
            assert!(content.contains("[theme] # inline table comment"));
            assert_eq!(read_config_hosts(path), vec!["jafar".to_string()]);
        });
    }

    #[test]
    fn remote_remove_preserves_theme_with_trailing_comment_header() {
        // Reviewer B case (b) at the CLI level: removing a host block followed
        // by a [theme] header with a trailing inline comment must delete only
        // the host block and leave the theme section (and enabled = false) intact.
        let initial = "[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\nconnection_policy = \"auto\"\nconnect_timeout_secs = 10\n\n[theme] # inline table comment\nenabled = false\n";
        with_temp_config(Some(initial), |path| {
            let code = remote_remove(&args(&["jafar", "--confirm"])).unwrap();
            assert_eq!(code, 0);
            let content = std::fs::read_to_string(path).unwrap();
            let value: toml::Value = toml::from_str(&content).expect("resulting config must parse");
            assert_eq!(
                value["theme"]["enabled"].as_bool(),
                Some(false),
                "theme section must be preserved with enabled = false; got config:\n{content}"
            );
            assert!(content.contains("[theme] # inline table comment"));
            assert!(read_config_hosts(path).is_empty());
        });
    }

    #[test]
    fn remote_add_preserves_quoted_table_with_hash_in_key() {
        // Reviewer B quoted-header case at the CLI level: a config with a valid
        // TOML table whose quoted key contains `#` must NOT have that table's
        // `enabled` key rewritten to true when `remote add` enables federation.
        let initial = "[remote]\nenabled = true\n\n[\"theme#dark\"]\nenabled = false\n";
        with_temp_config(Some(initial), |path| {
            let code = remote_add(&args(&["jafar", "--target", "jafar"])).unwrap();
            assert_eq!(code, 0);
            let content = std::fs::read_to_string(path).unwrap();
            let value: toml::Value = toml::from_str(&content).expect("resulting config must parse");
            assert_eq!(
                value["remote"]["enabled"].as_bool(),
                Some(true),
                "remote.enabled must be true; got config:\n{content}"
            );
            assert_eq!(
                value["theme#dark"]["enabled"].as_bool(),
                Some(false),
                "theme#dark.enabled must stay false; got config:\n{content}"
            );
            assert!(content.contains("[\"theme#dark\"]"));
            assert_eq!(read_config_hosts(path), vec!["jafar".to_string()]);
        });
    }

    #[test]
    fn remote_remove_preserves_quoted_table_with_hash_in_key() {
        // Reviewer B quoted-header case at the CLI level: removing a host block
        // followed by a quoted table whose key contains `#` must delete only the
        // host block and preserve the quoted table and its enabled key.
        let initial = "[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\nconnection_policy = \"auto\"\nconnect_timeout_secs = 10\n\n[\"theme#dark\"]\nenabled = false\n";
        with_temp_config(Some(initial), |path| {
            let code = remote_remove(&args(&["jafar", "--confirm"])).unwrap();
            assert_eq!(code, 0);
            let content = std::fs::read_to_string(path).unwrap();
            let value: toml::Value = toml::from_str(&content).expect("resulting config must parse");
            assert_eq!(
                value["theme#dark"]["enabled"].as_bool(),
                Some(false),
                "theme#dark section must be preserved; got config:\n{content}"
            );
            assert!(content.contains("[\"theme#dark\"]"));
            assert!(read_config_hosts(path).is_empty());
        });
    }

    #[test]
    fn validate_remote_add_requires_federation_enabled() {
        // Defense in depth: `remote add` must refuse to write a config whose
        // resulting remote.enabled is not true, even if header detection missed
        // and the host registry alone is valid.
        let disabled_but_valid = "[remote]\nenabled = false\n\n[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\n";
        assert!(validate_remote_add(disabled_but_valid).is_err());

        let enabled = "[remote]\nenabled = true\n\n[[remote.hosts]]\nname = \"jafar\"\ntarget = \"jafar\"\nsession = \"default\"\n";
        assert!(validate_remote_add(enabled).is_ok());
    }
}
