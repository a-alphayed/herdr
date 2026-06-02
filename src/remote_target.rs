//! Pure parsing for future host-qualified remote target routing.
//!
//! This module only classifies target strings. It does not consult remote
//! config, inspect caches, open bridges, or route commands.

#[allow(dead_code)] // Staged validation error for future remote config/target routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteAliasError {
    Empty,
    ContainsSlash,
    ContainsColon,
    StartsWithDash,
}

impl std::fmt::Display for RemoteAliasError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "remote alias cannot be empty"),
            Self::ContainsSlash => write!(f, "remote alias cannot contain '/'"),
            Self::ContainsColon => write!(f, "remote alias cannot contain ':'"),
            Self::StartsWithDash => write!(f, "remote alias cannot start with '-'"),
        }
    }
}

impl std::error::Error for RemoteAliasError {}

#[allow(dead_code)] // Staged for future remote config and host-qualified target parsing.
pub(crate) fn validate_remote_alias(alias: &str) -> Result<&str, RemoteAliasError> {
    if alias.is_empty() {
        return Err(RemoteAliasError::Empty);
    }
    if alias.contains('/') {
        return Err(RemoteAliasError::ContainsSlash);
    }
    if alias.contains(':') {
        return Err(RemoteAliasError::ContainsColon);
    }
    if alias.starts_with('-') {
        return Err(RemoteAliasError::StartsWithDash);
    }
    Ok(alias)
}

#[allow(dead_code)] // Staged parse error for future cross-host command routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TargetRouteParseError {
    InvalidAlias(RemoteAliasError),
    EmptyRemoteTarget,
}

impl std::fmt::Display for TargetRouteParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidAlias(err) => write!(f, "{err}"),
            Self::EmptyRemoteTarget => write!(f, "remote target cannot be empty"),
        }
    }
}

impl std::error::Error for TargetRouteParseError {}

impl From<RemoteAliasError> for TargetRouteParseError {
    fn from(err: RemoteAliasError) -> Self {
        Self::InvalidAlias(err)
    }
}

#[allow(dead_code)] // Staged parser output for future cross-host command routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TargetRoute {
    Local {
        target: String,
    },
    Remote {
        host: String,
        target: RemoteTargetSelector,
    },
}

#[allow(dead_code)] // Staged selector for future cross-host command routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoteTargetSelector {
    Agent(String),
    Pane(String),
    Terminal(String),
    Workspace(String),
}

#[allow(dead_code)] // Staged parser entrypoint for future cross-host command routing.
pub(crate) fn parse_target_route(target: &str) -> Result<TargetRoute, TargetRouteParseError> {
    let Some((host, remote_target)) = target.split_once('/') else {
        return Ok(TargetRoute::Local {
            target: target.to_string(),
        });
    };

    validate_remote_alias(host)?;
    if remote_target.is_empty() {
        return Err(TargetRouteParseError::EmptyRemoteTarget);
    }

    Ok(TargetRoute::Remote {
        host: host.to_string(),
        target: parse_remote_target_selector(remote_target)?,
    })
}

