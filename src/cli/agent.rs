use std::collections::HashMap;

use crate::api::schema::{
    AgentReadParams, AgentRenameParams, AgentSendParams, AgentStartParams, AgentStatus,
    AgentSubmitParams, AgentTarget, AgentTeardownParams, EmptyParams, Method, ReadFormat,
    ReadSource, Request, Subscription,
};

const AGENT_START_USAGE: &str = "usage: herdr agent start <name> [--cwd PATH] [--workspace ID] [--tab ID] [--split right|down] [--env KEY=VALUE] [--focus|--no-focus] -- <argv...>\n       herdr agent start --host HOST --name NAME [--cwd REMOTE_PATH] [--workspace ID] [--tab ID] [--split right|down] [--env KEY=VALUE] [--focus|--no-focus] -- <argv...>";

pub(super) fn run_agent_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_agent_help();
        return Ok(2);
    };

    match subcommand {
        "list" => agent_list(&args[1..]),
        "get" => agent_get(&args[1..]),
        "read" => agent_read(&args[1..]),
        "send" => agent_send(&args[1..]),
        "submit" => agent_submit(&args[1..]),
        "rename" => agent_rename(&args[1..]),
        "focus" => agent_focus(&args[1..]),
        "wait" => agent_wait(&args[1..]),
        "attach" => agent_attach(&args[1..]),
        "start" => agent_start(&args[1..]),
        "teardown" => agent_teardown(&args[1..]),
        "explain" => agent_explain(&args[1..]),
        "help" | "--help" | "-h" => {
            print_agent_help();
            Ok(0)
        }
        _ => {
            print_agent_help();
            Ok(2)
        }
    }
}

fn agent_explain(args: &[String]) -> std::io::Result<i32> {
    let mut file = None;
    let mut agent = None;
    let mut json = false;
    let mut verbose = false;
    let mut target = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--file" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --file");
                    return Ok(2);
                };
                file = Some(value.clone());
                index += 2;
            }
            "--agent" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --agent");
                    return Ok(2);
                };
                agent = Some(value.clone());
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            "--format" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --format");
                    return Ok(2);
                };
                match value.as_str() {
                    "json" => json = true,
                    "text" => json = false,
                    other => {
                        eprintln!("invalid --format: {other} (expected text or json)");
                        return Ok(2);
                    }
                }
                index += 2;
            }
            "--verbose" | "-v" => {
                verbose = true;
                index += 1;
            }
            "help" | "--help" | "-h" => {
                eprintln!("usage: herdr agent explain <target> [--json|--verbose]");
                eprintln!(
                    "usage: herdr agent explain --file PATH --agent LABEL [--json|--verbose]"
                );
                return Ok(0);
            }
            value if value.starts_with('-') => {
                eprintln!("unknown option: {value}");
                return Ok(2);
            }
            value => {
                if target.is_some() {
                    eprintln!("usage: herdr agent explain <target> [--json]");
                    return Ok(2);
                }
                target = Some(value.to_string());
                index += 1;
            }
        }
    }

    let explain = if let Some(path) = file {
        if target.is_some() {
            eprintln!("usage: herdr agent explain --file PATH --agent LABEL [--json]");
            return Ok(2);
        }
        let Some(agent_label) = agent else {
            eprintln!("herdr agent explain --file requires --agent LABEL");
            return Ok(2);
        };
        let content = std::fs::read_to_string(path)?;
        crate::detect::manifest::explain_to_json_value(&crate::detect::manifest::explain_for_label(
            &agent_label,
            &content,
        ))
    } else {
        let Some(target) = target else {
            eprintln!("usage: herdr agent explain <target> [--json]");
            eprintln!("usage: herdr agent explain --file PATH --agent LABEL [--json]");
            return Ok(2);
        };
        if agent.is_some() {
            eprintln!("--agent is only valid with --file");
            return Ok(2);
        }

        let response = super::send_request(&Request {
            id: "cli:agent:explain".into(),
            method: Method::AgentExplain(AgentTarget {
                target: target.to_owned(),
            }),
        })?;
        if response.get("error").is_some() {
            eprintln!("{}", serde_json::to_string(&response).unwrap());
            return Ok(1);
        }
        response["result"]["explain"].clone()
    };

    if json {
        println!("{explain}");
    } else {
        print_agent_explain_text(&explain, verbose);
    }
    Ok(0)
}

