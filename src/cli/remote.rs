use std::io;

pub(super) fn run_remote_command(args: &[String]) -> io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_remote_help();
        return Ok(2);
    };

    match subcommand {
        "list" => remote_list(&args[1..]),
        "status" => remote_status(&args[1..]),
        "check" => remote_check(&args[1..]),
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

fn print_remote_help() {
    eprintln!("herdr remote commands:");
    eprintln!("  herdr remote list [HOST]");
    eprintln!("  herdr remote status [HOST]");
    eprintln!("  herdr remote check [HOST]");
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
}
