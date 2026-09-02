//! Internal app events delivered via channel.
//!
//! Background tasks (PTY child watchers, future hook listeners, etc.) send
//! events to the main loop through this channel. No polling needed.

use std::time::Instant;

use crate::api::schema::{AgentInfo, WorkspaceInfo};
use crate::detect::{Agent, AgentState};
use crate::layout::PaneId;
use crate::remote_source::{
    RemoteAgentKey, RemoteConnectionStatus, RemoteHostKey, RemoteSourceCapabilities,
};
use crate::workspace::{GitStatusCacheEntry, WorkspaceGitStatus};

#[derive(Debug)]
pub struct ApiWorktreeAddRequest {
    pub id: String,
    pub operation_id: u64,
    pub checkout_key: std::path::PathBuf,
    pub source_workspace_id: Option<String>,
    pub source_existing_membership: Option<crate::workspace::WorktreeSpaceMembership>,
    pub source_checkout_path: std::path::PathBuf,
    pub source_repo_root: std::path::PathBuf,
    pub repo_key: String,
    pub repo_name: String,
    pub label: Option<String>,
    pub focus: bool,
    pub respond_to: std::sync::mpsc::Sender<String>,
}

#[derive(Debug)]
pub struct WorktreeAddResult {
    pub path: std::path::PathBuf,
    pub api_request: Option<ApiWorktreeAddRequest>,
    pub result: Result<(), String>,
}

#[derive(Debug)]
pub struct ApiWorktreeRemoveRequest {
    pub id: String,
    pub operation_id: u64,
    pub checkout_key: std::path::PathBuf,
    pub respond_to: std::sync::mpsc::Sender<String>,
}

#[derive(Debug)]
pub struct WorktreeRemoveResult {
    pub workspace_id: String,
    pub path: std::path::PathBuf,
    pub workspace: Option<Box<crate::api::schema::WorkspaceInfo>>,
    pub worktree: Option<Box<crate::api::schema::WorktreeInfo>>,
    pub forced: bool,
    pub api_request: Option<ApiWorktreeRemoveRequest>,
    pub result: Result<(), String>,
}

