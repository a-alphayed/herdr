use std::collections::HashMap;

use crate::api::schema::{
    Method, Request, TabCloseParams, TabCreateParams, TabListParams, TabRenameParams, TabTarget,
};

pub(super) fn run_tab_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_tab_help();
        return Ok(2);
    };

    match subcommand {
        "list" => tab_list(&args[1..]),
        "create" => tab_create(&args[1..]),
        "get" => tab_get(&args[1..]),
        "focus" => tab_focus(&args[1..]),
        "rename" => tab_rename(&args[1..]),
        "close" => tab_close(&args[1..]),
        "help" | "--help" | "-h" => {
            print_tab_help();
            Ok(0)
        }
        _ => {
            print_tab_help();
            Ok(2)
        }
    }
}

fn tab_list(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:tab:list".into(),
        method: Method::TabList(TabListParams { workspace_id }),
    })?)
}

fn tab_create(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut cwd = None;
    let mut focus = false;
    let mut label = None;
    let mut env = HashMap::new();

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --label");
                    return Ok(2);
                };
                label = Some(value.clone());
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            "--env" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --env");
                    return Ok(2);
                };
                let (key, value) = match super::parse_env_assignment(value) {
                    Ok(pair) => pair,
                    Err(err) => {
                        eprintln!("{err}");
                        return Ok(2);
                    }
                };
                env.insert(key, value);
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:tab:create".into(),
        method: Method::TabCreate(TabCreateParams {
            workspace_id,
            cwd,
            focus,
            label,
            env,
        }),
    })?)
}

fn tab_get(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab get <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab get <tab_id>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:tab:get".into(),
        method: Method::TabGet(TabTarget {
            tab_id: super::normalize_tab_id(raw_tab_id),
        }),
    })?)
}

fn tab_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(raw_tab_id) = args.first() else {
        eprintln!("usage: herdr tab focus <tab_id>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr tab focus <tab_id>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:tab:focus".into(),
        method: Method::TabFocus(TabTarget {
            tab_id: super::normalize_tab_id(raw_tab_id),
        }),
    })?)
}

fn tab_rename(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr tab rename <tab_id> <label>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:tab:rename".into(),
        method: Method::TabRename(TabRenameParams {
            tab_id: super::normalize_tab_id(&args[0]),
            label: args[1..].join(" "),
        }),
    })?)
}

fn tab_close(args: &[String]) -> std::io::Result<i32> {
    let remote_hosts = cli_remote_host_registry();
    let params = match parse_tab_close_args(args, remote_hosts.as_ref()) {
        Ok(params) => params,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:tab:close".into(),
        method: Method::TabClose(params),
    })?)
}

fn cli_remote_host_registry() -> Option<crate::remote_target::RemoteHostRegistry> {
    let config = crate::config::Config::load().config;
    if !config.remote.enabled {
        return None;
    }
    crate::remote_target::RemoteHostRegistry::from_configs(config.remote.hosts).ok()
}

fn parse_tab_close_args(
    args: &[String],
    remote_hosts: Option<&crate::remote_target::RemoteHostRegistry>,
) -> Result<TabCloseParams, String> {
    let mut tab_id = None;
    let mut confirm = false;
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--confirm" => {
                confirm = true;
                index += 1;
            }
            other if other.starts_with("--") => return Err(format!("unknown option: {other}")),
            other if tab_id.is_none() => {
                tab_id = Some(super::normalize_tab_id(other));
                index += 1;
            }
            _ => return Err("usage: herdr tab close <tab_id> [--confirm]".into()),
        }
    }

    let Some(tab_id) = tab_id else {
        return Err("usage: herdr tab close <tab_id> [--confirm]".into());
    };

    if !confirm && tab_close_target_uses_configured_remote_host(&tab_id, remote_hosts) {
        return Err(format!(
            "tab.close on remote target {tab_id} is destructive; pass --confirm to proceed"
        ));
    }

    Ok(TabCloseParams { tab_id, confirm })
}

fn tab_close_target_uses_configured_remote_host(
    tab_id: &str,
    remote_hosts: Option<&crate::remote_target::RemoteHostRegistry>,
) -> bool {
    let Some(remote_hosts) = remote_hosts else {
        return false;
    };
    let Some((host, target)) = tab_id.split_once('/') else {
        return false;
    };
    remote_hosts.get(host).is_some() && target.starts_with("tab:")
}

fn print_tab_help() {
    eprintln!("herdr tab commands:");
    eprintln!("  herdr tab list [--workspace <workspace_id>]");
    eprintln!(
        "  herdr tab create [--workspace <workspace_id>] [--cwd PATH] [--label TEXT] [--env KEY=VALUE] [--focus] [--no-focus]"
    );
    eprintln!("  herdr tab get <tab_id>");
    eprintln!("  herdr tab focus <tab_id>");
    eprintln!("  herdr tab rename <tab_id> <label>");
    eprintln!("  herdr tab close <tab_id> [--confirm]");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn registry() -> crate::remote_target::RemoteHostRegistry {
        crate::remote_target::RemoteHostRegistry::from_configs(vec![
            crate::remote_target::RemoteHostConfig::new("jafar", "jafar", "default", true),
        ])
        .unwrap()
    }

    #[test]
    fn parse_tab_close_args_preserves_local_target_without_confirm() {
        let params = parse_tab_close_args(&args(&["local/tab-label"]), None).unwrap();
        assert_eq!(params.tab_id, "local/tab-label");
        assert!(!params.confirm);
    }

    #[test]
    fn parse_tab_close_args_does_not_require_confirm_for_unknown_slash_label() {
        let registry = registry();
        let params = parse_tab_close_args(&args(&["logs/tab"]), Some(&registry)).unwrap();

        assert_eq!(params.tab_id, "logs/tab");
        assert!(!params.confirm);
    }
}