#[allow(dead_code)] // Staged selector parser for future cross-host command routing.
pub(crate) fn parse_remote_target_selector(
    target: &str,
) -> Result<RemoteTargetSelector, TargetRouteParseError> {
    for (prefix, constructor) in [
        (
            "pane:",
            RemoteTargetSelector::Pane as fn(String) -> RemoteTargetSelector,
        ),
        ("terminal:", RemoteTargetSelector::Terminal),
        ("workspace:", RemoteTargetSelector::Workspace),
    ] {
        if let Some(value) = target.strip_prefix(prefix) {
            if value.is_empty() {
                return Err(TargetRouteParseError::EmptyRemoteTarget);
            }
            return Ok(constructor(value.to_string()));
        }
    }

    Ok(RemoteTargetSelector::Agent(target.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_target_accepts_valid_aliases() {
        for alias in ["jafar", "work-mini", "host_1", "host.example"] {
            assert_eq!(validate_remote_alias(alias), Ok(alias));
        }
    }

    #[test]
    fn remote_target_rejects_invalid_aliases() {
        assert_eq!(validate_remote_alias(""), Err(RemoteAliasError::Empty));
        assert_eq!(
            validate_remote_alias("ja/far"),
            Err(RemoteAliasError::ContainsSlash)
        );
        assert_eq!(
            validate_remote_alias("ja:far"),
            Err(RemoteAliasError::ContainsColon)
        );
        assert_eq!(
            validate_remote_alias("-jafar"),
            Err(RemoteAliasError::StartsWithDash)
        );
    }

    #[test]
    fn remote_target_bare_target_is_local_only() {
        assert_eq!(
            parse_target_route("codex").unwrap(),
            TargetRoute::Local {
                target: "codex".to_string(),
            }
        );
    }

    #[test]
    fn remote_target_bare_typed_handle_lookalikes_are_local_only() {
        for target in ["pane:1-1", "terminal:term_abc", "workspace:w1"] {
            assert_eq!(
                parse_target_route(target).unwrap(),
                TargetRoute::Local {
                    target: target.to_string(),
                }
            );
        }
    }

    #[test]
    fn remote_target_splits_host_on_first_slash_and_preserves_remainder() {
        assert_eq!(
            parse_target_route("jafar/codex/review").unwrap(),
            TargetRoute::Remote {
                host: "jafar".to_string(),
                target: RemoteTargetSelector::Agent("codex/review".to_string()),
            }
        );
    }

    #[test]
    fn remote_target_rejects_empty_host_or_target() {
        assert_eq!(
            parse_target_route("/codex").unwrap_err(),
            TargetRouteParseError::InvalidAlias(RemoteAliasError::Empty)
        );
        assert_eq!(
            parse_target_route("jafar/").unwrap_err(),
            TargetRouteParseError::EmptyRemoteTarget
        );
    }

    #[test]
    fn remote_target_rejects_colon_in_alias_but_preserves_colon_in_target() {
        assert_eq!(
            parse_target_route("ja:far/codex").unwrap_err(),
            TargetRouteParseError::InvalidAlias(RemoteAliasError::ContainsColon)
        );
        assert_eq!(
            parse_target_route("jafar/codex:review").unwrap(),
            TargetRoute::Remote {
                host: "jafar".to_string(),
                target: RemoteTargetSelector::Agent("codex:review".to_string()),
            }
        );
    }

    #[test]
    fn remote_target_parses_typed_handles() {
        assert_eq!(
            parse_target_route("jafar/pane:1-1").unwrap(),
            TargetRoute::Remote {
                host: "jafar".to_string(),
                target: RemoteTargetSelector::Pane("1-1".to_string()),
            }
        );
        assert_eq!(
            parse_target_route("jafar/terminal:term_abc").unwrap(),
            TargetRoute::Remote {
                host: "jafar".to_string(),
                target: RemoteTargetSelector::Terminal("term_abc".to_string()),
            }
        );
        assert_eq!(
            parse_target_route("jafar/workspace:w1").unwrap(),
            TargetRoute::Remote {
                host: "jafar".to_string(),
                target: RemoteTargetSelector::Workspace("w1".to_string()),
            }
        );
    }

    #[test]
    fn remote_target_rejects_empty_typed_handle_payloads() {
        for target in ["jafar/pane:", "jafar/terminal:", "jafar/workspace:"] {
            assert_eq!(
                parse_target_route(target).unwrap_err(),
                TargetRouteParseError::EmptyRemoteTarget
            );
        }
    }

    #[test]
    fn remote_target_unknown_colon_prefix_remains_agent_target() {
        assert_eq!(
            parse_target_route("jafar/foo:bar").unwrap(),
            TargetRoute::Remote {
                host: "jafar".to_string(),
                target: RemoteTargetSelector::Agent("foo:bar".to_string()),
            }
        );
    }

    #[test]
    fn remote_target_agent_labels_may_contain_slash_and_colon_after_first_slash() {
        assert_eq!(
            parse_target_route("jafar/team/codex:review/1").unwrap(),
            TargetRoute::Remote {
                host: "jafar".to_string(),
                target: RemoteTargetSelector::Agent("team/codex:review/1".to_string()),
            }
        );
    }
}
