//! Runtime registry for in-place remote terminal-session projection streams.
//!
//! `AppState` remains pure: only generation-tagged frame/status data enters it.
//! SSH bridge listeners, accepted stream workers, sockets and writer handles
//! live here on `App`. One selected host owns one bounded `SshStdioBridge`
//! listener; every exported layout leaf opens one independent 1:1 client stream
//! through it (focused leaf = `ControlTerminal { takeover: false }`, all others
//! = `ObserveTerminal`).

#[cfg(unix)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
#[cfg(unix)]
use std::thread::JoinHandle;

use crate::app::state::AppState;
use crate::events::AppEvent;
use crate::remote_source::{
    RemoteHostKey, RemoteProjectionStatus, RemoteProjectionStreamRole,
    RemoteProjectionStreamStatus, RemoteProjectionTerminalKey, RemoteSpaceKey,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct DesiredStream {
    key: RemoteProjectionTerminalKey,
    role: RemoteProjectionStreamRole,
    cols: u16,
    rows: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectionSignature {
    source: RemoteSpaceKey,
    prepared_shell_path: String,
    streams: Vec<DesiredStream>,
}

impl ProjectionSignature {
    fn same_stream_identity(&self, other: &Self) -> bool {
        self.source == other.source
            && self.prepared_shell_path == other.prepared_shell_path
            && self.streams.len() == other.streams.len()
            && self
                .streams
                .iter()
                .zip(&other.streams)
                .all(|(left, right)| left.key == right.key && left.role == right.role)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProjectionAdmission {
    Inactive,
    Waiting {
        source: RemoteSpaceKey,
        streams: Vec<(RemoteProjectionTerminalKey, RemoteProjectionStreamRole)>,
        status: RemoteProjectionStreamStatus,
        message: String,
        preserve_last_known: bool,
    },
    Ready {
        source: RemoteSpaceKey,
        prepared: crate::remote::RemoteApiBridgeState,
        streams: Vec<DesiredStream>,
    },
}

fn layout_leaves<'a>(
    node: &'a crate::api::schema::LayoutNode,
    out: &mut Vec<&'a crate::api::schema::LayoutPane>,
) {
    match node {
        crate::api::schema::LayoutNode::Pane { pane } => out.push(pane),
        crate::api::schema::LayoutNode::Split { first, second, .. } => {
            layout_leaves(first, out);
            layout_leaves(second, out);
        }
    }
}

/// Pure admission planner. It performs every fail-closed check before any
/// bridge listener or per-pane stream is opened: exact selected source,
/// connected/fresh projection, additive `terminal_session_stream` capability,
/// prepared no-start bridge state, one unique terminal id per exported leaf,
/// and the authoritative 24-pane cap. Geometry must also exist for every leaf
/// so the controller never resizes an authoritative remote PTY from a guessed
/// local size.
fn plan_projection_admission(state: &AppState) -> ProjectionAdmission {
    let Some(source) = state.selected_remote_space.clone() else {
        return ProjectionAdmission::Inactive;
    };
    let host = RemoteHostKey::new(source.host.clone(), source.session.clone());
    let projection = state.remote_sources.projection_for_space(&source);
    let mut leaf_roles = Vec::new();

    let Some(projection) = projection else {
        return ProjectionAdmission::Waiting {
            source,
            streams: leaf_roles,
            status: RemoteProjectionStreamStatus::Connecting,
            message: "waiting for authoritative remote layout".into(),
            preserve_last_known: true,
        };
    };
    let Some(layout) = projection.layout.as_ref() else {
        return ProjectionAdmission::Waiting {
            source,
            streams: leaf_roles,
            status: match projection.status {
                RemoteProjectionStatus::StaleLastKnown => {
                    RemoteProjectionStreamStatus::StaleLastKnown
                }
                _ => RemoteProjectionStreamStatus::NeedsAttention,
            },
            message: "authoritative remote layout unavailable".into(),
            preserve_last_known: projection.status == RemoteProjectionStatus::StaleLastKnown,
        };
    };

    let mut leaves = Vec::new();
    layout_leaves(&layout.root, &mut leaves);
    if leaves.len() > crate::app::api::MAX_LAYOUT_PANES {
        return ProjectionAdmission::Waiting {
            source,
            streams: Vec::new(),
            status: RemoteProjectionStreamStatus::Unsupported,
            message: format!(
                "remote layout has {} panes; projection limit is {}",
                leaves.len(),
                crate::app::api::MAX_LAYOUT_PANES
            ),
            preserve_last_known: false,
        };
    }

    let mut seen = BTreeSet::new();
    for pane in &leaves {
        let Some(terminal_id) = pane.terminal_id.clone() else {
            return ProjectionAdmission::Waiting {
                source,
                streams: Vec::new(),
                status: RemoteProjectionStreamStatus::Unsupported,
                message: "remote layout omits terminal ids; update remote Herdr".into(),
                preserve_last_known: false,
            };
        };
        if !seen.insert(terminal_id.clone()) {
            return ProjectionAdmission::Waiting {
                source,
                streams: Vec::new(),
                status: RemoteProjectionStreamStatus::NeedsAttention,
                message: "remote layout repeats a terminal id; projection refused".into(),
                preserve_last_known: false,
            };
        }
        let role = if pane
            .pane_id
            .as_deref()
            .is_some_and(|pane_id| pane_id == layout.focused_pane_id)
        {
            RemoteProjectionStreamRole::Control
        } else {
            RemoteProjectionStreamRole::Observe
        };
        leaf_roles.push((
            RemoteProjectionTerminalKey {
                host: source.host.clone(),
                session: source.session.clone(),
                workspace_id: source.workspace_id.clone(),
                terminal_id,
            },
            role,
        ));
    }

    if projection.status != RemoteProjectionStatus::Available
        || !state
            .remote_sources
            .host_status(&host)
            .is_some_and(|status| status.is_connected())
    {
        return ProjectionAdmission::Waiting {
            source,
            streams: leaf_roles,
            status: RemoteProjectionStreamStatus::StaleLastKnown,
            message: "remote disconnected/stale; last-known frames are read-only".into(),
            preserve_last_known: true,
        };
    }

    if !state
        .remote_sources
        .host_capabilities(&host)
        .supports_terminal_session_stream()
    {
        return ProjectionAdmission::Waiting {
            source,
            streams: leaf_roles,
            status: RemoteProjectionStreamStatus::Unsupported,
            message: "remote does not advertise terminal_session_stream; update remote Herdr"
                .into(),
            preserve_last_known: false,
        };
    }

    let Some(prepared) = state.remote_sources.connected_bridge_state(&host) else {
        return ProjectionAdmission::Waiting {
            source,
            streams: leaf_roles,
            status: RemoteProjectionStreamStatus::Connecting,
            message: "waiting for supervisor-prepared no-start bridge state".into(),
            preserve_last_known: true,
        };
    };

    // Geometry is computed by `compute_view` from this exact source/layout.
    // Wait until every exported leaf has an exact hit area; guessing 80x24 here
    // would let source selection resize an authoritative PTY incorrectly.
    let mut desired = Vec::with_capacity(leaf_roles.len());
    for (key, role) in &leaf_roles {
        let Some(hit) = state.view.remote_projection_hit_areas.iter().find(|hit| {
            hit.host == key.host
                && hit.session == key.session
                && hit.terminal_id.as_deref() == Some(key.terminal_id.as_str())
        }) else {
            return ProjectionAdmission::Waiting {
                source,
                streams: leaf_roles,
                status: RemoteProjectionStreamStatus::Connecting,
                message: "waiting for projected pane geometry".into(),
                preserve_last_known: true,
            };
        };
        desired.push(DesiredStream {
            key: key.clone(),
            role: *role,
            cols: hit.rect.width.saturating_sub(2).max(1),
            rows: hit.rect.height.saturating_sub(2).max(1),
        });
    }

    ProjectionAdmission::Ready {
        source,
        prepared,
        streams: desired,
    }
}

#[cfg(unix)]
struct ProjectionStreamHandle {
    role: RemoteProjectionStreamRole,
    writer: Arc<Mutex<Option<std::os::unix::net::UnixStream>>>,
    writable: Arc<AtomicBool>,
    input_sent: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

/// App-owned runtime registry. Never serialized or copied into `AppState`.
#[derive(Default)]
pub(crate) struct RemoteProjectionRuntime {
    source: Option<RemoteSpaceKey>,
    signature: Option<ProjectionSignature>,
    generation: u64,
    bridge: Option<crate::remote::SshStdioBridge>,
    local_socket: Option<PathBuf>,
    #[cfg(unix)]
    streams: BTreeMap<String, ProjectionStreamHandle>,
}

impl RemoteProjectionRuntime {
    pub(crate) fn reconcile(
        &mut self,
        state: &mut AppState,
        #[cfg_attr(
            not(unix),
            expect(
                unused_variables,
                reason = "host registry lookup only happens on the Unix projection bridge path"
            )
        )]
        hosts: &crate::remote_target::RemoteHostRegistry,
        #[cfg_attr(
            not(unix),
            expect(
                unused_variables,
                reason = "stream events are only sent by the Unix projection bridge workers"
            )
        )]
        event_tx: &tokio::sync::mpsc::Sender<AppEvent>,
    ) {
        let admission = plan_projection_admission(state);
        let next_source = match &admission {
            ProjectionAdmission::Inactive => None,
            ProjectionAdmission::Waiting { source, .. }
            | ProjectionAdmission::Ready { source, .. } => Some(source.clone()),
        };

        if self.source != next_source {
            // Generation FIRST, before Detach/socket shutdown/bridge join.
            self.generation = state.begin_remote_projection_generation(next_source.as_ref(), false);
            self.teardown();
            self.source = next_source.clone();
            self.signature = None;
        } else if self.generation == 0 && next_source.is_some() {
            self.generation = state.begin_remote_projection_generation(next_source.as_ref(), false);
        }

        match admission {
            ProjectionAdmission::Inactive => {}
            ProjectionAdmission::Waiting {
                source,
                streams,
                status,
                message,
                preserve_last_known,
            } => {
                if self.bridge.is_some() || self.signature.is_some() {
                    self.generation = state
                        .begin_remote_projection_generation(Some(&source), preserve_last_known);
                    self.teardown();
                    self.signature = None;
                }
                state.seed_remote_projection_streams(
                    self.generation,
                    streams
                        .into_iter()
                        .map(|(key, role)| (key, role, status, Some(message.clone()))),
                );
            }
            ProjectionAdmission::Ready {
                source,
                prepared,
                streams,
            } => {
                let signature = ProjectionSignature {
                    source: source.clone(),
                    prepared_shell_path: prepared.shell_path.clone(),
                    streams: streams.clone(),
                };
                if self.signature.as_ref() == Some(&signature) {
                    self.prune_finished_streams();
                    return;
                }
                if self
                    .signature
                    .as_ref()
                    .is_some_and(|existing| existing.same_stream_identity(&signature))
                {
                    // Geometry-only change: keep ownership/streams and send the
                    // existing structured Resize substrate. Observers update
                    // render viewport only; only the controller resizes the
                    // authoritative PTY (server-enforced). No generation/retry.
                    self.resize_streams(&streams);
                    self.signature = Some(signature);
                    return;
                }

                // Focus/layout/prepared-state change: generation first,
                // then graceful Detach, local socket shutdown, bounded bridge
                // teardown, then admit the new generation.
                self.generation = state.begin_remote_projection_generation(Some(&source), true);
                self.teardown();
                state.seed_remote_projection_streams(
                    self.generation,
                    streams.iter().map(|stream| {
                        (
                            stream.key.clone(),
                            stream.role,
                            RemoteProjectionStreamStatus::Connecting,
                            Some("opening no-start terminal session stream".into()),
                        )
                    }),
                );
                self.signature = Some(signature);

                #[cfg(not(unix))]
                {
                    state.seed_remote_projection_streams(
                        self.generation,
                        streams.into_iter().map(|stream| {
                            (
                                stream.key,
                                stream.role,
                                RemoteProjectionStreamStatus::Unsupported,
                                Some(
                                    "in-place remote terminal streaming is unsupported on Windows"
                                        .into(),
                                ),
                            )
                        }),
                    );
                }

                #[cfg(unix)]
                {
                    let Some(host_config) = hosts
                        .get(&source.host)
                        .filter(|host| host.session == source.session)
                    else {
                        state.seed_remote_projection_streams(
                            self.generation,
                            streams.into_iter().map(|stream| {
                                (
                                    stream.key,
                                    stream.role,
                                    RemoteProjectionStreamStatus::NeedsAttention,
                                    Some(
                                        "selected remote host/session is no longer configured"
                                            .into(),
                                    ),
                                )
                            }),
                        );
                        return;
                    };
                    let socket = projection_socket_path(self.generation);
                    match crate::remote::start_projection_bridge(
                        host_config,
                        &prepared,
                        socket.clone(),
                        crate::app::api::MAX_LAYOUT_PANES,
                    ) {
                        Ok(bridge) => {
                            self.local_socket = Some(socket.clone());
                            self.bridge = Some(bridge);
                        }
                        Err(err) => {
                            state.seed_remote_projection_streams(
                                self.generation,
                                streams.into_iter().map(|stream| {
                                    (
                                        stream.key,
                                        stream.role,
                                        RemoteProjectionStreamStatus::NeedsAttention,
                                        Some(format!("projection bridge failed closed: {err}")),
                                    )
                                }),
                            );
                            return;
                        }
                    }

                    for desired in streams {
                        let handle = spawn_projection_stream(
                            &socket,
                            desired.clone(),
                            self.generation,
                            event_tx.clone(),
                        );
                        self.streams.insert(desired.key.terminal_id, handle);
                    }
                }
            }
        }
    }

    #[cfg(unix)]
    pub(crate) fn send_input(
        &mut self,
        state: &AppState,
        event: crate::protocol::ClientInputEvent,
        event_tx: &tokio::sync::mpsc::Sender<AppEvent>,
    ) -> bool {
        let Some(selected) = state.selected_remote_space.as_ref() else {
            return false;
        };
        let Some((terminal_id, _)) = state
            .view
            .remote_projection_hit_areas
            .iter()
            .find(|hit| {
                hit.focused
                    && hit.live
                    && hit.host == selected.host
                    && hit.session == selected.session
            })
            .and_then(|hit| hit.terminal_id.as_ref().map(|id| (id, hit)))
        else {
            // A projection is active but no live authoritative focused
            // terminal exists. Consume fail-closed; never fall through local.
            return true;
        };
        let Some(stream) = self.streams.get(terminal_id) else {
            return true;
        };
        if stream.role != RemoteProjectionStreamRole::Control
            || !stream.writable.load(Ordering::Acquire)
            || !state
                .remote_projection_frame(selected, terminal_id)
                .is_some_and(|entry| entry.status.accepts_input())
        {
            return true;
        }
        stream.input_sent.store(true, Ordering::Release);
        let result = stream
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "stream closed"))
            .and_then(|writer| {
                crate::protocol::write_message(
                    writer,
                    &crate::protocol::ClientMessage::InputEvents {
                        events: vec![event],
                    },
                )
                .map_err(|err| std::io::Error::other(err.to_string()))
            });
        if let Err(err) = result {
            stream.writable.store(false, Ordering::Release);
            if let Some(writer) = stream
                .writer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_mut()
            {
                let _ = writer.shutdown(std::net::Shutdown::Both);
            }
            let _ = event_tx.try_send(AppEvent::RemoteProjectionStream {
                key: RemoteProjectionTerminalKey {
                    host: selected.host.clone(),
                    session: selected.session.clone(),
                    workspace_id: selected.workspace_id.clone(),
                    terminal_id: terminal_id.clone(),
                },
                generation: self.generation,
                role: RemoteProjectionStreamRole::Control,
                status: RemoteProjectionStreamStatus::NeedsAttention,
                frame: None,
                message: Some(format!(
                    "remote input delivery is uncertain ({err}); not retried"
                )),
            });
            tracing::warn!(terminal_id, %err, "remote projection input delivery uncertain; not retrying");
        }
        true
    }

    #[cfg(not(unix))]
    pub(crate) fn send_input(
        &mut self,
        state: &AppState,
        _event: crate::protocol::ClientInputEvent,
        _event_tx: &tokio::sync::mpsc::Sender<AppEvent>,
    ) -> bool {
        // Windows projection is explicitly unsupported/read-only. Consume any
        // attempted projected input; never let it reach a local pane.
        state.selected_remote_space.is_some()
    }

    #[cfg(unix)]
    fn resize_streams(&mut self, desired: &[DesiredStream]) {
        for stream in desired {
            let Some(handle) = self.streams.get(&stream.key.terminal_id) else {
                continue;
            };
            let result = handle
                .writer
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_mut()
                .map(|writer| {
                    crate::protocol::write_message(
                        writer,
                        &crate::protocol::ClientMessage::Resize {
                            cols: stream.cols,
                            rows: stream.rows,
                            cell_width_px: 0,
                            cell_height_px: 0,
                        },
                    )
                });
            if result.is_some_and(|result| result.is_err()) {
                handle.writable.store(false, Ordering::Release);
            }
        }
    }

    #[cfg(not(unix))]
    fn resize_streams(&mut self, _desired: &[DesiredStream]) {}

    #[cfg(unix)]
    fn prune_finished_streams(&mut self) {
        let finished = self
            .streams
            .iter()
            .filter_map(|(id, handle)| handle.done.load(Ordering::Acquire).then_some(id.clone()))
            .collect::<Vec<_>>();
        for id in finished {
            if let Some(mut handle) = self.streams.remove(&id) {
                if let Some(join) = handle.join.take() {
                    let _ = join.join();
                }
            }
        }
    }

    #[cfg(not(unix))]
    fn prune_finished_streams(&mut self) {}

    fn teardown(&mut self) {
        #[cfg(unix)]
        {
            // Detach/release first, then local socket shutdown. Bridge Drop is
            // intentionally LAST and supplies the bounded deadline/kill
            // fallback before joining its accepted 1:1 workers.
            for handle in self.streams.values_mut() {
                let mut writer = handle
                    .writer
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(stream) = writer.as_mut() {
                    let _ = crate::protocol::write_message(
                        stream,
                        &crate::protocol::ClientMessage::Detach,
                    );
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
                writer.take();
                handle.writable.store(false, Ordering::Release);
            }
            drop(self.bridge.take());
            for (_, mut handle) in std::mem::take(&mut self.streams) {
                if handle.done.load(Ordering::Acquire) {
                    if let Some(join) = handle.join.take() {
                        let _ = join.join();
                    }
                } else {
                    // The local socket and accepted bridge side are already
                    // shut. Dropping detaches; worker's read returns and exits.
                    drop(handle.join.take());
                }
            }
            if let Some(path) = self.local_socket.take() {
                let _ = std::fs::remove_file(path);
            }
        }
        #[cfg(not(unix))]
        {
            self.bridge = None;
            self.local_socket = None;
        }
    }
}