fn print_agent_explain_text(explain: &serde_json::Value, verbose: bool) {
    println!("agent: {}", explain["agent"].as_str().unwrap_or("unknown"));
    println!("state: {}", explain["state"].as_str().unwrap_or("unknown"));
    println!(
        "manifest: {} {}",
        explain["manifest_source"].as_str().unwrap_or("none"),
        explain["manifest_version"].as_str().unwrap_or("unknown")
    );
    if let Some(rule) = explain["matched_rule"].as_object() {
        let rule_id = rule
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("-");
        println!(
            "rule: {} (region={} priority={})",
            rule_id,
            rule.get("region")
                .and_then(|value| value.as_str())
                .unwrap_or("-"),
            rule.get("priority")
                .and_then(|value| value.as_i64())
                .unwrap_or(0),
        );
        if let Some(preview) = matched_rule_region_preview(explain, rule_id) {
            println!("evidence: {preview:?}");
        }
    } else {
        println!("rule: none");
    }
    if let Some(reason) = explain["fallback_reason"].as_str() {
        println!("fallback_reason: {reason}");
    }
    if let Some(reason) = explain["screen_detection_skip_reason"].as_str() {
        println!("screen_detection_skip_reason: {reason}");
    }
    if let Some(reason) = explain["skipped_update_reason"].as_str() {
        println!("skipped_update_reason: {reason}");
    }
    if let Some(warning) = explain["warning"].as_str() {
        println!("warning: {warning}");
    }

    if !verbose {
        return;
    }

    println!(
        "visible: idle={} blocker={} working={}",
        explain["visible_idle"].as_bool().unwrap_or(false),
        explain["visible_blocker"].as_bool().unwrap_or(false),
        explain["visible_working"].as_bool().unwrap_or(false)
    );
    println!(
        "cached_remote_version: {}",
        explain["cached_remote_version"].as_str().unwrap_or("none")
    );
    println!(
        "local_override_shadowing_remote: {}",
        explain["local_override_shadowing_remote"]
            .as_bool()
            .unwrap_or(false)
    );
    if let Some(status) = explain["remote_update_status"].as_str() {
        println!("remote_update_status: {status}");
    }
    if let Some(error) = explain["remote_update_error"].as_str() {
        println!("remote_update_error: {error}");
    }
    if let Some(evaluated_rules) = explain["evaluated_rules"]
        .as_array()
        .filter(|rules| !rules.is_empty())
    {
        println!("evaluated_rules:");
        for rule in evaluated_rules {
            println!(
                "  {} {} priority={} region={} state={}",
                if rule["matched"].as_bool().unwrap_or(false) {
                    "✓"
                } else {
                    "✗"
                },
                rule["id"].as_str().unwrap_or("-"),
                rule["priority"].as_i64().unwrap_or(0),
                rule["region"].as_str().unwrap_or("-"),
                rule["state"].as_str().unwrap_or("unknown")
            );
            let evidence = &rule["evidence"];
            println!(
                "    matchers: contains={:?} regex={:?} line_regex={:?} all={} any={} not={}",
                evidence["contains"],
                evidence["regex"],
                evidence["line_regex"],
                evidence["all_count"].as_u64().unwrap_or(0),
                evidence["any_count"].as_u64().unwrap_or(0),
                evidence["not_count"].as_u64().unwrap_or(0)
            );
            println!(
                "    region: bytes={} preview={:?}",
                evidence["region_bytes"].as_u64().unwrap_or(0),
                evidence["region_preview"].as_str().unwrap_or("")
            );
        }
    }
}

fn matched_rule_region_preview<'a>(
    explain: &'a serde_json::Value,
    rule_id: &str,
) -> Option<&'a str> {
    explain["evaluated_rules"]
        .as_array()?
        .iter()
        .find(|rule| rule["id"].as_str() == Some(rule_id))?["evidence"]["region_preview"]
        .as_str()
        .filter(|preview| !preview.is_empty())
}