/// An event from a background task to the main loop.
#[derive(Debug)]
pub enum AppEvent {
    /// A pane's child process exited.
    PaneDied { pane_id: PaneId },
    /// A pane runtime's child process exited, with the runtime token that sent it.
    PaneRuntimeDied { pane_id: PaneId, runtime_token: u64 },
    /// Fallback detector state changed in a pane.
    StateChanged {
        pane_id: PaneId,
        agent: Option<Agent>,
        state: AgentState,
        visible_blocker: bool,
        visible_working: bool,
        process_exited: bool,
        observed_at: Instant,
    },
    /// Hook-authoritative agent state was reported for a pane.
    HookStateReported {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        state: AgentState,
        message: Option<String>,
        custom_status: Option<String>,
        seq: Option<u64>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
    },
    /// Agent session identity was reported without state authority.
    AgentSessionReported {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        seq: Option<u64>,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
        session_start_source: Option<String>,
    },
    /// Display-only agent metadata was reported for a pane.
    HookMetadataReported {
        pane_id: PaneId,
        source: String,
        agent_label: Option<String>,
        applies_to_source: Option<String>,
        title: Option<String>,
        display_agent: Option<String>,
        custom_status: Option<String>,
        state_labels: std::collections::HashMap<String, String>,
        clear_title: bool,
        clear_display_agent: bool,
        clear_custom_status: bool,
        clear_state_labels: bool,
        seq: Option<u64>,
        ttl: Option<std::time::Duration>,
    },
    /// Hook authority was explicitly cleared for a pane.
    HookAuthorityCleared {
        pane_id: PaneId,
        source: Option<String>,
        seq: Option<u64>,
    },
    /// The current detected agent gracefully released this pane back to the shell.
    HookAgentReleased {
        pane_id: PaneId,
        source: String,
        agent_label: String,
        known_agent: Option<Agent>,
        seq: Option<u64>,
    },
    /// A new version is available through the active installation manager.
    UpdateReady {
        version: String,
        install_command: String,
    },
    /// A connected authoritative remote host/session reported a full agent snapshot.
    RemoteSourceSnapshot {
        host: RemoteHostKey,
        /// Supervisor incarnation that produced this snapshot. The App accepts a
        /// remote-source event only when the host/session still matches the
        /// loaded registry and the currently active keyed handle carries this
        /// exact generation; a retired predecessor's queued/late event is
        /// rejected so a same-host reconnect cannot admit stale data.
        generation: u64,
        agents: Vec<AgentInfo>,
        workspaces: Option<Vec<WorkspaceInfo>>,
        capabilities: RemoteSourceCapabilities,
    },
    /// A connected authoritative remote host/session reported one newer agent entry.
    #[allow(dead_code)]
    // Staged ingress for future remote supervisors; reducer/tests exercise it before runtime sender exists.
    RemoteSourceAgentUpdated {
        host: RemoteHostKey,
        agent: Box<AgentInfo>,
    },
    /// A connected authoritative remote host/session reported one agent gone.
    #[allow(dead_code)]
    // Staged ingress for future remote supervisors; reducer/tests exercise it before runtime sender exists.
    RemoteSourceAgentRemoved { key: RemoteAgentKey },
    /// A remote host/session became unreachable or incompatible; keep last-known agents stale.
    RemoteSourceDisconnected {
        host: RemoteHostKey,
        /// Supervisor incarnation that produced this status; see
        /// [`AppEvent::RemoteSourceSnapshot`] generation filtering.
        generation: u64,
        status: RemoteConnectionStatus,
    },
    /// A connected authoritative remote host/session published prepared bridge
    /// state (prepared remote Herdr shell path plus advertised federation
    /// capabilities) captured from a successful supervisor ping. Routed agent
    /// dispatch may reuse this state to skip per-request remote binary
    /// preparation and capability/ping probes while the host stays `Connected`;
    /// it is invalidated when the host becomes non-connected. This is data reuse,
    /// not connection pooling.
    RemoteSourceBridgeState {
        host: RemoteHostKey,
        /// Supervisor incarnation that captured this prepared state; see
        /// [`AppEvent::RemoteSourceSnapshot`] generation filtering.
        generation: u64,
        bridge_state: crate::remote::RemoteApiBridgeState,
    },
    /// A remote host/session was removed from aggregation state.
    RemoteSourceRemoved { host: RemoteHostKey },
    /// A deferred runtime lifecycle attempt (connect/reconnect initial ping)
    /// finished for one supervisor incarnation. Tagged with the exact
    /// generation/epoch the App installed; if that generation is no longer the
    /// active one (superseded by reconnect/disconnect/config reload), the App
    /// already resolved its pending responder with a deterministic superseded
    /// error and ignores this event. Otherwise the App resolves the pending
    /// responder with the resulting LOCAL aggregation status. This event never
    /// reaches the pure [`AppState`] reducer: lifecycle completion is App-only
    /// bookkeeping that drives the pending `respond_to` channel.
    RemoteSourceLifecycleAttempt {
        host: RemoteHostKey,
        generation: u64,
        outcome: crate::remote_supervisor::RemoteSourceLifecycleOutcome,
    },
    /// A deferred persistent-bridge pool cleanup (disconnect) finished reaping
    /// one host's idle bridges off-loop. Tagged with the lifecycle generation
    /// the App installed when it advanced the pool generation; if superseded,
    /// the pending responder was already resolved and this event is ignored.
    /// Otherwise the App resolves the disconnect pending responder with
    /// success. App-only bookkeeping; never reaches the pure reducer.
    RemoteSourcePoolDrainCompleted {
        host: RemoteHostKey,
        generation: u64,
    },
    /// Wake hint for the App-owned host-glass worker mailbox. Frame bytes and
    /// lifecycle truth never travel through this bounded event queue; the App
    /// drains their single ordered, lossless mailbox before handling any event.
    #[cfg_attr(windows, allow(dead_code))]
    HostGlassWake,
    /// Remote agent detection manifest update check finished.
    AgentDetectionManifestsUpdated {
        updated: Vec<crate::detect::manifest_update::ManifestUpdateCommit>,
        status: crate::detect::manifest_update::ManifestUpdateStatus,
    },
    /// A pane child emitted a valid OSC 52 clipboard write. The main loop
    /// re-emits it through herdr's own clipboard writer.
    ClipboardWrite { content: Vec<u8> },
    /// The current host-glass connection emitted a valid OSC 52 clipboard
    /// write. Keep its non-secret authority tags until the main-loop side
    /// effect so selection, generation, or connection retirement can revoke
    /// an already-queued write.
    #[cfg_attr(windows, allow(dead_code))]
    // Windows keeps the shared event taxonomy, but host-glass streams are Unix-only.
    HostGlassClipboardWrite {
        host: RemoteHostKey,
        generation: u64,
        connection: u64,
        content: Vec<u8>,
    },
    /// A pane child reported its shell current directory through terminal
    /// metadata such as OSC 7.
    TerminalCwdReported {
        pane_id: PaneId,
        cwd: std::path::PathBuf,
    },
    /// Background git status refresh completed for workspaces.
    GitStatusRefreshed {
        results: Vec<WorkspaceGitStatus>,
        cache_updates: Vec<(std::path::PathBuf, GitStatusCacheEntry)>,
    },
    /// A plugin action or event command finished.
    PluginCommandFinished {
        log_id: String,
        finished_unix_ms: u64,
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
        error: Option<String>,
    },
    /// Background `git worktree add` completed.
    WorktreeAddFinished(Box<WorktreeAddResult>),
    /// Background `git worktree remove` completed.
    WorktreeRemoveFinished(Box<WorktreeRemoveResult>),
}