impl Drop for RemoteProjectionRuntime {
    fn drop(&mut self) {
        self.teardown();
    }
}

#[cfg(unix)]
fn projection_socket_path(generation: u64) -> PathBuf {
    let name = format!("herdr-projection-{}-{generation}.sock", std::process::id());
    let in_tmp = std::env::temp_dir().join(&name);
    use std::os::unix::ffi::OsStrExt;
    if in_tmp.as_os_str().as_bytes().len() <= 103 {
        in_tmp
    } else {
        PathBuf::from("/tmp").join(name)
    }
}

#[cfg(unix)]
fn emit_stream_event(
    tx: &tokio::sync::mpsc::Sender<AppEvent>,
    desired: &DesiredStream,
    generation: u64,
    status: RemoteProjectionStreamStatus,
    frame: Option<crate::protocol::FrameData>,
    message: Option<String>,
) {
    let _ = tx.try_send(AppEvent::RemoteProjectionStream {
        key: desired.key.clone(),
        generation,
        role: desired.role,
        status,
        frame,
        message,
    });
}

#[cfg(unix)]
fn spawn_projection_stream(
    socket: &Path,
    desired: DesiredStream,
    generation: u64,
    event_tx: tokio::sync::mpsc::Sender<AppEvent>,
) -> ProjectionStreamHandle {
    let writer = Arc::new(Mutex::new(None));
    let writable = Arc::new(AtomicBool::new(false));
    let input_sent = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let worker_writer = Arc::clone(&writer);
    let worker_writable = Arc::clone(&writable);
    let worker_input_sent = Arc::clone(&input_sent);
    let worker_done = Arc::clone(&done);
    let socket = socket.to_owned();
    let worker_desired = desired.clone();
    let join = std::thread::spawn(move || {
        let first = run_projection_stream_once(
            &socket,
            &worker_desired,
            generation,
            &event_tx,
            &worker_writer,
            &worker_writable,
            worker_desired.role,
            worker_desired.role,
        );
        if let Err(StreamEnd::OwnershipConflict(reason)) = first {
            // No takeover affordance: a no-takeover ownership conflict falls
            // back to a fresh observer connection for this same generation.
            worker_writable.store(false, Ordering::Release);
            emit_stream_event(
                &event_tx,
                &worker_desired,
                generation,
                RemoteProjectionStreamStatus::OwnedReadOnly,
                None,
                Some(reason),
            );
            let _ = run_projection_stream_once(
                &socket,
                &worker_desired,
                generation,
                &event_tx,
                &worker_writer,
                &worker_writable,
                RemoteProjectionStreamRole::Observe,
                RemoteProjectionStreamRole::Control,
            );
        } else if let Err(end) = first {
            worker_writable.store(false, Ordering::Release);
            let status = if worker_input_sent.load(Ordering::Acquire) {
                RemoteProjectionStreamStatus::NeedsAttention
            } else {
                RemoteProjectionStreamStatus::Disconnected
            };
            emit_stream_event(
                &event_tx,
                &worker_desired,
                generation,
                status,
                None,
                Some(end.message()),
            );
        }
        worker_done.store(true, Ordering::Release);
    });

    ProjectionStreamHandle {
        role: desired.role,
        writer,
        writable,
        input_sent,
        done,
        join: Some(join),
    }
}

