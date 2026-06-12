use std::io;

pub(super) fn run_remote_command(args: &[String]) -> io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_remote_help();
        return Ok(2);
    };

    match subcommand {
        "status" => remote_status(&args[1..]),
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

fn remote_status(args: &[String]) -> io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr remote status");
        return Ok(2);
    }

    let loaded = crate::config::Config::load();
    let remote = &loaded.config.remote;
    if !remote.enabled {
        println!("remote federation is disabled (set remote.enabled = true to configure remotes).");
        return Ok(0);
    }

    let registry = match remote_status_registry(&remote.hosts) {
        Ok(registry) => registry,
        Err(err) => {
            eprintln!("invalid remote host config: {err}");
            return Ok(1);
        }
    };
    let hosts = registry.list();
    if hosts.is_empty() {
        println!("remote federation is enabled, but no remote hosts are configured.");
        return Ok(0);
    }

    println!("{:<20} {:<18} {:<14} details", "host", "status", "session");
    for host in hosts {
        let status = probe_remote_status(host);
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

fn remote_status_registry(
    hosts: &[crate::remote_target::RemoteHostConfig],
) -> Result<crate::remote_target::RemoteHostRegistry, crate::remote_target::RemoteHostConfigError> {
    crate::remote_target::RemoteHostRegistry::from_configs(hosts.to_vec())
}

fn probe_remote_status(host: &crate::remote_target::RemoteHostConfig) -> RemoteStatus {
    match crate::remote::remote_api_status_to_host_noninteractive(host) {
        Ok(status) => classify_remote_api_status(&status),
        Err(err) => classify_remote_status_error(&err),
    }
}

fn classify_remote_api_status(status: &crate::remote::RemoteApiStatusResponse) -> RemoteStatus {
    match status.state {
        crate::remote::RemoteApiStatusState::NotRunning => RemoteStatus {
            kind: RemoteStatusKind::NotRunning,
            detail: "remote Herdr API is not running for the configured session".to_string(),
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
            detail: format!(
                "running server protocol {}; local protocol is {}",
                protocol_label(status.protocol),
                crate::protocol::PROTOCOL_VERSION
            ),
        };
    }

    let Some(federation) = status
        .capabilities
        .as_ref()
        .and_then(|capabilities| capabilities.federation.as_ref())
    else {
        return RemoteStatus {
            kind: RemoteStatusKind::Incompatible,
            detail: "running server does not advertise federation support".to_string(),
        };
    };
    if !federation.supports_method(crate::api::schema::FederationCapabilities::REMOTE_API_BRIDGE) {
        return RemoteStatus {
            kind: RemoteStatusKind::Incompatible,
            detail: "running server does not advertise federation method remote_api_bridge"
                .to_string(),
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
    match err.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::InvalidData => RemoteStatus {
            kind: RemoteStatusKind::Incompatible,
            detail: message.clone(),
        },
        io::ErrorKind::ConnectionRefused
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::TimedOut
        | io::ErrorKind::WouldBlock
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::PermissionDenied => RemoteStatus {
            kind: RemoteStatusKind::Unreachable,
            detail: message.clone(),
        },
        _ if looks_like_ssh_transport_error(&message) => RemoteStatus {
            kind: RemoteStatusKind::Unreachable,
            detail: message.clone(),
        },
        _ => RemoteStatus {
            kind: RemoteStatusKind::Unknown,
            detail: message,
        },
    }
}

fn normalize_status_detail(detail: &str) -> String {
    detail.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn looks_like_ssh_transport_error(message: &str) -> bool {
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

fn print_remote_help() {
    eprintln!("herdr remote commands:");
    eprintln!("  herdr remote status");
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn remote_status_config_validation_rejects_invalid_remote_config_before_probe() {
        let hosts = vec![crate::remote_target::RemoteHostConfig::new(
            "bad",
            "-oProxyCommand=x",
            "default",
            true,
        )];

        let err = remote_status_registry(&hosts).unwrap_err();

        assert_eq!(
            err,
            crate::remote_target::RemoteHostConfigError::SshTargetStartsWithDash
        );
    }

    #[test]
    fn remote_status_classifies_not_running_status() {
        let status = classify_remote_api_status(&crate::remote::RemoteApiStatusResponse {
            state: crate::remote::RemoteApiStatusState::NotRunning,
            version: None,
            protocol: None,
            capabilities: None,
        });

        assert_eq!(status.kind, RemoteStatusKind::NotRunning);
        assert!(status.detail.contains("not running"));
    }

    #[test]
    fn remote_status_classifies_running_missing_federation_as_incompatible() {
        let status = classify_running_remote_api_status(&running_response(Some(
            crate::api::schema::ServerCapabilities {
                live_handoff: true,
                federation: None,
            },
        )));

        assert_eq!(status.kind, RemoteStatusKind::Incompatible);
        assert!(status
            .detail
            .contains("does not advertise federation support"));
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
    }

    #[test]
    fn remote_status_classifies_empty_stderr_transport_255_as_unreachable() {
        let err = io::Error::other("remote platform detection failed: exit status: 255");

        let status = classify_remote_status_error(&err);

        assert_eq!(status.kind, RemoteStatusKind::Unreachable);
        assert!(status.detail.contains("exit status: 255"));
    }

    #[test]
    fn remote_status_classifies_transient_transport_as_unreachable() {
        let err = io::Error::new(io::ErrorKind::TimedOut, "connection timed out");

        let status = classify_remote_status_error(&err);

        assert_eq!(status.kind, RemoteStatusKind::Unreachable);
        assert!(status.detail.contains("timed out"));
    }

    #[test]
    fn remote_status_classifies_ssh_other_error_as_unreachable() {
        let err =
            io::Error::other("remote platform detection failed: Permission denied (publickey)");

        let status = classify_remote_status_error(&err);

        assert_eq!(status.kind, RemoteStatusKind::Unreachable);
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
            "remote host jafar has a Herdr binary that does not advertise federation support: unknown command: remote-federation-capabilities run 'herdr --help' for usage"
        );
    }
}
