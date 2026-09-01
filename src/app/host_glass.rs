use std::collections::BTreeMap;
use std::io;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender, TrySendError},
    Arc, Mutex, TryLockError,
};
#[cfg(unix)]
use std::thread::JoinHandle;
#[cfg(unix)]
use std::time::Duration;
use std::time::Instant;

use ratatui::layout::Rect;
use ratatui::Frame;

use crate::pane::{PtyFreeTerminalSurface, TerminalCursorState};
use crate::remote_source::RemoteHostKey;

const GLASS_SCROLLBACK_LIMIT_BYTES: usize = 0;
const DEFAULT_CELL_WIDTH_PX: u32 = 1;
const DEFAULT_CELL_HEIGHT_PX: u32 = 1;

/// Truthful lifecycle state for one host's glass surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // S1 defines all truthful states; stream transitions begin in S2.
pub(crate) enum GlassStatus {
    Connecting,
    Live,
    Stale { since: Instant },
}

/// Pure AppState metadata for one host glass generation. Presence in the
/// AppState map is the existence marker; runtime objects stay on `App`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HostGlassState {
    pub(crate) generation: u64,
    pub(crate) status: GlassStatus,
    pub(crate) message: Option<String>,
    /// Brief local-only feedback after glass-directed input is discarded.
    pub(crate) input_drop_cue: Option<GlassInputDropReason>,
}

/// A local, PTY-free VT surface fed only by TerminalAnsi bytes.
///
/// This type deliberately has no pane/terminal id and is never inserted into
/// PaneRuntime or TerminalRuntimeRegistry, keeping it outside agent detection.
pub(crate) struct GlassSurfaceCore {
    generation: u64,
    area: Rect,
    terminal: PtyFreeTerminalSurface,
    has_frame: std::sync::atomic::AtomicBool,
}

#[allow(dead_code)] // Visible-grid/cursor inspection is exercised by PTY-free parser tests.
impl GlassSurfaceCore {
    pub(crate) fn new(area: Rect, generation: u64) -> io::Result<Self> {
        let terminal = PtyFreeTerminalSurface::new(
            area.width.max(1),
            area.height.max(1),
            GLASS_SCROLLBACK_LIMIT_BYTES,
        )?;
        Ok(Self {
            generation,
            area,
            terminal,
            has_frame: std::sync::atomic::AtomicBool::new(false),
        })
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn retag_generation(&mut self, generation: u64) {
        self.generation = generation;
    }

    pub(crate) fn area(&self) -> Rect {
        self.area
    }

    pub(crate) fn feed(&self, bytes: &[u8]) {
        self.terminal.feed(bytes);
        self.has_frame
            .store(true, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn has_frame(&self) -> bool {
        self.has_frame.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) fn resize(&mut self, area: Rect) -> io::Result<()> {
        self.terminal.resize(
            area.height.max(1),
            area.width.max(1),
            DEFAULT_CELL_WIDTH_PX,
            DEFAULT_CELL_HEIGHT_PX,
        )?;
        self.area = area;
        Ok(())
    }

    pub(crate) fn visible_text(&self) -> String {
        self.terminal.visible_text()
    }

    pub(crate) fn cursor_state(&self) -> Option<TerminalCursorState> {
        self.terminal.cursor_state()
    }

    /// Render through the same grid-to-ratatui seam as local pane terminals.
    /// No IO or state reconciliation occurs here.
    pub(crate) fn render(&self, frame: &mut Frame<'_>, show_cursor: bool) {
        self.terminal.render(frame, self.area, show_cursor);
    }
}

/// App-owned VT registry. The pure existence/status mirror lives separately
/// on AppState; this collection contains only local runtime surfaces.
#[derive(Default)]
pub(crate) struct HostGlassSurfaceRegistry {
    surfaces: BTreeMap<RemoteHostKey, GlassSurfaceCore>,
}

#[allow(dead_code)] // Explicit removal is reserved for later bounded lifecycle pruning.
impl HostGlassSurfaceRegistry {
    pub(crate) fn insert(&mut self, host: RemoteHostKey, surface: GlassSurfaceCore) {
        self.surfaces.insert(host, surface);
    }

    pub(crate) fn get(&self, host: &RemoteHostKey) -> Option<&GlassSurfaceCore> {
        self.surfaces.get(host)
    }

    pub(crate) fn get_mut(&mut self, host: &RemoteHostKey) -> Option<&mut GlassSurfaceCore> {
        self.surfaces.get_mut(host)
    }

    pub(crate) fn remove(&mut self, host: &RemoteHostKey) -> Option<GlassSurfaceCore> {
        self.surfaces.remove(host)
    }

    pub(crate) fn apply_frame(&self, host: &RemoteHostKey, generation: u64, bytes: &[u8]) -> bool {
        let Some(surface) = self.surfaces.get(host) else {
            return false;
        };
        if surface.generation() != generation {
            return false;
        }
        surface.feed(bytes);
        true
    }
}

/// Exact geometry advertised by one full-App TerminalAnsi client stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GlassGeometry {
    pub(crate) area: Rect,
    pub(crate) cell_width_px: u32,
    pub(crate) cell_height_px: u32,
}

impl GlassGeometry {
    #[cfg(any(unix, test))]
    pub(crate) fn hello(self) -> crate::protocol::ClientMessage {
        crate::protocol::ClientMessage::Hello {
            version: crate::protocol::PROTOCOL_VERSION,
            cols: self.area.width.max(1),
            rows: self.area.height.max(1),
            cell_width_px: self.cell_width_px,
            cell_height_px: self.cell_height_px,
            requested_encoding: crate::protocol::RenderEncoding::TerminalAnsi,
            keybindings: crate::protocol::ClientKeybindings::Server,
            launch_mode: crate::protocol::ClientLaunchMode::App,
        }
    }

    #[cfg(any(unix, test))]
    fn resize(self) -> crate::protocol::ClientMessage {
        crate::protocol::ClientMessage::Resize {
            cols: self.area.width.max(1),
            rows: self.area.height.max(1),
            cell_width_px: self.cell_width_px,
            cell_height_px: self.cell_height_px,
        }
    }

    fn usable(self) -> bool {
        self.area != Rect::default() && self.area.width > 0 && self.area.height > 0
    }
}

#[cfg(unix)]
pub(super) const GLASS_OUTBOUND_CAPACITY: usize = 16;
#[cfg(unix)]
const GLASS_RECONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
#[cfg(unix)]
const GLASS_RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(2);
#[cfg(unix)]
const GLASS_BACKOFF_POLL: Duration = Duration::from_millis(25);
#[cfg(unix)]
const GLASS_TEARDOWN_BUDGET: Duration = Duration::from_millis(500);

#[cfg(unix)]
const GLASS_MOUSE_CAPTURE_UNKNOWN: u8 = 0;
#[cfg(unix)]
const GLASS_MOUSE_CAPTURE_DISABLED: u8 = 1;
#[cfg(unix)]
const GLASS_MOUSE_CAPTURE_ENABLED: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GlassInputOutcome {
    NotActive,
    #[cfg(unix)]
    Queued,
    Dropped(GlassInputDropReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GlassInputDropReason {
    Stale,
    Connecting,
    MissingStream,
    #[cfg(unix)]
    GenerationChanged,
    #[cfg(unix)]
    Disconnected,
    #[cfg(unix)]
    AdmissionBusy,
    #[cfg(unix)]
    QueueFull,
    #[cfg(unix)]
    QueueClosed,
}

impl GlassInputDropReason {
    pub(crate) fn cue_text(self) -> &'static str {
        match self {
            Self::Stale => "glass is stale",
            Self::Connecting => "glass is connecting",
            Self::MissingStream => "stream unavailable",
            #[cfg(unix)]
            Self::GenerationChanged => "stream changed",
            #[cfg(unix)]
            Self::Disconnected => "stream disconnected",
            #[cfg(unix)]
            Self::AdmissionBusy => "stream reconnecting",
            #[cfg(unix)]
            Self::QueueFull => "input queue full",
            #[cfg(unix)]
            Self::QueueClosed => "stream closed",
        }
    }
}

#[cfg(unix)]
struct GlassOutboundAdmission {
    connection: u64,
    sender: SyncSender<crate::protocol::ClientMessage>,
}

#[cfg(unix)]
type GlassOutboundGate = Arc<Mutex<Option<GlassOutboundAdmission>>>;

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlassAdmissionOutcome {
    Queued,
    Disconnected,
    Busy,
    Full,
    Closed,
}

#[cfg(unix)]
fn try_admit_glass_message(
    gate: &GlassOutboundGate,
    message: crate::protocol::ClientMessage,
) -> GlassAdmissionOutcome {
    let mut admission = match gate.try_lock() {
        Ok(admission) => admission,
        Err(TryLockError::WouldBlock) => return GlassAdmissionOutcome::Busy,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
    };
    let Some(active) = admission.as_ref() else {
        return GlassAdmissionOutcome::Disconnected;
    };
    match active.sender.try_send(message) {
        Ok(()) => GlassAdmissionOutcome::Queued,
        Err(TrySendError::Full(_)) => GlassAdmissionOutcome::Full,
        Err(TrySendError::Disconnected(_)) => {
            *admission = None;
            GlassAdmissionOutcome::Closed
        }
    }
}

#[cfg(unix)]
fn close_glass_admission(gate: &GlassOutboundGate, connection: Option<u64>) {
    let mut admission = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if connection.is_none_or(|connection| {
        admission
            .as_ref()
            .is_some_and(|active| active.connection == connection)
    }) {
        admission.take();
    }
}

#[cfg(unix)]
#[derive(Debug)]
enum GlassStreamEnd {
    Retryable(String),
    Fatal(String),
    Stopped,
}

#[cfg(unix)]
struct GlassShutdownRegistration<'a> {
    slot: &'a Arc<Mutex<Option<std::os::unix::net::UnixStream>>>,
}

#[cfg(unix)]
impl Drop for GlassShutdownRegistration<'_> {
    fn drop(&mut self) {
        *self
            .slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }
}

#[cfg(unix)]
struct GlassStreamHandle {
    outbound_gate: GlassOutboundGate,
    geometry: Arc<Mutex<GlassGeometry>>,
    connected: Arc<AtomicBool>,
    mouse_capture: Arc<std::sync::atomic::AtomicU8>,
    shutdown_stream: Arc<Mutex<Option<std::os::unix::net::UnixStream>>>,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

#[cfg(unix)]
impl GlassStreamHandle {
    /// Queue one resize without ever writing a socket from the App/event loop.
    /// A disconnected stream drops the command; the next Hello reads the
    /// latest shared geometry instead of replaying stale queued commands.
    fn resize(&self, geometry: GlassGeometry) -> bool {
        *self
            .geometry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = geometry;
        matches!(
            try_admit_glass_message(&self.outbound_gate, geometry.resize()),
            GlassAdmissionOutcome::Queued
        )
    }