#[cfg(unix)]
#[derive(Debug)]
enum StreamEnd {
    OwnershipConflict(String),
    Closed(String),
}

#[cfg(unix)]
impl StreamEnd {
    fn message(self) -> String {
        match self {
            Self::OwnershipConflict(message) | Self::Closed(message) => message,
        }
    }
}

#[cfg(unix)]
fn run_projection_stream_once(
    socket: &Path,
    desired: &DesiredStream,
    generation: u64,
    event_tx: &tokio::sync::mpsc::Sender<AppEvent>,
    shared_writer: &Arc<Mutex<Option<std::os::unix::net::UnixStream>>>,
    writable: &Arc<AtomicBool>,
    actual_role: RemoteProjectionStreamRole,
    reported_role: RemoteProjectionStreamRole,
) -> Result<(), StreamEnd> {
    use crate::protocol::{
        ClientKeybindings, ClientLaunchMode, ClientMessage, RenderEncoding, ServerMessage,
        MAX_FRAME_SIZE, MAX_GRAPHICS_FRAME_SIZE, PROTOCOL_VERSION,
    };

    let mut stream = std::os::unix::net::UnixStream::connect(socket)
        .map_err(|err| StreamEnd::Closed(format!("projection stream connect failed: {err}")))?;
    let hello = ClientMessage::Hello {
        version: PROTOCOL_VERSION,
        cols: desired.cols,
        rows: desired.rows,
        cell_width_px: 0,
        cell_height_px: 0,
        requested_encoding: RenderEncoding::SemanticFrame,
        keybindings: ClientKeybindings::Server,
        launch_mode: ClientLaunchMode::TerminalAttach,
    };
    crate::protocol::write_message(&mut stream, &hello)
        .map_err(|err| StreamEnd::Closed(format!("projection handshake send failed: {err}")))?;
    match crate::protocol::read_message::<_, ServerMessage>(&mut stream, MAX_FRAME_SIZE) {
        Ok(ServerMessage::Welcome {
            encoding: RenderEncoding::SemanticFrame,
            error: None,
            ..
        }) => {}
        Ok(ServerMessage::Welcome {
            error: Some(error), ..
        }) => {
            return Err(StreamEnd::Closed(format!(
                "projection handshake rejected: {error}"
            )))
        }
        Ok(ServerMessage::Welcome { encoding, .. }) => {
            return Err(StreamEnd::Closed(format!(
                "projection negotiated unsupported render encoding {encoding:?}"
            )))
        }
        Ok(_) => return Err(StreamEnd::Closed("projection expected Welcome".into())),
        Err(err) => {
            return Err(StreamEnd::Closed(format!(
                "projection handshake read failed: {err}"
            )))
        }
    }
    let request = match actual_role {
        RemoteProjectionStreamRole::Observe => ClientMessage::ObserveTerminal {
            target: desired.key.terminal_id.clone(),
        },
        RemoteProjectionStreamRole::Control => ClientMessage::ControlTerminal {
            target: desired.key.terminal_id.clone(),
            takeover: false,
        },
    };
    crate::protocol::write_message(&mut stream, &request)
        .map_err(|err| StreamEnd::Closed(format!("projection stream request failed: {err}")))?;
    let write_stream = stream
        .try_clone()
        .map_err(|err| StreamEnd::Closed(format!("projection stream clone failed: {err}")))?;
    *shared_writer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(write_stream);

    let mut received_frame = false;
    loop {
        match crate::protocol::read_message::<_, ServerMessage>(
            &mut stream,
            MAX_GRAPHICS_FRAME_SIZE,
        ) {
            Ok(ServerMessage::Frame(frame)) => {
                received_frame = true;
                let status = match (actual_role, reported_role) {
                    (RemoteProjectionStreamRole::Control, _) => {
                        writable.store(true, Ordering::Release);
                        RemoteProjectionStreamStatus::LiveController
                    }
                    (RemoteProjectionStreamRole::Observe, RemoteProjectionStreamRole::Control) => {
                        RemoteProjectionStreamStatus::OwnedReadOnly
                    }
                    (RemoteProjectionStreamRole::Observe, _) => {
                        RemoteProjectionStreamStatus::LiveObserver
                    }
                };
                emit_stream_event(event_tx, desired, generation, status, Some(frame), None);
            }
            Ok(ServerMessage::ServerShutdown { reason }) => {
                writable.store(false, Ordering::Release);
                *shared_writer
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                let reason = reason.unwrap_or_else(|| "remote terminal session closed".into());
                if actual_role == RemoteProjectionStreamRole::Control
                    && !received_frame
                    && (reason.contains("already has an attached client")
                        || reason.contains("already has a controller"))
                {
                    return Err(StreamEnd::OwnershipConflict(reason));
                }
                return Err(StreamEnd::Closed(reason));
            }
            Ok(ServerMessage::Graphics { .. }) => {
                // Kitty graphics are deliberately out of scope for the first
                // in-place projection unit. Ignore graphics without claiming
                // support; semantic text frames remain live.
            }
            Ok(_) => {}
            Err(crate::protocol::FramingError::UnexpectedEof) => {
                writable.store(false, Ordering::Release);
                return Err(StreamEnd::Closed(
                    "remote terminal session stream disconnected".into(),
                ));
            }
            Err(err) => {
                writable.store(false, Ordering::Release);
                return Err(StreamEnd::Closed(format!(
                    "remote terminal session stream failed: {err}"
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{LayoutDescription, LayoutNode, LayoutPane, WorkspaceInfo};

    fn workspace(id: &str, number: usize, focused: bool) -> WorkspaceInfo {
        WorkspaceInfo {
            workspace_id: id.into(),
            number,
            label: id.into(),
            focused,
            pane_count: 1,
            tab_count: 1,
            active_tab_id: "tab".into(),
            agent_status: crate::api::schema::AgentStatus::Unknown,
            worktree: None,
        }
    }

    fn pane(id: usize, focused: bool) -> (LayoutPane, String) {
        let pane_id = format!("pane-{id}");
        (
            LayoutPane {
                pane_id: Some(pane_id.clone()),
                terminal_id: Some(format!("term-{id}")),
                ..Default::default()
            },
            if focused { pane_id } else { String::new() },
        )
    }

    fn layout_with_panes(count: usize) -> LayoutDescription {
        assert!(count > 0);
        let (first, focused) = pane(0, true);
        let mut root = LayoutNode::Pane { pane: first };
        for id in 1..count {
            let (next, _) = pane(id, false);
            root = LayoutNode::Split {
                direction: crate::api::schema::SplitDirection::Right,
                ratio: 0.5,
                first: Box::new(root),
                second: Box::new(LayoutNode::Pane { pane: next }),
            };
        }
        LayoutDescription {
            workspace_id: "ws".into(),
            tab_id: "tab".into(),
            zoomed: false,
            focused_pane_id: focused,
            root,
        }
    }

    fn selected_state(capabilities: crate::remote_source::RemoteSourceCapabilities) -> AppState {
        let mut state = AppState::test_new();
        let host = RemoteHostKey::new("remote-a", "default");
        state
            .remote_sources
            .replace_workspace_snapshot(host.clone(), vec![workspace("ws", 1, true)]);
        state.remote_sources.set_capabilities(&host, capabilities);
        state.remote_sources.apply_projection_snapshot(
            &host,
            vec![crate::remote_source::RemoteProjectionSnapshot {
                workspace_id: "ws".into(),
                tab_id: Some("tab".into()),
                tab_label: Some("tab".into()),
                status: RemoteProjectionStatus::Available,
                layout: Some(layout_with_panes(1)),
            }],
        );
        state.selected_remote_space = Some(RemoteSpaceKey {
            host: "remote-a".into(),
            session: "default".into(),
            workspace_id: "ws".into(),
        });
        state
    }

    #[test]
    fn projection_cap_is_pinned_to_authoritative_layout_cap() {
        assert_eq!(
            crate::remote::BRIDGE_MAX_CONCURRENT_STREAMS,
            crate::app::api::MAX_LAYOUT_PANES
        );
    }

    #[test]
    fn older_remote_with_terminal_attach_but_without_session_stream_fails_closed() {
        // terminal_attach is intentionally not represented by a route cache
        // bool. Absence of additive terminal_session_stream remains decisive.
        let capabilities = crate::remote_source::RemoteSourceCapabilities {
            layout_export: true,
            ..Default::default()
        };
        let state = selected_state(capabilities);
        assert!(matches!(
            plan_projection_admission(&state),
            ProjectionAdmission::Waiting {
                status: RemoteProjectionStreamStatus::Unsupported,
                ..
            }
        ));
    }

    #[test]
    fn over_limit_projection_is_rejected_before_stream_admission() {
        let mut state = selected_state(crate::remote_source::RemoteSourceCapabilities {
            layout_export: true,
            terminal_session_stream: true,
            ..Default::default()
        });
        let host = RemoteHostKey::new("remote-a", "default");
        state.remote_sources.apply_projection_snapshot(
            &host,
            vec![crate::remote_source::RemoteProjectionSnapshot {
                workspace_id: "ws".into(),
                tab_id: Some("tab".into()),
                tab_label: Some("tab".into()),
                status: RemoteProjectionStatus::Available,
                layout: Some(layout_with_panes(crate::app::api::MAX_LAYOUT_PANES + 1)),
            }],
        );
        assert!(matches!(
            plan_projection_admission(&state),
            ProjectionAdmission::Waiting {
                status: RemoteProjectionStreamStatus::Unsupported,
                ..
            }
        ));
    }

    #[test]
    fn late_generation_frame_is_rejected_after_source_switch() {
        let mut state = AppState::test_new();
        let a = RemoteSpaceKey {
            host: "a".into(),
            session: "default".into(),
            workspace_id: "ws-a".into(),
        };
        state.selected_remote_space = Some(a.clone());
        let old = state.begin_remote_projection_generation(Some(&a), false);
        let key = RemoteProjectionTerminalKey {
            host: "a".into(),
            session: "default".into(),
            workspace_id: "ws-a".into(),
            terminal_id: "term-a".into(),
        };
        state.seed_remote_projection_streams(
            old,
            [(
                key.clone(),
                RemoteProjectionStreamRole::Control,
                RemoteProjectionStreamStatus::Connecting,
                None,
            )],
        );
        state.apply_remote_projection_stream_event(
            key.clone(),
            old,
            RemoteProjectionStreamRole::Control,
            RemoteProjectionStreamStatus::LiveController,
            None,
            None,
        );
        let b = RemoteSpaceKey {
            host: "b".into(),
            session: "default".into(),
            workspace_id: "ws-b".into(),
        };
        state.selected_remote_space = Some(b.clone());
        state.begin_remote_projection_generation(Some(&b), false);
        state.apply_remote_projection_stream_event(
            key,
            old,
            RemoteProjectionStreamRole::Control,
            RemoteProjectionStreamStatus::LiveController,
            None,
            None,
        );
        assert!(state.remote_projection_frames.is_empty());
    }

    #[test]
    fn unseeded_terminal_frame_is_rejected_for_current_generation() {
        let mut state = AppState::test_new();
        let selected = RemoteSpaceKey {
            host: "a".into(),
            session: "default".into(),
            workspace_id: "ws-a".into(),
        };
        state.selected_remote_space = Some(selected.clone());
        let generation = state.begin_remote_projection_generation(Some(&selected), false);
        state.apply_remote_projection_stream_event(
            RemoteProjectionTerminalKey {
                host: "a".into(),
                session: "default".into(),
                workspace_id: "ws-a".into(),
                terminal_id: "unseeded".into(),
            },
            generation,
            RemoteProjectionStreamRole::Observe,
            RemoteProjectionStreamStatus::LiveObserver,
            None,
            None,
        );
        assert!(
            state.remote_projection_frames.is_empty(),
            "only admitted/seeded terminal streams may enter the frame cache"
        );
    }
}