fn agent_start(args: &[String]) -> std::io::Result<i32> {
    let params = match parse_agent_start_args(args) {
        Ok(params) => params,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    if let Some(host_alias) = params.host.as_deref() {
        let host = match configured_remote_start_host(host_alias) {
            Ok(host) => host,
            Err(message) => {
                eprintln!("{message}");
                return Ok(1);
            }
        };
        let request = crate::app::remote_agent_start_request("cli:agent:start".into(), params);
        let response = crate::remote::send_remote_api_request_to_host(&host, &request)?;
        let response = crate::app::rewrite_remote_agent_start_response(
            &response,
            "cli:agent:start",
            &host.name,
        )?;
        let response: serde_json::Value =
            serde_json::from_str(&response).map_err(std::io::Error::other)?;
        return super::print_response(&response);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:start".into(),
        method: Method::AgentStart(params),
    })?)
}

fn parse_agent_start_args(args: &[String]) -> Result<AgentStartParams, String> {
    let Some(separator) = args.iter().position(|arg| arg == "--") else {
        return Err(AGENT_START_USAGE.to_string());
    };
    if separator == args.len() - 1 {
        return Err("agent start requires argv after --".to_string());
    }

    let mut host = None;
    let mut name = None;
    let mut cwd = None;
    let mut workspace_id = None;
    let mut tab_id = None;
    let mut split = None;
    let mut focus = false;
    let mut env = HashMap::new();

    let mut index = 0;
    while index < separator {
        match args[index].as_str() {
            "--host" => {
                let value = flag_value(args, index, separator, "--host")?;
                host = Some(value.clone());
                index += 2;
            }
            "--name" => {
                let value = flag_value(args, index, separator, "--name")?;
                set_agent_start_name(&mut name, value)?;
                index += 2;
            }
            "--cwd" => {
                let value = flag_value(args, index, separator, "--cwd")?;
                cwd = Some(value.clone());
                index += 2;
            }
            "--workspace" => {
                let value = flag_value(args, index, separator, "--workspace")?;
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--tab" => {
                let value = flag_value(args, index, separator, "--tab")?;
                tab_id = Some(super::normalize_tab_id(value));
                index += 2;
            }
            "--split" => {
                let value = flag_value(args, index, separator, "--split")?;
                split = Some(super::parse_split_direction(value).map_err(|err| err.to_string())?);
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
                let value = flag_value(args, index, separator, "--env")?;
                let (key, value) = super::parse_env_assignment(value)?;
                env.insert(key, value);
                index += 2;
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option: {other}"));
            }
            other => {
                set_agent_start_name(&mut name, other)?;
                index += 1;
            }
        }
    }

    let Some(name) = name else {
        return Err(AGENT_START_USAGE.to_string());
    };
    let new_workspace = host.is_some() && workspace_id.is_none() && tab_id.is_none();

    Ok(AgentStartParams {
        host,
        name,
        cwd,
        workspace_id,
        tab_id,
        split,
        focus,
        new_workspace,
        argv: args[separator + 1..].to_vec(),
        env,
    })
}

fn flag_value<'a>(
    args: &'a [String],
    index: usize,
    separator: usize,
    flag: &str,
) -> Result<&'a String, String> {
    args.get(index + 1)
        .filter(|_| index + 1 < separator)
        .ok_or_else(|| format!("missing value for {flag}"))
}

fn set_agent_start_name(name: &mut Option<String>, value: &str) -> Result<(), String> {
    if name.is_some() {
        return Err("agent start name specified more than once".to_string());
    }
    *name = Some(value.to_string());
    Ok(())
}

fn configured_remote_start_host(
    alias: &str,
) -> Result<crate::remote_target::RemoteHostConfig, String> {
    let loaded = crate::config::Config::load();
    configured_remote_start_host_from_config(&loaded.config, alias)
}