    fn stop_and_join(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.connected.store(false, Ordering::Release);
        let detach_sender = self
            .outbound_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .map(|active| active.sender);
        if let Some(sender) = detach_sender {
            let _ = sender.try_send(crate::protocol::ClientMessage::Detach);
        }
        if let Some(stream) = self
            .shutdown_stream
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        let deadline = Instant::now() + GLASS_TEARDOWN_BUDGET;
        while !self.done.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(join) = self.join.take() {
            if self.done.load(Ordering::Acquire) {
                let _ = join.join();
            } else {
                // The socket has already been shut down and the worker owns no
                // App state. Detach rather than make process/app teardown
                // unbounded on an unexpected worker stall.
                drop(join);
            }
        }
    }
}

#[cfg(unix)]
impl Drop for GlassStreamHandle {
    fn drop(&mut self) {
        self.stop_and_join();
    }
}

#[cfg(unix)]
#[derive(Clone, Debug)]
enum GlassWorkerUpdate {
    Frame {
        host: RemoteHostKey,
        generation: u64,
        bytes: Vec<u8>,
    },
    Status {
        host: RemoteHostKey,
        generation: u64,
        status: GlassStatus,
        message: Option<String>,
    },
}

#[cfg(unix)]
struct GlassStreamReaper {
    tx: mpsc::Sender<GlassStreamHandle>,
}

#[cfg(unix)]
impl Default for GlassStreamReaper {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel::<GlassStreamHandle>();
        std::thread::spawn(move || {
            while let Ok(stream) = rx.recv() {
                drop(stream);
            }
        });
        Self { tx }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GlassSignature {
    host: RemoteHostKey,
    prepared_shell_path: String,
    geometry: GlassGeometry,
}

impl GlassSignature {
    fn same_stream_identity(&self, other: &Self) -> bool {
        self.host == other.host && self.prepared_shell_path == other.prepared_shell_path
    }
}

/// App-owned bridge/stream lifecycle for the one selected host glass.
///
/// The runtime is reconciled only after current content geometry is available;
/// semantic projection may retire its predecessor earlier, but only one bridge
/// consumer is active for the selected source.
#[cfg_attr(not(unix), derive(Default))]
pub(crate) struct HostGlassRuntime {
    source: Option<RemoteHostKey>,
    generation: u64,
    signature: Option<GlassSignature>,
    #[cfg(unix)]
    stream: Option<GlassStreamHandle>,
    #[cfg(unix)]
    worker_update_tx: mpsc::Sender<GlassWorkerUpdate>,
    #[cfg(unix)]
    worker_update_rx: Receiver<GlassWorkerUpdate>,
    #[cfg(unix)]
    reaper: GlassStreamReaper,
}

#[cfg(unix)]
impl Default for HostGlassRuntime {
    fn default() -> Self {
        let (worker_update_tx, worker_update_rx) = mpsc::channel();
        Self {
            source: None,
            generation: 0,
            signature: None,
            stream: None,
            worker_update_tx,
            worker_update_rx,
            reaper: GlassStreamReaper::default(),
        }
    }
}

impl HostGlassRuntime {
    /// Queue one structured input event without socket IO on the App/event
    /// loop. Stale/connecting input is never queued, and any bounded-channel
    /// failure is a drop rather than a reconnect replay.
    pub(crate) fn send_input(
        &self,
        state: &crate::app::state::AppState,
        event: crate::protocol::ClientInputEvent,
    ) -> GlassInputOutcome {
        if !state.host_glass_surface_active() {
            return GlassInputOutcome::NotActive;
        }
        let Some((host, glass)) = state.selected_host_glass_mode() else {
            // Selection owns the content area immediately, while runtime
            // reconciliation creates generation metadata later. Input in
            // that first-attach window is consumed and truthfully reported,
            // never allowed to fall through or queue for a future stream.
            return GlassInputOutcome::Dropped(GlassInputDropReason::Connecting);
        };
        if matches!(glass.status, GlassStatus::Stale { .. }) {
            return GlassInputOutcome::Dropped(GlassInputDropReason::Stale);
        }
        if glass.status != GlassStatus::Live {
            return GlassInputOutcome::Dropped(GlassInputDropReason::Connecting);
        }

        #[cfg(not(unix))]
        {
            let _ = host;
            let _ = event;
            GlassInputOutcome::Dropped(GlassInputDropReason::MissingStream)
        }

        #[cfg(unix)]
        {
            let Some(stream) = self
                .source
                .as_ref()
                .filter(|source| *source == host)
                .and(self.stream.as_ref())
            else {
                return GlassInputOutcome::Dropped(GlassInputDropReason::MissingStream);
            };
            if glass.generation != self.generation {
                return GlassInputOutcome::Dropped(GlassInputDropReason::GenerationChanged);
            }
            match try_admit_glass_message(
                &stream.outbound_gate,
                crate::protocol::ClientMessage::InputEvents {
                    events: vec![event],
                },
            ) {
                GlassAdmissionOutcome::Queued => GlassInputOutcome::Queued,
                GlassAdmissionOutcome::Disconnected => {
                    GlassInputOutcome::Dropped(GlassInputDropReason::Disconnected)
                }
                GlassAdmissionOutcome::Busy => {
                    GlassInputOutcome::Dropped(GlassInputDropReason::AdmissionBusy)
                }
                GlassAdmissionOutcome::Full => {
                    GlassInputOutcome::Dropped(GlassInputDropReason::QueueFull)
                }
                GlassAdmissionOutcome::Closed => {
                    GlassInputOutcome::Dropped(GlassInputDropReason::QueueClosed)
                }
            }
        }
    }

    /// Latest full-App capture mode reported by the selected remote Herdr.
    /// The server derives this from its authoritative App/VT state. A stale,
    /// disconnected, or predecessor stream can never force local capture.
    pub(crate) fn selected_mouse_capture(
        &self,
        state: &crate::app::state::AppState,
    ) -> Option<bool> {
        let (host, glass) = state.selected_host_glass_mode()?;
        if glass.status != GlassStatus::Live {
            return None;
        }

        #[cfg(not(unix))]
        {
            let _ = host;
            None
        }

        #[cfg(unix)]
        {
            if glass.generation != self.generation {
                return None;
            }
            let stream = self
                .source
                .as_ref()
                .filter(|source| *source == host)
                .and(self.stream.as_ref())?;
            if !stream.connected.load(Ordering::Acquire) {
                return None;
            }
            match stream.mouse_capture.load(Ordering::Acquire) {
                GLASS_MOUSE_CAPTURE_DISABLED => Some(false),
                GLASS_MOUSE_CAPTURE_ENABLED => Some(true),
                _ => None,
            }
        }
    }

