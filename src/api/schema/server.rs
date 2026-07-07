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
    pub const AGENT_SUBMIT: &'static str = "agent_submit";
    pub const AGENT_FOCUS: &'static str = "agent_focus";
    pub const AGENT_START: &'static str = "agent_start";
    pub const AGENT_TEARDOWN: &'static str = "agent_teardown";
    pub const WORKSPACE_RENAME: &'static str = "workspace_rename";
    pub const TAB_RENAME: &'static str = "tab_rename";
    pub const PANE_SPLIT: &'static str = "pane_split";
    pub const PANE_CLOSE: &'static str = "pane_close";
    pub const PANE_RENAME: &'static str = "pane_rename";
    pub const PANE_FOCUS: &'static str = "pane_focus";
    pub const PANE_FOCUS_DIRECTION: &'static str = "pane_focus_direction";
    pub const TAB_LIST: &'static str = "tab_list";
    pub const TAB_CREATE: &'static str = "tab_create";
    pub const TAB_FOCUS: &'static str = "tab_focus";
    pub const TAB_CLOSE: &'static str = "tab_close";
    pub const LAYOUT_EXPORT: &'static str = "layout_export";
    pub const TERMINAL_ATTACH: &'static str = "terminal_attach";

    pub fn current() -> Self {
        Self {
            methods: [
                Self::REMOTE_API_BRIDGE,
                Self::WORKSPACE_CREATE,
                Self::WORKSPACE_LIST_LOCAL,
                Self::WORKSPACE_RENAME,
                Self::AGENT_LIST,
                Self::AGENT_LIST_LOCAL,
                Self::AGENT_GET,
                Self::AGENT_READ,
                Self::AGENT_SEND,
                Self::AGENT_SUBMIT,
                Self::AGENT_FOCUS,
                Self::AGENT_START,
                Self::AGENT_TEARDOWN,
                Self::PANE_SPLIT,
                Self::PANE_CLOSE,
                Self::PANE_RENAME,
                Self::PANE_FOCUS,
                Self::PANE_FOCUS_DIRECTION,
                Self::TAB_LIST,
                Self::TAB_CREATE,
                Self::TAB_FOCUS,
                Self::TAB_CLOSE,
                Self::TAB_RENAME,
                Self::LAYOUT_EXPORT,
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
