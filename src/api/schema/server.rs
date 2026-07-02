use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
pub struct PingParams {}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerLiveHandoffParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_exe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerCapabilities {
    pub live_handoff: bool,
    #[serde(default)]
    pub detached_server_daemon: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub federation: Option<FederationCapabilities>,
}

impl ServerCapabilities {
    pub fn current() -> Self {
        Self {
            live_handoff: true,
            detached_server_daemon: true,
            federation: Some(FederationCapabilities::current()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FederationCapabilities {
    #[serde(default)]
    pub methods: Vec<String>,
}

impl FederationCapabilities {
    pub const REMOTE_API_BRIDGE: &'static str = "remote_api_bridge";
    pub const WORKSPACE_CREATE: &'static str = "workspace_create";
    pub const WORKSPACE_LIST_LOCAL: &'static str = "workspace_list_local";
    pub const AGENT_LIST: &'static str = "agent_list";
    pub const AGENT_LIST_LOCAL: &'static str = "agent_list_local";
    pub const AGENT_GET: &'static str = "agent_get";
    pub const AGENT_READ: &'static str = "agent_read";
    pub const AGENT_SEND: &'static str = "agent_send";
    pub const AGENT_FOCUS: &'static str = "agent_focus";
    pub const AGENT_START: &'static str = "agent_start";
    pub const TERMINAL_ATTACH: &'static str = "terminal_attach";

    pub fn current() -> Self {
        Self {
            methods: [
                Self::REMOTE_API_BRIDGE,
                Self::WORKSPACE_CREATE,
                Self::WORKSPACE_LIST_LOCAL,
                Self::AGENT_LIST,
                Self::AGENT_LIST_LOCAL,
                Self::AGENT_GET,
                Self::AGENT_READ,
                Self::AGENT_SEND,
                Self::AGENT_FOCUS,
                Self::AGENT_START,
                Self::TERMINAL_ATTACH,
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
        }
    }

    pub fn supports_method(&self, method: &str) -> bool {
        self.methods.iter().any(|candidate| candidate == method)
    }
}