    pub(crate) fn reconcile(
        &mut self,
        state: &mut crate::app::state::AppState,
        surfaces: &mut HostGlassSurfaceRegistry,
        hosts: &crate::remote_target::RemoteHostRegistry,
        event_tx: &tokio::sync::mpsc::Sender<crate::events::AppEvent>,
        bridge_owner: &mut crate::app::remote_projection::SelectedHostBridgeRuntime,
        geometry: GlassGeometry,
    ) {
        self.drain_worker_updates(state, surfaces);
        let next_source = if state.host_glass_enabled {
            match state.effective_sidebar_source() {
                crate::app::state::SidebarSource::Remote(host) => Some(host),
                crate::app::state::SidebarSource::Local => None,
            }
        } else {
            None
        };

        if self.source != next_source {
            if let Some(previous) = self.source.clone() {
                let retired = state.begin_host_glass_generation(previous.clone());
                let _ = state.set_host_glass_status(
                    &previous,
                    retired,
                    GlassStatus::Stale {
                        since: Instant::now(),
                    },
                    Some("glass detached; showing cached frame".into()),
                );
                if let Some(surface) = surfaces.get_mut(&previous) {
                    surface.retag_generation(retired);
                }
            }
            self.retire_stream();
            bridge_owner
                .release(crate::app::remote_projection::SelectedHostBridgeConsumer::HostGlass);
            self.source = next_source.clone();
            self.signature = None;
            if let Some(host) = next_source {
                let has_cached_frame = surfaces.get(&host).is_some_and(GlassSurfaceCore::has_frame);
                self.generation = state.begin_host_glass_generation(host.clone());
                if let Some(surface) = surfaces.get_mut(&host) {
                    surface.retag_generation(self.generation);
                }
                if has_cached_frame {
                    let _ = state.set_host_glass_status(
                        &host,
                        self.generation,
                        GlassStatus::Stale {
                            since: Instant::now(),
                        },
                        Some("cached frame; reconnecting host glass stream".into()),
                    );
                }
            } else {
                self.generation = 0;
            }
        }

        let Some(host) = self.source.clone() else {
            return;
        };
        if !geometry.usable() {
            return;
        }

        let had_cached_frame = surfaces.get(&host).is_some_and(GlassSurfaceCore::has_frame);
        match surfaces.get_mut(&host) {
            Some(surface) => {
                if surface.area() != geometry.area {
                    if let Err(err) = surface.resize(geometry.area) {
                        let _ = state.set_host_glass_status(
                            &host,
                            self.generation,
                            GlassStatus::Stale {
                                since: Instant::now(),
                            },
                            Some(format!("glass surface resize failed: {err}")),
                        );
                        return;
                    }
                }
            }
            None => match GlassSurfaceCore::new(geometry.area, self.generation) {
                Ok(surface) => surfaces.insert(host.clone(), surface),
                Err(err) => {
                    let _ = state.set_host_glass_status(
                        &host,
                        self.generation,
                        GlassStatus::Stale {
                            since: Instant::now(),
                        },
                        Some(format!("glass surface initialization failed: {err}")),
                    );
                    return;
                }
            },
        }

        let Some(prepared) = state.remote_sources.connected_bridge_state(&host) else {
            let status = if had_cached_frame {
                GlassStatus::Stale {
                    since: Instant::now(),
                }
            } else {
                GlassStatus::Connecting
            };
            let _ = state.set_host_glass_status(
                &host,
                self.generation,
                status,
                Some("waiting for supervisor-prepared no-start bridge state".into()),
            );
            self.retire_stream();
            bridge_owner
                .release(crate::app::remote_projection::SelectedHostBridgeConsumer::HostGlass);
            self.signature = None;
            return;
        };

        let signature = GlassSignature {
            host: host.clone(),
            prepared_shell_path: prepared.shell_path.clone(),
            geometry,
        };
        #[cfg(unix)]
        let connector_unchanged = bridge_owner.is_acquired_by(
            crate::app::remote_projection::SelectedHostBridgeConsumer::HostGlass,
            &host,
            &prepared,
        ) && self.stream.is_some();
        #[cfg(not(unix))]
        let connector_unchanged = true;
        if self.signature.as_ref() == Some(&signature) && connector_unchanged {
            return;
        }
        if self
            .signature
            .as_ref()
            .is_some_and(|existing| existing.same_stream_identity(&signature))
            && connector_unchanged
        {
            #[cfg(unix)]
            if let Some(stream) = self.stream.as_ref() {
                let _ = stream.resize(geometry);
            }
            self.signature = Some(signature);
            return;
        }

        if self.signature.is_some() {
            self.generation = state.begin_host_glass_generation(host.clone());
            if let Some(surface) = surfaces.get_mut(&host) {
                surface.retag_generation(self.generation);
            }
            self.retire_stream();
        }
        self.signature = Some(signature);

        #[cfg(not(unix))]
        {
            let _ = hosts;
            let _ = event_tx;
            let _ = state.set_host_glass_status(
                &host,
                self.generation,
                GlassStatus::Stale {
                    since: Instant::now(),
                },
                Some("host glass streaming is unsupported on Windows".into()),
            );
        }

        #[cfg(unix)]
        {
            let Some(host_config) = hosts
                .get(&host.host)
                .filter(|candidate| candidate.session == host.session)
            else {
                bridge_owner
                    .release(crate::app::remote_projection::SelectedHostBridgeConsumer::HostGlass);
                let _ = state.set_host_glass_status(
                    &host,
                    self.generation,
                    GlassStatus::Stale {
                        since: Instant::now(),
                    },
                    Some("selected remote host/session is no longer configured".into()),
                );
                return;
            };
            match bridge_owner.acquire(
                crate::app::remote_projection::SelectedHostBridgeConsumer::HostGlass,
                &host,
                host_config,
                &prepared,
            ) {
                Ok(socket) => {
                    self.stream = Some(spawn_glass_stream(
                        &socket,
                        host,
                        self.generation,
                        geometry,
                        event_tx.clone(),
                        self.worker_update_tx.clone(),
                    ));
                }
                Err(err) => {
                    let _ = state.set_host_glass_status(
                        &host,
                        self.generation,
                        GlassStatus::Stale {
                            since: Instant::now(),
                        },
                        Some(format!("glass bridge failed closed: {err}")),
                    );
                }
            }
        }
    }

    #[cfg(unix)]
    pub(crate) fn drain_worker_updates(
        &mut self,
        state: &mut crate::app::state::AppState,
        surfaces: &mut HostGlassSurfaceRegistry,
    ) {
        while let Ok(update) = self.worker_update_rx.try_recv() {
            match update {
                GlassWorkerUpdate::Frame {
                    host,
                    generation,
                    bytes,
                } => {
                    let current_generation = state
                        .host_glass_states
                        .get(&host)
                        .map(|glass| glass.generation);
                    if current_generation == Some(generation)
                        && surfaces.apply_frame(&host, generation, &bytes)
                    {
                        let _ =
                            state.set_host_glass_status(&host, generation, GlassStatus::Live, None);
                    }
                }
                GlassWorkerUpdate::Status {
                    host,
                    generation,
                    status,
                    message,
                } => {
                    // A retained frame remains truthfully stale until the
                    // replacement generation produces a fresh frame. Do not
                    // overwrite the visible cached-state cue with a transient
                    // Connecting update from the new worker.
                    if status == GlassStatus::Connecting
                        && surfaces.get(&host).is_some_and(GlassSurfaceCore::has_frame)
                        && state.host_glass_states.get(&host).is_some_and(|glass| {
                            glass.generation == generation
                                && matches!(glass.status, GlassStatus::Stale { .. })
                        })
                    {
                        continue;
                    }
                    let _ = state.set_host_glass_status(&host, generation, status, message);
                }
            }
        }
    }

    #[cfg(not(unix))]
    pub(crate) fn drain_worker_updates(
        &mut self,
        _state: &mut crate::app::state::AppState,
        _surfaces: &mut HostGlassSurfaceRegistry,
    ) {
    }