fn configured_remote_start_host_from_config(
    config: &crate::config::Config,
    alias: &str,
) -> Result<crate::remote_target::RemoteHostConfig, String> {
    if !config.remote.enabled {
        return Err("remote agent start requires remote.enabled = true".to_string());
    }
    let registry =
        crate::remote_target::RemoteHostRegistry::from_configs(config.remote.hosts.clone())
            .map_err(|err| format!("invalid remote host config: {err}"))?;
    let host = registry
        .get(alias)
        .ok_or_else(|| format!("unknown remote host: {alias}"))?;
    // A Manual host must never be reached implicitly by `agent start --host`:
    // fail locally before any SSH/API dispatch with a distinct policy error
    // (never the unknown-host / remote-disabled / connectivity errors). This
    // mirrors the app API `remote_host_connection_policy_manual` guard.
    if !host.connection_policy.allows_explicit_start() {
        return Err(format!(
            "remote host {}/{} has connection_policy = \"manual\"; `agent start --host {}` will not reach it implicitly (connect it explicitly first)",
            host.name, host.session, alias
        ));
    }
    Ok(host.clone())
}

fn agent_list(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr agent list");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:list".into(),
        method: Method::AgentList(EmptyParams::default()),
    })?)
}

fn agent_get(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent get <target>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr agent get <target>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:get".into(),
        method: Method::AgentGet(AgentTarget {
            target: target.clone(),
        }),
    })?)
}

fn agent_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent focus <target>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr agent focus <target>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:focus".into(),
        method: Method::AgentFocus(AgentTarget {
            target: target.clone(),
        }),
    })?)
}

fn agent_attach(args: &[String]) -> std::io::Result<i32> {
    let (target, takeover) =
        match super::parse_attach_target(args, "usage: herdr agent attach <target> [--takeover]") {
            Ok(parsed) => parsed,
            Err(code) => return Ok(code),
        };

    // Projection-derived `<host>/terminal:<id>` targets bypass `agent.get`: the
    // terminal id may not be an agent pane, and the authoritative remote attach
    // server validates it. Only the explicit `terminal:` form bypasses; agent
    // names/labels/legacy ids still resolve via `agent.get` unchanged.
    if let Some(terminal_id) = remote_terminal_attach_target(&target) {
        let Some(host) = configured_remote_attach_host(&target) else {
            eprintln!("agent attach failed: {target} is not a configured remote host");
            return Ok(1);
        };
        crate::remote::run_remote_terminal_attach(&host, terminal_id, takeover)?;
        return Ok(0);
    }

    let remote_host = configured_remote_attach_host(&target);

    let response = resolve_agent_target(&target, "cli:agent:attach:resolve")?;
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }
    let Some(terminal_id) = response["result"]["agent"]["terminal_id"].as_str() else {
        eprintln!("agent attach failed: response did not include terminal_id");
        return Ok(1);
    };
    if let Some(host) = remote_host {
        crate::remote::run_remote_terminal_attach(&host, terminal_id.to_owned(), takeover)?;
    } else {
        crate::client::run_terminal_attach(terminal_id.to_owned(), takeover)?;
    }
    Ok(0)
}

fn configured_remote_attach_host(target: &str) -> Option<crate::remote_target::RemoteHostConfig> {
    let loaded = crate::config::Config::load();
    configured_remote_attach_host_from_config(&loaded.config, target)
}

/// Detect the explicit `<host>/terminal:<id>` attach form used by projected
/// remote panes. Only the FIRST `/` is structural (the host qualifier); the
/// remainder is checked for the `terminal:` prefix and is not re-split. Returns
/// the terminal id when the target is in this form, or `None` for agent-name /
/// label / legacy-id / local targets, which must still resolve via `agent.get`.
fn remote_terminal_attach_target(target: &str) -> Option<String> {
    let (_host, remainder) = target.split_once('/')?;
    let id = remainder.strip_prefix("terminal:")?;
    if id.is_empty() {
        return None;
    }
    Some(id.to_string())
}

fn configured_remote_attach_host_from_config(
    config: &crate::config::Config,
    target: &str,
) -> Option<crate::remote_target::RemoteHostConfig> {
    if !config.remote.enabled {
        return None;
    }

    let registry =
        crate::remote_target::RemoteHostRegistry::from_configs(config.remote.hosts.clone()).ok()?;
    match crate::remote_target::plan_target_route(&registry, target).ok()? {
        crate::remote_target::PlannedTargetRoute::Remote { host, .. } => Some(host),
        crate::remote_target::PlannedTargetRoute::Local { .. } => None,
    }
}