    fn retire_stream(&mut self) {
        #[cfg(unix)]
        if let Some(stream) = self.stream.take() {
            if let Err(err) = self.reaper.tx.send(stream) {
                // A dead process-lifetime reaper is an invariant violation.
                // Preserve event-loop responsiveness rather than running the
                // handle's bounded-but-nonzero teardown inline.
                std::mem::forget(err.0);
                tracing::error!("host glass stream reaper stopped unexpectedly");
            }
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) fn test_install_connected_stream(
        &mut self,
        host: RemoteHostKey,
        generation: u64,
        mouse_capture: Option<bool>,
    ) -> Receiver<crate::protocol::ClientMessage> {
        let (outbound, receiver) = mpsc::sync_channel(GLASS_OUTBOUND_CAPACITY);
        let outbound_gate = Arc::new(Mutex::new(Some(GlassOutboundAdmission {
            connection: 1,
            sender: outbound,
        })));
        self.source = Some(host.clone());
        self.generation = generation;
        self.stream = Some(GlassStreamHandle {
            outbound_gate,
            geometry: Arc::new(Mutex::new(GlassGeometry {
                area: Rect::new(0, 0, 80, 24),
                cell_width_px: 1,
                cell_height_px: 1,
            })),
            connected: Arc::new(AtomicBool::new(true)),
            mouse_capture: Arc::new(std::sync::atomic::AtomicU8::new(match mouse_capture {
                None => GLASS_MOUSE_CAPTURE_UNKNOWN,
                Some(false) => GLASS_MOUSE_CAPTURE_DISABLED,
                Some(true) => GLASS_MOUSE_CAPTURE_ENABLED,
            })),
            shutdown_stream: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicBool::new(true)),
            join: None,
        });
        self.signature = Some(GlassSignature {
            host,
            prepared_shell_path: "test".into(),
            geometry: GlassGeometry {
                area: Rect::new(0, 0, 80, 24),
                cell_width_px: 1,
                cell_height_px: 1,
            },
        });
        receiver
    }
}

impl Drop for HostGlassRuntime {
    fn drop(&mut self) {
        self.retire_stream();
    }
}

impl crate::app::App {
    /// Route one glass-directed structured input event. Returning true means
    /// the glass authority boundary consumed it even when delivery was
    /// intentionally dropped. Every drop raises the bounded local cue.
    pub(crate) fn route_host_glass_input(
        &mut self,
        event: crate::protocol::ClientInputEvent,
    ) -> bool {
        match self.host_glass_runtime.send_input(&self.state, event) {
            GlassInputOutcome::NotActive => false,
            GlassInputOutcome::Dropped(reason) => {
                if self.state.note_selected_host_glass_input_dropped(reason) {
                    self.host_glass_input_drop_cue_deadline =
                        Some(Instant::now() + super::HOST_GLASS_INPUT_DROP_CUE_DURATION);
                }
                true
            }
            #[cfg(unix)]
            GlassInputOutcome::Queued => true,
        }
    }
}

#[cfg(unix)]
fn emit_glass_update(
    worker_update_tx: &mpsc::Sender<GlassWorkerUpdate>,
    event_tx: &tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    update: GlassWorkerUpdate,
) -> bool {
    // This unbounded std channel is the one authoritative App-owned ordering
    // path for both bytes and lifecycle. Sending does not wait for the App
    // loop. The bounded Tokio event carries no state; it only wakes that loop.
    if worker_update_tx.send(update).is_err() {
        tracing::debug!("retired host glass worker-update receiver is closed");
        return false;
    }
    if let Err(err) = event_tx.try_send(crate::events::AppEvent::HostGlassWake) {
        tracing::debug!(%err, "host glass event queue unavailable");
        false
    } else {
        true
    }
}

#[cfg(unix)]
fn emit_glass_status(
    worker_update_tx: &mpsc::Sender<GlassWorkerUpdate>,
    event_tx: &tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    host: &RemoteHostKey,
    generation: u64,
    status: GlassStatus,
    message: Option<String>,
) {
    let update = GlassWorkerUpdate::Status {
        host: host.clone(),
        generation,
        status,
        message,
    };
    let _ = emit_glass_update(worker_update_tx, event_tx, update);
}

#[cfg(unix)]
fn spawn_glass_stream(
    socket: &Path,
    host: RemoteHostKey,
    generation: u64,
    geometry: GlassGeometry,
    event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    worker_update_tx: mpsc::Sender<GlassWorkerUpdate>,
) -> GlassStreamHandle {
    let outbound_gate = Arc::new(Mutex::new(None));
    let shared_geometry = Arc::new(Mutex::new(geometry));
    let connected = Arc::new(AtomicBool::new(false));
    let mouse_capture = Arc::new(std::sync::atomic::AtomicU8::new(
        GLASS_MOUSE_CAPTURE_UNKNOWN,
    ));
    let shutdown_stream = Arc::new(Mutex::new(None));
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let worker_geometry = Arc::clone(&shared_geometry);
    let worker_outbound_gate = Arc::clone(&outbound_gate);
    let worker_connected = Arc::clone(&connected);
    let worker_mouse_capture = Arc::clone(&mouse_capture);
    let worker_shutdown_stream = Arc::clone(&shutdown_stream);
    let worker_stop = Arc::clone(&stop);
    let worker_done = Arc::clone(&done);
    let worker_socket = socket.to_owned();
    let join = std::thread::spawn(move || {
        let mut backoff = GLASS_RECONNECT_INITIAL_BACKOFF;
        let mut first_attempt = true;
        let mut connection = 0_u64;
        while !worker_stop.load(Ordering::Acquire) {
            if first_attempt {
                emit_glass_status(
                    &worker_update_tx,
                    &event_tx,
                    &host,
                    generation,
                    GlassStatus::Connecting,
                    Some("opening no-start full-app TerminalAnsi stream".into()),
                );
                first_attempt = false;
            }
            let stream = match std::os::unix::net::UnixStream::connect(&worker_socket) {
                Ok(stream) => stream,
                Err(err) => {
                    let message = format!("glass stream connect failed: {err}");
                    emit_glass_status(
                        &worker_update_tx,
                        &event_tx,
                        &host,
                        generation,
                        GlassStatus::Stale {
                            since: Instant::now(),
                        },
                        Some(message),
                    );
                    if !wait_for_retry(&worker_stop, backoff) {
                        break;
                    }
                    backoff = (backoff * 2).min(GLASS_RECONNECT_MAX_BACKOFF);
                    continue;
                }
            };
            connection = connection.wrapping_add(1).max(1);
            let result = run_glass_stream_once_and_publish_lifecycle(
                stream,
                &host,
                generation,
                connection,
                &event_tx,
                &worker_outbound_gate,
                &worker_geometry,
                &worker_connected,
                &worker_mouse_capture,
                &worker_shutdown_stream,
                &worker_stop,
                &worker_update_tx,
            );
            worker_connected.store(false, Ordering::Release);
            match result {
                GlassStreamEnd::Stopped => break,
                GlassStreamEnd::Fatal(_) => break,
                GlassStreamEnd::Retryable(_) => {
                    if !wait_for_retry(&worker_stop, backoff) {
                        break;
                    }
                    backoff = (backoff * 2).min(GLASS_RECONNECT_MAX_BACKOFF);
                }
            }
        }
        worker_connected.store(false, Ordering::Release);
        worker_done.store(true, Ordering::Release);
    });

    GlassStreamHandle {
        outbound_gate,
        geometry: shared_geometry,
        connected,
        mouse_capture,
        shutdown_stream,
        stop,
        done,
        join: Some(join),
    }
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)] // Keeps the worker seam's shared generation resources explicit.
fn run_glass_stream_once_and_publish_lifecycle(
    stream: std::os::unix::net::UnixStream,
    host: &RemoteHostKey,
    generation: u64,
    connection: u64,
    event_tx: &tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    outbound_gate: &GlassOutboundGate,
    geometry: &Arc<Mutex<GlassGeometry>>,
    connected: &Arc<AtomicBool>,
    mouse_capture: &Arc<std::sync::atomic::AtomicU8>,
    shutdown_stream: &Arc<Mutex<Option<std::os::unix::net::UnixStream>>>,
    stop: &Arc<AtomicBool>,
    worker_update_tx: &mpsc::Sender<GlassWorkerUpdate>,
) -> GlassStreamEnd {
    let result = run_glass_stream_once(
        stream,
        host,
        generation,
        connection,
        event_tx,
        worker_update_tx,
        outbound_gate,
        geometry,
        connected,
        mouse_capture,
        shutdown_stream,
        stop,
    );
    match &result {
        GlassStreamEnd::Retryable(message) | GlassStreamEnd::Fatal(message) => {
            emit_glass_status(
                worker_update_tx,
                event_tx,
                host,
                generation,
                GlassStatus::Stale {
                    since: Instant::now(),
                },
                Some(message.clone()),
            );
        }
        GlassStreamEnd::Stopped => {}
    }
    result
}

#[cfg(unix)]
fn wait_for_retry(stop: &AtomicBool, duration: Duration) -> bool {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        std::thread::sleep(
            GLASS_BACKOFF_POLL.min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    !stop.load(Ordering::Acquire)
}

#[cfg(unix)]
#[allow(clippy::too_many_arguments)] // Keeps the worker's connection-scoped IO resources explicit.
fn run_glass_stream_once(
    mut stream: std::os::unix::net::UnixStream,
    host: &RemoteHostKey,
    generation: u64,
    connection: u64,
    event_tx: &tokio::sync::mpsc::Sender<crate::events::AppEvent>,
    worker_update_tx: &mpsc::Sender<GlassWorkerUpdate>,
    outbound_gate: &GlassOutboundGate,
    geometry: &Arc<Mutex<GlassGeometry>>,
    connected: &Arc<AtomicBool>,
    mouse_capture: &Arc<std::sync::atomic::AtomicU8>,
    shutdown_stream: &Arc<Mutex<Option<std::os::unix::net::UnixStream>>>,
    stop: &Arc<AtomicBool>,
) -> GlassStreamEnd {
    use crate::protocol::{
        RenderEncoding, ServerMessage, MAX_GRAPHICS_FRAME_SIZE, PROTOCOL_VERSION,
    };

    match stream.try_clone() {
        Ok(clone) => {
            *shutdown_stream
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(clone);
        }
        Err(err) => {
            return GlassStreamEnd::Retryable(format!("glass stream clone failed: {err}"));
        }
    }
    let _shutdown_registration = GlassShutdownRegistration {
        slot: shutdown_stream,
    };
    let hello = geometry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .hello();
    if let Err(err) = crate::protocol::write_message(&mut stream, &hello) {
        return GlassStreamEnd::Retryable(format!("glass handshake send failed: {err}"));
    }
    match crate::protocol::read_message::<_, ServerMessage>(
        &mut stream,
        crate::protocol::MAX_FRAME_SIZE,
    ) {
        Ok(ServerMessage::Welcome {
            version,
            encoding: RenderEncoding::TerminalAnsi,
            error: None,
        }) if version == PROTOCOL_VERSION => {}
        Ok(ServerMessage::Welcome {
            version,
            error: Some(error),
            ..
        }) => {
            return GlassStreamEnd::Fatal(format!(
                "glass handshake rejected (version {version}): {error}"
            ));
        }
        Ok(ServerMessage::Welcome {
            version, encoding, ..
        }) => {
            return GlassStreamEnd::Fatal(format!(
                "glass handshake mismatch: server version {version}, encoding {encoding:?}; expected version {PROTOCOL_VERSION}, TerminalAnsi"
            ));
        }
        Ok(_) => return GlassStreamEnd::Fatal("glass handshake expected Welcome".into()),
        Err(crate::protocol::FramingError::UnexpectedEof) => {
            return GlassStreamEnd::Retryable("glass handshake stream disconnected".into());
        }
        Err(crate::protocol::FramingError::Io(err)) => {
            return GlassStreamEnd::Retryable(format!("glass handshake read failed: {err}"));
        }
        Err(err) => return GlassStreamEnd::Fatal(format!("glass handshake invalid: {err}")),
    }

    mouse_capture.store(GLASS_MOUSE_CAPTURE_UNKNOWN, Ordering::Release);
    let mut write_stream = match stream.try_clone() {
        Ok(stream) => stream,
        Err(err) => {
            return GlassStreamEnd::Retryable(format!("glass stream clone failed: {err}"));
        }
    };
    let (outbound_sender, outbound_receiver) = mpsc::sync_channel(GLASS_OUTBOUND_CAPACITY);
    let writer_stop = Arc::new(AtomicBool::new(false));
    let writer_done = Arc::clone(&writer_stop);
    let writer_geometry = Arc::clone(geometry);
    let worker_stop = Arc::clone(stop);
    let writer = std::thread::spawn(move || {
        while !worker_stop.load(Ordering::Acquire) && !writer_done.load(Ordering::Acquire) {
            let message = outbound_receiver.recv_timeout(Duration::from_millis(50));
            match message {
                Ok(mut message) => {
                    if matches!(message, crate::protocol::ClientMessage::Resize { .. }) {
                        message = writer_geometry
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .resize();
                    }
                    if crate::protocol::write_message(&mut write_stream, &message).is_err() {
                        let _ = write_stream.shutdown(std::net::Shutdown::Both);
                        break;
                    }
                    if matches!(message, crate::protocol::ClientMessage::Detach) {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
    {
        let mut admission = outbound_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *admission = Some(GlassOutboundAdmission {
            connection,
            sender: outbound_sender,
        });
        connected.store(true, Ordering::Release);
    }

    let result = loop {
        if stop.load(Ordering::Acquire) {
            break GlassStreamEnd::Stopped;
        }
        match crate::protocol::read_message::<_, ServerMessage>(
            &mut stream,
            MAX_GRAPHICS_FRAME_SIZE,
        ) {
            Ok(ServerMessage::Terminal(frame)) => {
                if !emit_glass_update(
                    worker_update_tx,
                    event_tx,
                    GlassWorkerUpdate::Frame {
                        host: host.clone(),
                        generation,
                        bytes: frame.bytes,
                    },
                ) {
                    // Dropping one incremental TerminalAnsi frame would make
                    // every later delta ambiguous. Reconnect instead so the
                    // next stream starts from a fresh full-frame baseline.
                    break GlassStreamEnd::Retryable(
                        "host glass event queue saturated; restarting stream".into(),
                    );
                }
            }
            Ok(ServerMessage::Graphics { .. }) => {
                // MVP intentionally drops Kitty graphics. TerminalAnsi text
                // remains authoritative and continues streaming.
            }
            Ok(ServerMessage::MouseCapture { enabled }) => {
                mouse_capture.store(
                    if enabled {
                        GLASS_MOUSE_CAPTURE_ENABLED
                    } else {
                        GLASS_MOUSE_CAPTURE_DISABLED
                    },
                    Ordering::Release,
                );
            }
            Ok(ServerMessage::ServerShutdown { reason }) => {
                break GlassStreamEnd::Retryable(
                    reason.unwrap_or_else(|| "remote Herdr app stream closed".into()),
                );
            }
            Ok(ServerMessage::Welcome { .. }) | Ok(ServerMessage::Frame(_)) => {
                break GlassStreamEnd::Fatal(
                    "glass stream received a message incompatible with negotiated TerminalAnsi"
                        .into(),
                );
            }
            Ok(_) => {}
            Err(crate::protocol::FramingError::UnexpectedEof) => {
                break GlassStreamEnd::Retryable("remote glass stream disconnected".into());
            }
            Err(crate::protocol::FramingError::Io(err)) => {
                break GlassStreamEnd::Retryable(format!("remote glass stream failed: {err}"));
            }
            Err(err) => {
                break GlassStreamEnd::Fatal(format!("remote glass protocol failed: {err}"));
            }
        }
    };
    // Close admission before stopping or draining the per-connection writer.
    // The event loop's try-lock admission cannot enqueue after this point,
    // and the receiver is dropped with this connection, so no accepted input
    // can survive into a replacement connection.
    close_glass_admission(outbound_gate, Some(connection));
    connected.store(false, Ordering::Release);
    writer_stop.store(true, Ordering::Release);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = writer.join();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{
        CellData, ClientKeybindings, ClientLaunchMode, ClientMessage, CursorState, FrameData,
        RenderEncoding,
    };
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    fn cell(symbol: &str, fg: u32) -> CellData {
        CellData {
            symbol: symbol.to_owned(),
            fg,
            bg: 0,
            modifier: 0,
            skip: false,
            hyperlink: None,
        }
    }

    fn geometry(cols: u16, rows: u16) -> GlassGeometry {
        GlassGeometry {
            area: Rect::new(3, 2, cols, rows),
            cell_width_px: 9,
            cell_height_px: 18,
        }
    }

    #[test]
    fn full_app_terminal_ansi_handshake_uses_exact_content_geometry() {
        assert_eq!(
            geometry(91, 27).hello(),
            ClientMessage::Hello {
                version: crate::protocol::PROTOCOL_VERSION,
                cols: 91,
                rows: 27,
                cell_width_px: 9,
                cell_height_px: 18,
                requested_encoding: RenderEncoding::TerminalAnsi,
                keybindings: ClientKeybindings::Server,
                launch_mode: ClientLaunchMode::App,
            }
        );
    }

    #[test]
    #[cfg(unix)]
    fn live_input_is_structured_fire_and_forget_and_stale_input_never_replays() {
        let host = RemoteHostKey::new("remote-a", "default");
        let mut state = crate::app::state::AppState::test_new();
        state.host_glass_enabled = true;
        state.view.layout = crate::app::state::ViewLayout::Desktop;
        state.view.host_rail_rect = Rect::new(0, 0, 8, 24);
        state.select_sidebar_source(crate::app::state::SidebarSource::Remote(host.clone()));
        let generation = state.begin_host_glass_generation(host.clone());
        assert!(state.set_host_glass_status(&host, generation, GlassStatus::Live, None));

        let mut runtime = HostGlassRuntime::default();
        let receiver = runtime.test_install_connected_stream(host.clone(), generation, Some(true));
        let events = [
            crate::protocol::ClientInputEvent::Key {
                code: crate::protocol::ClientKeyCode::Char('x'),
                modifiers: 0,
                kind: crate::protocol::ClientKeyKind::Press,
            },
            crate::protocol::ClientInputEvent::Paste {
                text: "bracket me".into(),
            },
            crate::protocol::ClientInputEvent::Mouse {
                kind: crate::protocol::ClientMouseKind::ScrollDown,
                column: 7,
                row: 4,
                modifiers: 0,
            },
        ];
        for event in &events {
            assert_eq!(
                runtime.send_input(&state, event.clone()),
                GlassInputOutcome::Queued
            );
            assert_eq!(
                receiver.try_recv().expect("one queued structured event"),
                ClientMessage::InputEvents {
                    events: vec![event.clone()]
                }
            );
        }

        assert!(state.set_host_glass_status(
            &host,
            generation,
            GlassStatus::Stale {
                since: Instant::now(),
            },
            Some("link down".into()),
        ));
        assert_eq!(
            runtime.send_input(
                &state,
                crate::protocol::ClientInputEvent::Paste {
                    text: "must-not-replay".into(),
                },
            ),
            GlassInputOutcome::Dropped(GlassInputDropReason::Stale)
        );
        assert!(receiver.try_recv().is_err(), "stale input must not queue");

        assert!(state.set_host_glass_status(&host, generation, GlassStatus::Live, None));
        let fresh = crate::protocol::ClientInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Enter,
            modifiers: 0,
            kind: crate::protocol::ClientKeyKind::Press,
        };
        assert_eq!(
            runtime.send_input(&state, fresh.clone()),
            GlassInputOutcome::Queued
        );
        assert_eq!(
            receiver.try_recv().expect("fresh post-reconnect input"),
            ClientMessage::InputEvents {
                events: vec![fresh]
            }
        );
        assert!(receiver.try_recv().is_err(), "no stale replay may follow");
    }

    #[test]
    #[cfg(unix)]
    fn disconnect_drain_reconnect_admission_interleaving_cannot_replay_input() {
        let host = RemoteHostKey::new("remote-a", "default");
        let mut state = crate::app::state::AppState::test_new();
        state.host_glass_enabled = true;
        state.view.layout = crate::app::state::ViewLayout::Desktop;
        state.view.host_rail_rect = Rect::new(0, 0, 8, 24);
        state.select_sidebar_source(crate::app::state::SidebarSource::Remote(host.clone()));
        let generation = state.begin_host_glass_generation(host.clone());
        assert!(state.set_host_glass_status(&host, generation, GlassStatus::Live, None));

        let mut runtime = HostGlassRuntime::default();
        let old_receiver =
            runtime.test_install_connected_stream(host.clone(), generation, Some(true));
        let old = crate::protocol::ClientInputEvent::Paste {
            text: "old-connection".into(),
        };
        assert_eq!(
            runtime.send_input(&state, old.clone()),
            GlassInputOutcome::Queued
        );
        assert_eq!(
            old_receiver
                .try_recv()
                .expect("worker drains old connection queue"),
            ClientMessage::InputEvents { events: vec![old] }
        );
        assert!(old_receiver.try_recv().is_err());

        let stream = runtime.stream.as_ref().expect("test stream installed");
        let mut admission = stream
            .outbound_gate
            .lock()
            .expect("test outbound gate remains healthy");
        let old_admission = admission.take().expect("old connection admitted");

        // This is the exact former race window: the public connected flag and
        // App metadata still say Live, while the worker has closed admission
        // and is about to drain/drop the old receiver. The event loop never
        // waits for that critical section and cannot enqueue behind the drain.
        assert!(stream.connected.load(Ordering::Acquire));
        assert_eq!(
            runtime.send_input(
                &state,
                crate::protocol::ClientInputEvent::Paste {
                    text: "raced-after-drain".into(),
                },
            ),
            GlassInputOutcome::Dropped(GlassInputDropReason::AdmissionBusy)
        );

        let (new_sender, new_receiver) = mpsc::sync_channel(GLASS_OUTBOUND_CAPACITY);
        *admission = Some(GlassOutboundAdmission {
            connection: 2,
            sender: new_sender,
        });
        drop(old_admission);
        drop(admission);

        assert!(
            old_receiver.try_recv().is_err(),
            "raced input cannot appear behind the old drain"
        );
        assert!(
            new_receiver.try_recv().is_err(),
            "new connection starts empty"
        );

        let fresh = crate::protocol::ClientInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Enter,
            modifiers: 0,
            kind: crate::protocol::ClientKeyKind::Press,
        };
        assert_eq!(
            runtime.send_input(&state, fresh.clone()),
            GlassInputOutcome::Queued
        );
        assert_eq!(
            new_receiver.try_recv().expect("fresh input uses new queue"),
            ClientMessage::InputEvents {
                events: vec![fresh]
            }
        );
    }

    #[test]
    #[cfg(unix)]
    fn bounded_input_admission_reports_full_and_closed_without_queueing() {
        let host = RemoteHostKey::new("remote-a", "default");
        let mut state = crate::app::state::AppState::test_new();
        state.host_glass_enabled = true;
        state.view.layout = crate::app::state::ViewLayout::Desktop;
        state.view.host_rail_rect = Rect::new(0, 0, 8, 24);
        state.select_sidebar_source(crate::app::state::SidebarSource::Remote(host.clone()));
        let generation = state.begin_host_glass_generation(host.clone());
        assert!(state.set_host_glass_status(&host, generation, GlassStatus::Live, None));

        let mut runtime = HostGlassRuntime::default();
        let receiver = runtime.test_install_connected_stream(host, generation, Some(false));
        let event = crate::protocol::ClientInputEvent::Key {
            code: crate::protocol::ClientKeyCode::Char('x'),
            modifiers: 0,
            kind: crate::protocol::ClientKeyKind::Press,
        };
        for _ in 0..GLASS_OUTBOUND_CAPACITY {
            assert_eq!(
                runtime.send_input(&state, event.clone()),
                GlassInputOutcome::Queued
            );
        }
        assert_eq!(
            runtime.send_input(&state, event.clone()),
            GlassInputOutcome::Dropped(GlassInputDropReason::QueueFull)
        );

        drop(receiver);
        assert_eq!(
            runtime.send_input(&state, event.clone()),
            GlassInputOutcome::Dropped(GlassInputDropReason::QueueClosed)
        );
        assert_eq!(
            runtime.send_input(&state, event),
            GlassInputOutcome::Dropped(GlassInputDropReason::Disconnected)
        );
    }

    #[test]
    #[cfg(unix)]
    fn selected_live_stream_exposes_only_its_vt_reported_mouse_capture_mode() {
        let host = RemoteHostKey::new("remote-a", "default");
        let mut state = crate::app::state::AppState::test_new();
        state.host_glass_enabled = true;
        state.view.layout = crate::app::state::ViewLayout::Desktop;
        state.view.host_rail_rect = Rect::new(0, 0, 8, 24);
        state.select_sidebar_source(crate::app::state::SidebarSource::Remote(host.clone()));
        let generation = state.begin_host_glass_generation(host.clone());
        assert!(state.set_host_glass_status(&host, generation, GlassStatus::Live, None));

        let mut runtime = HostGlassRuntime::default();
        let _receiver = runtime.test_install_connected_stream(host.clone(), generation, Some(true));
        assert_eq!(runtime.selected_mouse_capture(&state), Some(true));

        assert!(state.set_host_glass_status(
            &host,
            generation,
            GlassStatus::Stale {
                since: Instant::now(),
            },
            None,
        ));
        assert_eq!(runtime.selected_mouse_capture(&state), None);
    }

    #[test]
    #[cfg(unix)]
    fn stream_consumes_server_mouse_capture_reports_without_wire_changes() {
        let (client, mut server) = std::os::unix::net::UnixStream::pair().expect("stream pair");
        let server = std::thread::spawn(move || {
            accept_test_hello(&mut server);
            crate::protocol::write_message(
                &mut server,
                &crate::protocol::ServerMessage::MouseCapture { enabled: true },
            )
            .expect("write mouse-capture report");
            crate::protocol::write_message(
                &mut server,
                &crate::protocol::ServerMessage::ServerShutdown {
                    reason: Some("capture observed".into()),
                },
            )
            .expect("close fake stream");
        });
        let host = RemoteHostKey::new("remote-a", "default");
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        let (worker_update_tx, _worker_update_rx) = mpsc::channel();
        let outbound_gate = Arc::new(Mutex::new(None));
        let shared_geometry = Arc::new(Mutex::new(geometry(12, 4)));
        let connected = Arc::new(AtomicBool::new(false));
        let mouse_capture = Arc::new(std::sync::atomic::AtomicU8::new(
            GLASS_MOUSE_CAPTURE_UNKNOWN,
        ));
        let shutdown_stream = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));

        let result = run_glass_stream_once(
            client,
            &host,
            9,
            1,
            &event_tx,
            &worker_update_tx,
            &outbound_gate,
            &shared_geometry,
            &connected,
            &mouse_capture,
            &shutdown_stream,
            &stop,
        );
        server.join().expect("fake server should finish");

        assert!(matches!(result, GlassStreamEnd::Retryable(_)));
        assert_eq!(
            mouse_capture.load(Ordering::Acquire),
            GLASS_MOUSE_CAPTURE_ENABLED
        );
    }

    #[cfg(unix)]
    fn run_fake_glass_stream(
        server: std::os::unix::net::UnixStream,
        client: std::os::unix::net::UnixStream,
        event_tx: tokio::sync::mpsc::Sender<crate::events::AppEvent>,
        worker_update_tx: mpsc::Sender<GlassWorkerUpdate>,
        server_body: impl FnOnce(std::os::unix::net::UnixStream) + Send + 'static,
    ) -> GlassStreamEnd {
        let server = std::thread::spawn(move || server_body(server));
        let host = RemoteHostKey::new("remote-a", "default");
        let outbound_gate = Arc::new(Mutex::new(None));
        let shared_geometry = Arc::new(Mutex::new(geometry(12, 4)));
        let connected = Arc::new(AtomicBool::new(false));
        let mouse_capture = Arc::new(std::sync::atomic::AtomicU8::new(
            GLASS_MOUSE_CAPTURE_UNKNOWN,
        ));
        let shutdown_stream = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let result = run_glass_stream_once_and_publish_lifecycle(
            client,
            &host,
            7,
            1,
            &event_tx,
            &outbound_gate,
            &shared_geometry,
            &connected,
            &mouse_capture,
            &shutdown_stream,
            &stop,
            &worker_update_tx,
        );
        server.join().expect("fake glass server should finish");
        result
    }

    fn state_with_glass_generation(
        host: &RemoteHostKey,
        generation: u64,
    ) -> crate::app::state::AppState {
        let mut state = crate::app::state::AppState::test_new();
        state.host_glass_states.insert(
            host.clone(),
            HostGlassState {
                generation,
                status: GlassStatus::Connecting,
                message: Some("test connection".into()),
                input_drop_cue: None,
            },
        );
        state
    }

    #[cfg(unix)]
    fn accept_test_hello(stream: &mut std::os::unix::net::UnixStream) {
        assert!(matches!(
            crate::protocol::read_message::<_, ClientMessage>(
                stream,
                crate::protocol::MAX_FRAME_SIZE
            )
            .expect("read glass hello"),
            ClientMessage::Hello {
                version: crate::protocol::PROTOCOL_VERSION,
                requested_encoding: RenderEncoding::TerminalAnsi,
                launch_mode: ClientLaunchMode::App,
                ..
            }
        ));
        crate::protocol::write_message(
            stream,
            &crate::protocol::ServerMessage::Welcome {
                version: crate::protocol::PROTOCOL_VERSION,
                encoding: RenderEncoding::TerminalAnsi,
                error: None,
            },
        )
        .expect("write glass welcome");
    }

    #[test]
    #[cfg(unix)]
    fn worker_frame_then_stale_drains_in_order_through_app_runtime() {
        let (client, server) = std::os::unix::net::UnixStream::pair().expect("stream pair");
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(8);
        let host = RemoteHostKey::new("remote-a", "default");
        let generation = 7;
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state.host_glass_states.insert(
            host.clone(),
            HostGlassState {
                generation,
                status: GlassStatus::Connecting,
                message: Some("test connection".into()),
                input_drop_cue: None,
            },
        );
        app.host_glass_surfaces.insert(
            host.clone(),
            GlassSurfaceCore::new(Rect::new(0, 0, 12, 4), generation).expect("test surface"),
        );
        let result = run_fake_glass_stream(
            server,
            client,
            event_tx,
            app.host_glass_runtime.worker_update_tx.clone(),
            |mut server| {
                accept_test_hello(&mut server);
                crate::protocol::write_message(
                    &mut server,
                    &crate::protocol::ServerMessage::Terminal(crate::protocol::TerminalFrame {
                        seq: 1,
                        width: 12,
                        height: 4,
                        full: true,
                        bytes: b"\x1b[2J\x1b[Hframe-from-remote".to_vec(),
                    }),
                )
                .expect("write terminal frame");
                crate::protocol::write_message(
                    &mut server,
                    &crate::protocol::ServerMessage::ServerShutdown {
                        reason: Some("test complete".into()),
                    },
                )
                .expect("close fake stream");
            },
        );
        assert!(matches!(result, GlassStreamEnd::Retryable(_)));

        let event = event_rx.blocking_recv().expect("worker wake hint");
        app.handle_internal_event(event);
        assert!(app
            .host_glass_surfaces
            .get(&host)
            .expect("surface retained")
            .visible_text()
            .starts_with("frame-from-r"));
        assert!(matches!(
            app.state
                .host_glass_states
                .get(&host)
                .map(|glass| glass.status),
            Some(GlassStatus::Stale { .. })
        ));
    }

    #[test]
    #[cfg(unix)]
    fn worker_connecting_then_frame_drains_to_live_through_app_runtime() {
        let host = RemoteHostKey::new("remote-a", "default");
        let generation = 7;
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state = state_with_glass_generation(&host, generation);
        app.host_glass_surfaces.insert(
            host.clone(),
            GlassSurfaceCore::new(Rect::new(0, 0, 12, 4), generation).expect("test surface"),
        );
        app.host_glass_runtime
            .worker_update_tx
            .send(GlassWorkerUpdate::Status {
                host: host.clone(),
                generation,
                status: GlassStatus::Connecting,
                message: Some("opening".into()),
            })
            .expect("queue connecting");
        app.host_glass_runtime
            .worker_update_tx
            .send(GlassWorkerUpdate::Frame {
                host: host.clone(),
                generation,
                bytes: b"first-live-frame".to_vec(),
            })
            .expect("queue frame");

        app.handle_internal_event(crate::events::AppEvent::HostGlassWake);

        assert_eq!(
            app.state
                .host_glass_states
                .get(&host)
                .map(|glass| glass.status),
            Some(GlassStatus::Live)
        );
        assert!(app
            .host_glass_surfaces
            .get(&host)
            .expect("surface retained")
            .visible_text()
            .starts_with("first-live-f"));
    }

    #[test]
    #[cfg(unix)]
    fn handshake_refusal_is_fatal_instead_of_silent_retry() {
        let (client, server) = std::os::unix::net::UnixStream::pair().expect("stream pair");
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        let mut runtime = HostGlassRuntime::default();
        let result = run_fake_glass_stream(
            server,
            client,
            event_tx,
            runtime.worker_update_tx.clone(),
            |mut server| {
                let _hello = crate::protocol::read_message::<_, ClientMessage>(
                    &mut server,
                    crate::protocol::MAX_FRAME_SIZE,
                )
                .expect("read glass hello");
                crate::protocol::write_message(
                    &mut server,
                    &crate::protocol::ServerMessage::Welcome {
                        version: crate::protocol::PROTOCOL_VERSION,
                        encoding: RenderEncoding::TerminalAnsi,
                        error: Some("version refused".into()),
                    },
                )
                .expect("write refusal");
            },
        );
        assert!(matches!(
            result,
            GlassStreamEnd::Fatal(message) if message.contains("version refused")
        ));
        let host = RemoteHostKey::new("remote-a", "default");
        let mut state = state_with_glass_generation(&host, 7);
        runtime.drain_worker_updates(&mut state, &mut HostGlassSurfaceRegistry::default());
        let glass = state.host_glass_states.get(&host).expect("glass state");
        assert!(matches!(glass.status, GlassStatus::Stale { .. }));
        assert!(glass
            .message
            .as_deref()
            .is_some_and(|message| message.contains("version refused")));
    }

    #[test]
    #[cfg(unix)]
    fn closed_runtime_stream_transitions_current_generation_to_stale() {
        let host = RemoteHostKey::new("remote-a", "default");
        let generation = 7;
        let mut state = state_with_glass_generation(&host, generation);
        let mut runtime = HostGlassRuntime::default();
        let (client, server) = std::os::unix::net::UnixStream::pair().expect("stream pair");
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        let result = run_fake_glass_stream(
            server,
            client,
            event_tx,
            runtime.worker_update_tx.clone(),
            |mut server| {
                accept_test_hello(&mut server);
                drop(server);
            },
        );
        assert!(matches!(result, GlassStreamEnd::Retryable(_)));
        runtime.drain_worker_updates(&mut state, &mut HostGlassSurfaceRegistry::default());
        let glass = state.host_glass_states.get(&host).expect("glass state");
        assert!(matches!(glass.status, GlassStatus::Stale { .. }));
        assert!(glass
            .message
            .as_deref()
            .is_some_and(|message| message.contains("disconnected")));
    }

    #[test]
    #[cfg(unix)]
    fn saturated_app_queue_still_delivers_stale_truth_through_runtime_mailbox() {
        let host = RemoteHostKey::new("remote-a", "default");
        let generation = 7;
        let mut app = crate::app::App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        );
        app.state = state_with_glass_generation(&host, generation);
        app.host_glass_surfaces.insert(
            host.clone(),
            GlassSurfaceCore::new(Rect::new(0, 0, 12, 4), generation).expect("test surface"),
        );
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel(1);
        event_tx
            .try_send(crate::events::AppEvent::HostGlassWake)
            .expect("fill bounded App queue");

        let (client, server) = std::os::unix::net::UnixStream::pair().expect("stream pair");
        let result = run_fake_glass_stream(
            server,
            client,
            event_tx,
            app.host_glass_runtime.worker_update_tx.clone(),
            |mut server| {
                accept_test_hello(&mut server);
                crate::protocol::write_message(
                    &mut server,
                    &crate::protocol::ServerMessage::Terminal(crate::protocol::TerminalFrame {
                        seq: 1,
                        width: 12,
                        height: 4,
                        full: true,
                        bytes: b"frame-that-cannot-be-queued".to_vec(),
                    }),
                )
                .expect("write terminal frame");
            },
        );
        assert!(matches!(
            result,
            GlassStreamEnd::Retryable(ref message) if message.contains("queue saturated")
        ));

        // The full Tokio queue carries only a wake. Handling that queued hint
        // drains the authoritative same-generation Frame -> Stale sequence.
        app.handle_internal_event(event_rx.blocking_recv().expect("queued wake"));
        assert!(app
            .host_glass_surfaces
            .get(&host)
            .expect("surface retained")
            .visible_text()
            .starts_with("frame-that-c"));
        let glass = app.state.host_glass_states.get(&host).expect("glass state");
        assert!(matches!(glass.status, GlassStatus::Stale { .. }));
        assert!(glass
            .message
            .as_deref()
            .is_some_and(|message| message.contains("queue saturated")));
    }

    #[test]
    #[cfg(unix)]
    fn retiring_stream_never_waits_for_worker_join_on_caller() {
        let outbound_gate = Arc::new(Mutex::new(None));
        let geometry = Arc::new(Mutex::new(geometry(12, 4)));
        let connected = Arc::new(AtomicBool::new(false));
        let shutdown_stream = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let worker_done = Arc::clone(&done);
        let (release_tx, release_rx) = mpsc::channel();
        let join = std::thread::spawn(move || {
            let _ = release_rx.recv();
            worker_done.store(true, Ordering::Release);
        });
        let handle = GlassStreamHandle {
            outbound_gate,
            geometry,
            connected,
            mouse_capture: Arc::new(std::sync::atomic::AtomicU8::new(
                GLASS_MOUSE_CAPTURE_UNKNOWN,
            )),
            shutdown_stream,
            stop,
            done: Arc::clone(&done),
            join: Some(join),
        };
        let mut runtime = HostGlassRuntime::default();
        runtime.stream = Some(handle);

        let (returned_tx, returned_rx) = mpsc::channel();
        let retire = std::thread::spawn(move || {
            runtime.retire_stream();
            let _ = returned_tx.send(());
            runtime
        });
        let returned = returned_rx.recv_timeout(Duration::from_millis(100));
        // Let the reaper's intentionally blocked test worker finish even when
        // the assertion below fails, keeping this test failure bounded.
        let _ = release_tx.send(());
        let runtime = retire.join().expect("retirement caller should finish");
        assert!(returned.is_ok(), "retirement waited for the worker join");
        let deadline = Instant::now() + Duration::from_secs(1);
        while !done.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(done.load(Ordering::Acquire));
        drop(runtime);
    }

    #[test]
    #[cfg(unix)]
    fn resize_is_forwarded_by_worker_writer_as_structured_message() {
        let (client, mut server) = std::os::unix::net::UnixStream::pair().expect("stream pair");
        let server = std::thread::spawn(move || {
            accept_test_hello(&mut server);
            let resize = crate::protocol::read_message::<_, ClientMessage>(
                &mut server,
                crate::protocol::MAX_FRAME_SIZE,
            )
            .expect("read forwarded resize");
            crate::protocol::write_message(
                &mut server,
                &crate::protocol::ServerMessage::ServerShutdown {
                    reason: Some("resize observed".into()),
                },
            )
            .expect("close fake stream");
            resize
        });

        let host = RemoteHostKey::new("remote-a", "default");
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(8);
        let (worker_update_tx, _worker_update_rx) = mpsc::channel();
        let outbound_gate = Arc::new(Mutex::new(None));
        let shared_geometry = Arc::new(Mutex::new(geometry(12, 4)));
        let connected = Arc::new(AtomicBool::new(false));
        let mouse_capture = Arc::new(std::sync::atomic::AtomicU8::new(
            GLASS_MOUSE_CAPTURE_UNKNOWN,
        ));
        let shutdown_stream = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_outbound_gate = Arc::clone(&outbound_gate);
        let worker_geometry = Arc::clone(&shared_geometry);
        let worker_connected = Arc::clone(&connected);
        let worker_mouse_capture = Arc::clone(&mouse_capture);
        let worker_shutdown = Arc::clone(&shutdown_stream);
        let worker_stop = Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            run_glass_stream_once(
                client,
                &host,
                9,
                1,
                &event_tx,
                &worker_update_tx,
                &worker_outbound_gate,
                &worker_geometry,
                &worker_connected,
                &worker_mouse_capture,
                &worker_shutdown,
                &worker_stop,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while !connected.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(connected.load(Ordering::Acquire));
        *shared_geometry
            .lock()
            .expect("test geometry lock should remain healthy") = geometry(100, 31);
        assert_eq!(
            try_admit_glass_message(&outbound_gate, geometry(100, 31).resize()),
            GlassAdmissionOutcome::Queued
        );

        assert_eq!(
            server.join().expect("fake server should finish"),
            ClientMessage::Resize {
                cols: 100,
                rows: 31,
                cell_width_px: 9,
                cell_height_px: 18,
            }
        );
        assert!(matches!(
            worker.join().expect("glass worker should finish"),
            GlassStreamEnd::Retryable(_)
        ));
    }

    #[test]
    fn handwritten_ansi_updates_grid_cursor_style_and_resize() {
        let mut glass = GlassSurfaceCore::new(Rect::new(1, 1, 12, 4), 7)
            .expect("PTY-free glass terminal should initialize");

        glass.feed(b"\x1b[2J\x1b[H\x1b[31mRED\x1b[0m\x1b[2;1Hna\xc3\xafve \xe6\x97\xa5\xe6\x9c\xac\x1b[3;5H\x1b[?25l");

        let text = glass.visible_text();
        assert!(text
            .lines()
            .next()
            .is_some_and(|line| line.starts_with("RED")));
        assert!(text
            .lines()
            .nth(1)
            .is_some_and(|line| line.starts_with("naïve 日本")));
        assert_eq!(
            glass.cursor_state(),
            Some(TerminalCursorState {
                x: 4,
                y: 2,
                visible: false,
                shape: 0,
            })
        );

        glass
            .resize(Rect::new(2, 1, 6, 2))
            .expect("PTY-free glass terminal should resize");
        glass.feed(b"\x1b[2J\x1b[HABC\x1b[2;1Hxy\x1b[?25h");

        assert_eq!(glass.generation(), 7);
        assert_eq!(glass.area(), Rect::new(2, 1, 6, 2));
        let resized = glass.visible_text();
        assert_eq!(resized.lines().next(), Some("ABC"));
        assert_eq!(resized.lines().nth(1), Some("xy"));
        assert_eq!(
            glass.cursor_state(),
            Some(TerminalCursorState {
                x: 2,
                y: 1,
                visible: true,
                shape: 0,
            })
        );

        let mut terminal = Terminal::new(TestBackend::new(10, 4)).expect("test terminal");
        terminal
            .draw(|frame| glass.render(frame, true))
            .expect("glass render should draw");
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(2, 1)].symbol(), "A");
        assert_eq!(buffer[(2, 2)].symbol(), "x");
        terminal.backend_mut().assert_cursor_position((4, 2));

        // Verify the earlier SGR was interpreted by the shared render seam on
        // a fresh surface, rather than merely retained as literal bytes.
        let styled = GlassSurfaceCore::new(Rect::new(0, 0, 3, 1), 8)
            .expect("styled glass terminal should initialize");
        styled.feed(b"\x1b[31mR\x1b[0m");
        let mut terminal = Terminal::new(TestBackend::new(3, 1)).expect("test terminal");
        terminal
            .draw(|frame| styled.render(frame, false))
            .expect("styled glass render should draw");
        assert_eq!(
            terminal.backend().buffer()[(0, 0)].style().fg,
            Some(Color::Indexed(1))
        );
    }

    #[test]
    fn captured_terminal_ansi_blit_dialect_round_trips_through_glass() {
        // Captured from BlitEncoder for the exact 2x1 frame below. Keeping the
        // bytes literal makes this a parser compatibility fixture rather than
        // merely feeding hand-authored ANSI through both sides of one test.
        const CAPTURED_TERMINAL_ANSI: &[u8] = b"\x1b[?25l\x1b[?2026h\x1b]8;;\x1b\\\x1b[2J\x1b[H\x1b[1;1H\x1b[0;31;49mA\x1b[1;2H\x1b[0;39;49mB\x1b[0m\x1b[1;2H\x1b[5 q\x1b[?25h\x1b[?2026l\x1b[1;2H\x1b[?25h";
        let frame = FrameData {
            cells: vec![cell("A", 2), cell("B", 0)],
            width: 2,
            height: 1,
            cursor: Some(CursorState {
                x: 1,
                y: 0,
                visible: true,
                shape: 5,
            }),
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        };
        let encoded = crate::protocol::render_ansi::BlitEncoder::new().encode(&frame, false);
        assert!(encoded.full);
        assert_eq!(encoded.bytes, CAPTURED_TERMINAL_ANSI);

        let glass = GlassSurfaceCore::new(Rect::new(0, 0, 2, 1), 11)
            .expect("captured-blit glass terminal should initialize");
        glass.feed(CAPTURED_TERMINAL_ANSI);

        let text = glass.visible_text();
        assert_eq!(text.lines().next(), Some("AB"));
        assert_eq!(
            glass.cursor_state(),
            Some(TerminalCursorState {
                x: 1,
                y: 0,
                visible: true,
                shape: 5,
            })
        );
    }

    #[test]
    fn repeated_first_attach_without_a_frame_remains_connecting() {
        let mut state = crate::app::state::AppState::test_new();
        state.host_glass_enabled = true;
        state.view.layout = crate::app::state::ViewLayout::Desktop;
        state.view.host_rail_rect = Rect::new(0, 0, 10, 20);
        let host = RemoteHostKey::new("remote-a", crate::session::DEFAULT_SESSION_NAME);
        state.select_sidebar_source(crate::app::state::SidebarSource::Remote(host.clone()));

        let mut surfaces = HostGlassSurfaceRegistry::default();
        let hosts = crate::remote_target::RemoteHostRegistry::default();
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(4);
        let mut bridges = crate::app::remote_projection::SelectedHostBridgeRuntime::default();
        let mut runtime = HostGlassRuntime::default();
        let geometry = GlassGeometry {
            area: Rect::new(10, 1, 50, 19),
            cell_width_px: 8,
            cell_height_px: 16,
        };

        for _ in 0..2 {
            runtime.reconcile(
                &mut state,
                &mut surfaces,
                &hosts,
                &event_tx,
                &mut bridges,
                geometry,
            );
            assert!(matches!(
                state.host_glass_states.get(&host).map(|glass| glass.status),
                Some(GlassStatus::Connecting)
            ));
            assert!(!surfaces
                .get(&host)
                .expect("first attach allocates its PTY-free surface")
                .has_frame());
        }
    }

    #[test]
    fn host_switch_retains_cached_surface_stale_until_a_fresh_frame() {
        let mut state = crate::app::state::AppState::test_new();
        state.host_glass_enabled = true;
        state.view.layout = crate::app::state::ViewLayout::Desktop;
        state.view.host_rail_rect = Rect::new(0, 0, 10, 20);
        let host = RemoteHostKey::new("remote-a", crate::session::DEFAULT_SESSION_NAME);
        state.select_sidebar_source(crate::app::state::SidebarSource::Remote(host.clone()));

        let mut surfaces = HostGlassSurfaceRegistry::default();
        let hosts = crate::remote_target::RemoteHostRegistry::default();
        let (event_tx, _event_rx) = tokio::sync::mpsc::channel(4);
        let mut bridges = crate::app::remote_projection::SelectedHostBridgeRuntime::default();
        let mut runtime = HostGlassRuntime::default();
        let geometry = GlassGeometry {
            area: Rect::new(10, 1, 50, 19),
            cell_width_px: 8,
            cell_height_px: 16,
        };

        runtime.reconcile(
            &mut state,
            &mut surfaces,
            &hosts,
            &event_tx,
            &mut bridges,
            geometry,
        );
        assert!(matches!(
            state.host_glass_states.get(&host).map(|glass| glass.status),
            Some(GlassStatus::Connecting)
        ));
        surfaces
            .get(&host)
            .expect("first selection creates a surface")
            .feed(b"\x1b[2J\x1b[HCACHED");

        state.select_sidebar_source(crate::app::state::SidebarSource::Local);
        runtime.reconcile(
            &mut state,
            &mut surfaces,
            &hosts,
            &event_tx,
            &mut bridges,
            geometry,
        );
        state.select_sidebar_source(crate::app::state::SidebarSource::Remote(host.clone()));
        runtime.reconcile(
            &mut state,
            &mut surfaces,
            &hosts,
            &event_tx,
            &mut bridges,
            geometry,
        );

        let retained = surfaces.get(&host).expect("cached surface retained");
        assert!(retained.visible_text().starts_with("CACHED"));
        assert!(matches!(
            state.host_glass_states.get(&host).map(|glass| glass.status),
            Some(GlassStatus::Stale { .. })
        ));

        #[cfg(unix)]
        {
            let generation = state
                .host_glass_states
                .get(&host)
                .expect("reselected generation")
                .generation;
            runtime
                .worker_update_tx
                .send(GlassWorkerUpdate::Status {
                    host: host.clone(),
                    generation,
                    status: GlassStatus::Connecting,
                    message: Some("replacement worker connecting".into()),
                })
                .expect("queue replacement connecting status");
            runtime.drain_worker_updates(&mut state, &mut surfaces);
            assert!(matches!(
                state.host_glass_states.get(&host).map(|glass| glass.status),
                Some(GlassStatus::Stale { .. })
            ));

            runtime
                .worker_update_tx
                .send(GlassWorkerUpdate::Frame {
                    host: host.clone(),
                    generation,
                    bytes: b"\x1b[2J\x1b[HFRESH".to_vec(),
                })
                .expect("queue fresh replacement frame");
            runtime.drain_worker_updates(&mut state, &mut surfaces);
            assert!(matches!(
                state.host_glass_states.get(&host).map(|glass| glass.status),
                Some(GlassStatus::Live)
            ));
            assert!(surfaces
                .get(&host)
                .expect("surface remains present")
                .visible_text()
                .starts_with("FRESH"));
        }
    }
}