fn agent_wait(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent wait <target> --status <idle|working|blocked|unknown> [--timeout MS]");
        return Ok(2);
    };

    let mut timeout_ms = None;
    let mut desired_status = None;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--status" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --status");
                    return Ok(2);
                };
                desired_status = Some(parse_agent_wait_status(value)?);
                index += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                timeout_ms = Some(super::parse_u64_flag("--timeout", value)?);
                index += 2;
            }
            "help" | "--help" | "-h" => {
                eprintln!("usage: herdr agent wait <target> --status <idle|working|blocked|unknown> [--timeout MS]");
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(agent_status) = desired_status else {
        eprintln!("missing required --status");
        return Ok(2);
    };

    let response = resolve_agent_target(target, "cli:agent:wait:resolve")?;
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }
    if response["result"]["agent"]["agent_status"]
        .as_str()
        .is_some_and(|current| agent_wait_status_satisfied(agent_status, current))
    {
        println!("{}", serde_json::to_string(&response).unwrap());
        return Ok(0);
    }

    let Some(pane_id) = response["result"]["agent"]["pane_id"].as_str() else {
        eprintln!("agent wait failed: response did not include pane_id");
        return Ok(1);
    };

    let subscriptions = if agent_status == AgentStatus::Idle {
        vec![
            Subscription::PaneAgentStatusChanged {
                pane_id: pane_id.to_owned(),
                agent_status: Some(AgentStatus::Idle),
            },
            Subscription::PaneAgentStatusChanged {
                pane_id: pane_id.to_owned(),
                agent_status: Some(AgentStatus::Done),
            },
        ]
    } else {
        vec![Subscription::PaneAgentStatusChanged {
            pane_id: pane_id.to_owned(),
            agent_status: Some(agent_status),
        }]
    };

    super::wait_for_agent_change(
        Request {
            id: "cli:agent:wait".into(),
            method: Method::EventsSubscribe(crate::api::schema::EventsSubscribeParams {
                subscriptions,
            }),
        },
        timeout_ms,
        "timed out waiting for agent status change",
    )
}

fn resolve_agent_target(target: &str, request_id: &str) -> std::io::Result<serde_json::Value> {
    super::send_request(&Request {
        id: request_id.into(),
        method: Method::AgentGet(AgentTarget {
            target: target.to_owned(),
        }),
    })
}

fn agent_rename(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent rename <target> <name>|--clear");
        return Ok(2);
    };
    if args.len() < 2 {
        eprintln!("usage: herdr agent rename <target> <name>|--clear");
        return Ok(2);
    }
    let name = if args.len() == 2 && args[1] == "--clear" {
        None
    } else {
        Some(args[1..].join(" "))
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:rename".into(),
        method: Method::AgentRename(AgentRenameParams {
            target: target.clone(),
            name,
        }),
    })?)
}

fn agent_send(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr agent send <target> <text>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:send".into(),
        method: Method::AgentSend(AgentSendParams {
            target: args[0].clone(),
            text: args[1..].join(" "),
        }),
    })?)
}

fn agent_submit(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr agent submit <target> <text>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:submit".into(),
        method: Method::AgentSubmit(AgentSubmitParams {
            target: args[0].clone(),
            text: args[1..].join(" "),
        }),
    })?)
}

fn agent_teardown(args: &[String]) -> std::io::Result<i32> {
    const USAGE: &str = "usage: herdr agent teardown <target> --confirm";
    let mut target = None;
    let mut confirm = false;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--confirm" => {
                confirm = true;
                index += 1;
            }
            "help" | "--help" | "-h" => {
                eprintln!("{USAGE}");
                eprintln!(
                    "  tears down the federation-placed agent/lane pane (destructive); --confirm is required"
                );
                return Ok(0);
            }
            value if value.starts_with('-') => {
                eprintln!("unknown option: {value}");
                return Ok(2);
            }
            value => {
                if target.is_some() {
                    eprintln!("{USAGE}");
                    return Ok(2);
                }
                target = Some(value.to_string());
                index += 1;
            }
        }
    }

    let Some(target) = target else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    // Reject before sending: the API enforces confirmation too, but the CLI
    // fails fast with a usage error so no destructive request is attempted.
    if !confirm {
        eprintln!("agent teardown is destructive; pass --confirm to proceed");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:teardown".into(),
        method: Method::AgentTeardown(AgentTeardownParams {
            target,
            confirm: true,
        }),
    })?)
}

fn agent_read(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent read <target> [--source visible|recent|recent-unwrapped] [--lines N] [--format text|ansi] [--ansi]");
        return Ok(2);
    };

    let mut source = ReadSource::Recent;
    let mut lines = None;
    let mut format = ReadFormat::Text;
    let mut strip_ansi = true;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = super::parse_read_source(value)?;
                index += 2;
            }
            "--lines" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --lines");
                    return Ok(2);
                };
                lines = Some(super::parse_u32_flag("--lines", value)?);
                index += 2;
            }
            "--format" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --format");
                    return Ok(2);
                };
                format = super::parse_read_format(value)?;
                strip_ansi = !matches!(format, ReadFormat::Ansi);
                index += 2;
            }
            "--ansi" => {
                format = ReadFormat::Ansi;
                strip_ansi = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:read".into(),
        method: Method::AgentRead(AgentReadParams {
            target: target.clone(),
            source,
            lines,
            format,
            strip_ansi,
        }),
    })?)
}

fn agent_wait_status_satisfied(desired: AgentStatus, current: &str) -> bool {
    match desired {
        AgentStatus::Idle => matches!(current, "idle" | "done"),
        AgentStatus::Working => current == "working",
        AgentStatus::Blocked => current == "blocked",
        AgentStatus::Unknown => current == "unknown",
        AgentStatus::Done => false,
    }
}

fn parse_agent_wait_status(value: &str) -> std::io::Result<AgentStatus> {
    match value {
        "idle" => Ok(AgentStatus::Idle),
        "working" => Ok(AgentStatus::Working),
        "blocked" => Ok(AgentStatus::Blocked),
        "unknown" => Ok(AgentStatus::Unknown),
        "done" => Err(std::io::Error::other(
            "done is a UI attention state; use idle for CLI agent completion waits",
        )),
        _ => Err(std::io::Error::other(format!(
            "invalid agent status: {value} (expected idle, working, blocked, or unknown)"
        ))),
    }
}

fn print_agent_help() {
    eprintln!("herdr agent commands:");
    eprintln!("  herdr agent list");
    eprintln!("  herdr agent get <target>");
    eprintln!("  herdr agent read <target> [--source visible|recent|recent-unwrapped] [--lines N] [--format text|ansi] [--ansi]");
    eprintln!("  herdr agent send <target> <text>");
    eprintln!("  herdr agent submit <target> <text>");
    eprintln!("  herdr agent rename <target> <name>|--clear");
    eprintln!("  herdr agent focus <target>");
    eprintln!("  herdr agent wait <target> --status <idle|working|blocked|unknown> [--timeout MS]");
    eprintln!("  herdr agent attach <target> [--takeover]");
    eprintln!("  herdr agent start <name> [--cwd PATH] [--workspace ID] [--tab ID] [--split right|down] [--env KEY=VALUE] [--focus|--no-focus] -- <argv...>");
    eprintln!("  herdr agent start --host HOST --name NAME [--cwd REMOTE_PATH] [--workspace ID] [--tab ID] [--split right|down] [--env KEY=VALUE] [--focus|--no-focus] -- <argv...>");
    eprintln!("  herdr agent teardown <target> --confirm  (destructive; closes the pane hosting a federation-placed agent/lane)");
    eprintln!("  herdr agent explain <target> [--json]");
    eprintln!("  herdr agent explain --file PATH --agent LABEL [--json]");
    eprintln!("  targets accept terminal ids, unique agent names, detected/reported agent labels, and legacy pane ids");
    eprintln!(
        "  agent send writes literal text; agent submit writes text plus Enter for composer-style prompts"
    );
    eprintln!("  use pane run when you want command text plus Enter on a plain shell");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote_config(enabled: bool) -> crate::config::Config {
        let mut config = crate::config::Config::default();
        config.remote.enabled = enabled;
        config.remote.hosts = vec![crate::remote_target::RemoteHostConfig::new(
            "jafar",
            "user@jafar:2222",
            "fed-agents",
            true,
        )];
        config
    }

    #[test]
    fn attach_route_keeps_bare_targets_local() {
        let config = remote_config(true);

        assert_eq!(
            configured_remote_attach_host_from_config(&config, "codex"),
            None
        );
    }

    #[test]
    fn attach_route_resolves_configured_host() {
        let config = remote_config(true);

        let host = configured_remote_attach_host_from_config(&config, "jafar/codex")
            .expect("configured remote host");

        assert_eq!(host.name, "jafar");
        assert_eq!(host.target, "user@jafar:2222");
        assert_eq!(host.session, "fed-agents");
    }

    #[test]
    fn attach_route_respects_remote_enabled_flag() {
        let config = remote_config(false);

        assert_eq!(
            configured_remote_attach_host_from_config(&config, "jafar/codex"),
            None
        );
    }

    #[test]
    fn attach_route_ignores_unconfigured_hosts() {
        let config = remote_config(true);

        assert_eq!(
            configured_remote_attach_host_from_config(&config, "other/codex"),
            None
        );
    }

    #[test]
    fn agent_start_parse_legacy_local_form() {
        let args = vec![
            "codex".to_string(),
            "--cwd".to_string(),
            "/tmp/project".to_string(),
            "--focus".to_string(),
            "--".to_string(),
            "codex".to_string(),
            "--ask".to_string(),
        ];

        let params = parse_agent_start_args(&args).unwrap();

        assert_eq!(params.host, None);
        assert_eq!(params.name, "codex");
        assert_eq!(params.cwd.as_deref(), Some("/tmp/project"));
        assert_eq!(params.workspace_id, None);
        assert_eq!(params.tab_id, None);
        assert!(params.focus);
        assert!(!params.new_workspace);
        assert_eq!(params.argv, vec!["codex", "--ask"]);
    }

    #[test]
    fn agent_start_parse_env_values() {
        let args = vec![
            "codex".to_string(),
            "--env".to_string(),
            "HERDR_MODE=review".to_string(),
            "--env".to_string(),
            "TRACE=1".to_string(),
            "--".to_string(),
            "codex".to_string(),
        ];

        let params = parse_agent_start_args(&args).unwrap();

        assert_eq!(params.host, None);
        assert!(!params.new_workspace);
        assert_eq!(
            params.env.get("HERDR_MODE").map(String::as_str),
            Some("review")
        );
        assert_eq!(params.env.get("TRACE").map(String::as_str), Some("1"));
    }

    #[test]
    fn agent_start_parse_remote_host_name_defaults_to_new_workspace() {
        let args = vec![
            "--host".to_string(),
            "jafar".to_string(),
            "--name".to_string(),
            "codex".to_string(),
            "--cwd".to_string(),
            "/remote/project".to_string(),
            "--".to_string(),
            "codex".to_string(),
        ];

        let params = parse_agent_start_args(&args).unwrap();

        assert_eq!(params.host.as_deref(), Some("jafar"));
        assert_eq!(params.name, "codex");
        assert_eq!(params.cwd.as_deref(), Some("/remote/project"));
        assert_eq!(params.workspace_id, None);
        assert_eq!(params.tab_id, None);
        assert!(params.new_workspace);
        assert_eq!(params.argv, vec!["codex"]);
    }

    #[test]
    fn agent_start_parse_remote_host_positional_name_with_placement_does_not_force_workspace() {
        let args = vec![
            "--host".to_string(),
            "jafar".to_string(),
            "codex".to_string(),
            "--workspace".to_string(),
            "remote-ws".to_string(),
            "--".to_string(),
            "codex".to_string(),
        ];

        let params = parse_agent_start_args(&args).unwrap();

        assert_eq!(params.host.as_deref(), Some("jafar"));
        assert_eq!(params.name, "codex");
        assert_eq!(params.workspace_id.as_deref(), Some("remote-ws"));
        assert!(!params.new_workspace);
    }

    #[test]
    fn agent_start_remote_host_config_resolves_configured_alias() {
        let config = remote_config(true);

        let host = configured_remote_start_host_from_config(&config, "jafar")
            .expect("configured start host");

        assert_eq!(host.name, "jafar");
        assert_eq!(host.target, "user@jafar:2222");
        assert_eq!(host.session, "fed-agents");
    }

    #[test]
    fn agent_start_remote_host_config_errors_for_unknown_alias() {
        let config = remote_config(true);

        let err = configured_remote_start_host_from_config(&config, "other").unwrap_err();

        assert_eq!(err, "unknown remote host: other");
    }

    #[test]
    fn agent_start_remote_host_config_errors_when_remote_disabled() {
        let config = remote_config(false);

        let err = configured_remote_start_host_from_config(&config, "jafar").unwrap_err();

        assert_eq!(err, "remote agent start requires remote.enabled = true");
    }

    #[test]
    fn agent_start_remote_host_config_errors_for_manual_policy_before_dispatch() {
        // A Manual host must fail locally before any SSH/API dispatch. Testing
        // the pure config resolver proves no dispatch path is reached, with a
        // distinct policy error (never the unknown-host / remote-disabled /
        // connectivity messages). Mirrors the app API
        // `remote_host_connection_policy_manual` guard.
        let mut config = crate::config::Config::default();
        config.remote.enabled = true;
        config.remote.hosts = vec![crate::remote_target::RemoteHostConfig::new(
            "jafar",
            "user@jafar:2222",
            "fed-agents",
            true,
        )
        .with_connection_policy(crate::remote_target::RemoteConnectionPolicy::Manual)];

        let err = configured_remote_start_host_from_config(&config, "jafar").unwrap_err();

        assert!(err.contains("connection_policy = \"manual\""));
        assert!(err.contains("jafar/fed-agents"));
        assert!(err.contains("agent start --host jafar"));
        assert!(!err.starts_with("unknown remote host"));
        assert_ne!(err, "remote agent start requires remote.enabled = true");
    }

    #[test]
    fn agent_start_remote_host_config_resolves_configured_connect_timeout() {
        // `agent start --host` dispatches through `send_remote_api_request_to_host`
        // using this resolved host, so a custom bounded connect timeout must
        // survive alias resolution (on-demand hosts still dispatch, now bounded).
        let mut config = crate::config::Config::default();
        config.remote.enabled = true;
        config.remote.hosts = vec![crate::remote_target::RemoteHostConfig::new(
            "jafar",
            "user@jafar:2222",
            "fed-agents",
            false,
        )];
        config.remote.hosts[0].connect_timeout_secs = 25;

        let host = configured_remote_start_host_from_config(&config, "jafar")
            .expect("configured start host");

        assert_eq!(host.connect_timeout_secs, 25);
        assert!(!host.connection_policy.starts_automatically());
    }

    #[test]
    fn agent_submit_requires_target_and_text() {
        // Insufficient args must surface usage before any request is sent.
        let code = agent_submit(&[]).unwrap();
        assert_eq!(code, 2);
    }

    #[test]
    fn remote_terminal_attach_target_detects_explicit_form() {
        // `<host>/terminal:<id>` → Some(id).
        assert_eq!(
            remote_terminal_attach_target("jafar/terminal:abc123"),
            Some("abc123".to_string())
        );
        // Nested slash in id: only the FIRST slash is structural.
        assert_eq!(
            remote_terminal_attach_target("jafar/terminal:abc/suffix"),
            Some("abc/suffix".to_string())
        );
    }

    #[test]
    fn remote_terminal_attach_target_rejects_non_terminal_forms() {
        // Agent name target → None (must still resolve via agent.get).
        assert_eq!(remote_terminal_attach_target("codex"), None);
        // Host-qualified agent name → None.
        assert_eq!(remote_terminal_attach_target("jafar/codex"), None);
        // Empty terminal id → None.
        assert_eq!(remote_terminal_attach_target("jafar/terminal:"), None);
        // No slash → None.
        assert_eq!(remote_terminal_attach_target("terminal:abc"), None);
    }
}
