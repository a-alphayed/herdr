//! Per-host serial mutation executor and routed-request planner (app layer).
//!
//! Mutating routed remote requests (pane focus/focus-direction/split/close/
//! rename, tab create/focus/close/rename, workspace rename/create) are planned
//! on the event loop, then enqueued to ONE per-host serial executor: a bounded
//! FIFO (depth [`ROUTED_MUTATION_QUEUE_DEPTH`]) drained by one worker task per
//! host that exists only while its queue is non-empty. Execution order ==
//! submission order == completion order per host: no older-planned mutation
//! can execute its remote request after a newer one.
//!
//! Read-only routed requests (agent reads, standalone refreshes) do NOT
//! serialize: they keep the existing deferred-agent seam and its
//! `REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT` limiter concurrency. A mutating
//! sequence (primary op → `tab.list` → pure selection → `layout.export`) runs
//! inside ONE IO worker; the sequence holds ONE limiter permit across the
//! whole sequence and releases it on every path.
//!
//! **v11 contract** (see `.local/reviews/remote-fed-latency-reduction-v11.md`,
//! corrected by post-v11 review rounds): there is NO deadline on a
//! DISPATCHED worker's transport call — once a permit is held, transport
//! prep and the wait on the worker's notification channel (a plain blocking
//! `recv()`) are unbounded. Permit ACQUISITION ITSELF is bounded (FIX-1,
//! `ROUTED_PERMIT_WAIT_MAX`, currently 30s) — a narrower, categorically
//! different bound than the one v11 removed: it applies before any transport
//! exists (no child, no PID, no bytes), so its expiry never needs a kill or a
//! reconnect, only a `remote_bridge_busy` response (see `run_sequence`). What
//! bounds a DISPATCHED worker's hung request is the SSH transport's own
//! keepalive (see `crate::remote::routed`'s module documentation for the full
//! contract and its known wedged-remote-process limitation — corrected in
//! diff review round 4: `remote reconnect` cancels queued work but does NOT
//! release an in-flight wedged request). A primary-leg failure at or after
//! the first write attempt (including a primary response that fails to parse
//! — FIX-2, treated the same way even though the transport itself reported
//! `Ok`) resolves the caller with a single honest "outcome unknown, host is
//! reconnecting" error and triggers the existing supervisor reconnect
//! transition. That same failure ALSO synchronously drains and cancels every
//! queued descriptor for the host, on the executor's own worker thread,
//! before `run_worker`'s loop can dequeue the next one (FIX-1 — see
//! `RoutedExecutorPool::cancel_queued_for_recovery`); the App's own
//! `App::start_remote_lifecycle_reconnect` (reached both by an explicit
//! `remote.reconnect` and by the `RemoteRoutedRecoveryNeeded` escalation) and
//! the `RemoteSourceDisconnected` handler still call the equivalent
//! `App::cancel_queued_routed_for_reconnect`, but by then the queue is
//! already empty for this trigger — that call remains the ONLY drain for the
//! other two triggers. There is no mutation-quarantine gate; a mutation
//! arriving while the host is unreachable is already rejected by the
//! pre-existing connectivity precheck.

use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc as tokio_mpsc;

use super::agents_deferred::{remote_agent_bridge_limiter, RemoteAgentBridgeLimiter};
use super::remote_helpers::{
    remote_capability_unavailable_body, remote_route_plan_error_body,
    rewrite_remote_response_id_value,
};
use super::responses::{encode_error, encode_error_body};
use crate::api::schema::{ErrorBody, Method, Request, ResponseResult, SuccessResponse};
use crate::app::App;
use crate::events::AppEvent;
use crate::remote::{
    completion_from_parts, one_shot_full_request, one_shot_prepared_request,
    production_transport_starter, release_persistent_bridge_active, remote_agent_bridge_pool,
    return_persistent_bridge, routed_sequence_worker, synthetic_indeterminate_final,
    OneShotFullLeg, OneShotPreparedLeg, PrimaryResult, RefreshLegOutcome, RemoteAgentBridgePool,
    RemoteApiBridgeState, RoutedCompletion, RoutedRefreshSpec, RoutedSequenceSpec, SequenceFinal,
    WorkerFinished, WorkerTransport,
};
use crate::remote_source::RemoteHostKey;
use crate::remote_target::RemoteHostConfig;

/// Bounded FIFO depth per host for mutating routed sequences. Overflow
/// resolves the request immediately with a terminal busy error (unreachable
/// in interactive use; correct under pathological flooding).
pub(crate) const ROUTED_MUTATION_QUEUE_DEPTH: usize = 8;
/// `try_acquire` poll cadence inside the worker-side executor loop (off the
/// event loop).
const ROUTED_PERMIT_POLL: Duration = Duration::from_millis(25);
/// Continuous permit-acquisition failure window that triggers a starvation
/// warning log (bounded background agent traffic shares the limiter).
const ROUTED_PERMIT_STARVATION_AFTER: Duration = Duration::from_secs(5);
/// FIX-1 (post-v11 correction): the ONLY bounded wait left in this executor.
/// This is categorically different from the deadline v11 removed: it bounds
/// waiting for a per-host bridge PERMIT, before any transport exists — no
/// child, no PID, no bytes written, nothing to kill or classify. A saturated
/// limiter can occur in NORMAL operation (background agent dispatch sharing
/// the 4-wide limiter), not only against a frozen/wedged remote host, so it
/// must not be allowed to hang forever like the documented wedged-process
/// case. On expiry: no permit was ever held, so the descriptor resolves with
/// the existing `remote_bridge_busy` error and is dropped — never
/// `RemoteRoutedRecoveryNeeded`, never a reconnect. Nothing was dispatched
/// and the host itself may be perfectly healthy; it is busy, not broken.
const ROUTED_PERMIT_WAIT_MAX: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Completion sink
// ---------------------------------------------------------------------------

/// Exactly-once completion sink for one routed sequence. `ApiResponder`
/// resolves an API client's response channel; `UiCreate` resolves the remote
/// workspace-create UI flow through its tokenized `AppEvent`s (the same events
/// the inline worker emits today). Both variants are dropped-receiver safe.
pub(crate) enum CompletionSink {
    ApiResponder {
        respond_to: mpsc::Sender<String>,
        request_id: String,
    },
    UiCreate {
        token: u64,
    },
}

impl CompletionSink {
    fn resolve_success(self, primary: &str) {
        match self {
            Self::ApiResponder {
                respond_to,
                request_id,
            } => {
                let value = rewrite_remote_response_id_value(primary, &request_id)
                    .and_then(|value| {
                        serde_json::to_string(&value).map_err(|err| {
                            io::Error::new(io::ErrorKind::InvalidData, err.to_string())
                        })
                    })
                    .unwrap_or_else(|err| {
                        encode_error(request_id, "remote_request_failed", err.to_string())
                    });
                let _ = respond_to.send(value);
            }
            Self::UiCreate { .. } => {
                // Create sinks resolve through events (see resolve_create);
                // success application is the event handler's job.
            }
        }
    }

    fn resolve_error(self, code: &str, message: impl Into<String>) {
        match self {
            Self::ApiResponder {
                respond_to,
                request_id,
            } => {
                let _ = respond_to.send(encode_error(request_id, code, message));
            }
            Self::UiCreate { .. } => {}
        }
    }
}

/// Owned, self-contained description of one routed remote request sequence.
pub(crate) struct RoutedSequenceDescriptor {
    pub(crate) host: RemoteHostConfig,
    pub(crate) host_key: RemoteHostKey,
    /// Active supervisor generation at plan time; stamped on completion events
    /// and used for stale-completion rejection (state application only —
    /// response semantics follow the taxonomy).
    pub(crate) source_generation: Option<u64>,
    pub(crate) bridge_state: Option<RemoteApiBridgeState>,
    pub(crate) spec: RoutedSequenceSpec,
    /// Set by the dispatch site: the API seam fills the responder, internal
    /// TUI dispatches use a dropped-receiver channel (fire-and-forget), and
    /// the create flow uses the UI-event sink. `None` resolves nothing.
    pub(crate) sink: Option<CompletionSink>,
}

// ---------------------------------------------------------------------------
// Executor environment (injectable for tests)
// ---------------------------------------------------------------------------

pub(crate) type TransportStarterFn = fn(
    &'static RemoteAgentBridgePool,
    &RemoteHostConfig,
    Option<&RemoteApiBridgeState>,
    &Request,
) -> io::Result<WorkerTransport>;

/// Everything the executor worker threads need. Production is
/// [`RoutedEnv::production`]; tests inject local limiter/pool instances, fake
/// legs, and short timing knobs so no test touches real SSH or the process
/// globals.
pub(crate) struct RoutedEnv {
    pub(crate) limiter: &'static RemoteAgentBridgeLimiter,
    pub(crate) pool: &'static RemoteAgentBridgePool,
    pub(crate) transport_starter: TransportStarterFn,
    pub(crate) prepared_leg: OneShotPreparedLeg,
    pub(crate) full_leg: OneShotFullLeg,
    pub(crate) event_tx: tokio_mpsc::Sender<AppEvent>,
    pub(crate) permit_poll: Duration,
    pub(crate) starvation_after: Duration,
    /// FIX-1: bound on waiting for a per-host bridge permit only (see
    /// `ROUTED_PERMIT_WAIT_MAX`). Independent of, and much smaller in scope
    /// than, the per-sequence deadline v11 removed.
    pub(crate) permit_wait_max: Duration,
}

impl RoutedEnv {
    pub(crate) fn production(event_tx: tokio_mpsc::Sender<AppEvent>) -> Self {
        Self {
            limiter: remote_agent_bridge_limiter(),
            pool: remote_agent_bridge_pool(),
            transport_starter: production_transport_starter,
            prepared_leg: one_shot_prepared_request,
            full_leg: one_shot_full_request,
            event_tx,
            permit_poll: ROUTED_PERMIT_POLL,
            starvation_after: ROUTED_PERMIT_STARVATION_AFTER,
            permit_wait_max: ROUTED_PERMIT_WAIT_MAX,
        }
    }
}

// ---------------------------------------------------------------------------
// Per-host serial executor
// ---------------------------------------------------------------------------

struct HostQueue {
    queue: VecDeque<RoutedSequenceDescriptor>,
    worker_alive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoutedEnqueueError {
    /// FIFO overflow: immediate terminal busy error, nothing queued.
    Busy,
}

struct ExecutorInner {
    hosts: BTreeMap<RemoteHostKey, HostQueue>,
}

pub(crate) struct RoutedExecutorPool {
    inner: Mutex<ExecutorInner>,
    pub(crate) env: RoutedEnv,
}

impl RoutedExecutorPool {
    pub(crate) fn new(env: RoutedEnv) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(ExecutorInner {
                hosts: BTreeMap::new(),
            }),
            env,
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ExecutorInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Atomic enqueue-or-spawn under ONE per-host lock (the same lock the
    /// worker uses to check-empty-and-mark-dead, so no lost wakeup and no
    /// double worker): a depth-0→non-empty transition spawns the worker.
    pub(crate) fn enqueue(
        self: &Arc<Self>,
        descriptor: RoutedSequenceDescriptor,
    ) -> Result<(), RoutedEnqueueError> {
        let host_key = descriptor.host_key.clone();
        let spawn = {
            let mut inner = self.lock();
            let host = inner
                .hosts
                .entry(host_key.clone())
                .or_insert_with(|| HostQueue {
                    queue: VecDeque::new(),
                    worker_alive: false,
                });
            if host.queue.len() >= ROUTED_MUTATION_QUEUE_DEPTH {
                return Err(RoutedEnqueueError::Busy);
            }
            host.queue.push_back(descriptor);
            let spawn = !host.worker_alive;
            host.worker_alive = true;
            spawn
        };
        if spawn {
            let pool = Arc::clone(self);
            std::thread::spawn(move || pool.run_worker(host_key));
        }
        Ok(())
    }

    /// Cancel a STILL-QUEUED create descriptor (UI token expiry cancel hook).
    /// Returns the removed descriptor so the caller resolves it
    /// cancelled-before-write (it never executes late); an EXECUTING
    /// descriptor cannot be retracted and runs to completion (there is no
    /// deadline to bound it — see the module contract).
    pub(crate) fn cancel_queued_create(
        &self,
        host: &RemoteHostKey,
        token: u64,
    ) -> Option<RoutedSequenceDescriptor> {
        let mut inner = self.lock();
        let host_queue = inner.hosts.get_mut(host)?;
        let position = host_queue.queue.iter().position(|descriptor| {
            matches!(
                &descriptor.sink,
                Some(CompletionSink::UiCreate { token: pending }) if *pending == token
            )
        })?;
        host_queue.queue.remove(position)
    }

    /// H6 (plan v6/v8: a generation bump cancels ALL queued descriptors as
    /// cancelled-before-write): drain every STILL-QUEUED descriptor for
    /// `host` — pane/tab/workspace mutations and any queued create alike.
    /// The already-EXECUTING descriptor (if any) is untouched — it runs to
    /// completion (there is no deadline to bound it — see the module
    /// contract); only descriptors that never left the FIFO are stale
    /// relative to the retired generation. Callers resolve each returned
    /// descriptor's sink as cancelled (never silently drop it — that would
    /// leak a pending API responder or strand the create's spinner). REQ-2:
    /// called from both `App::start_remote_lifecycle_reconnect` (the
    /// `remote.reconnect` transition itself, reached either by an explicit
    /// user-issued reconnect or by the internal `RemoteRoutedRecoveryNeeded`
    /// escalation) and the existing `RemoteSourceDisconnected` handler, so a
    /// stale descriptor can never execute around either recovery path.
    pub(crate) fn cancel_all_queued(&self, host: &RemoteHostKey) -> Vec<RoutedSequenceDescriptor> {
        let mut inner = self.lock();
        let Some(host_queue) = inner.hosts.get_mut(host) else {
            return Vec::new();
        };
        std::mem::take(&mut host_queue.queue).into()
    }

    /// FIX-1 (diff review round 4, blocker 1 — recovery race): drain and
    /// resolve every STILL-QUEUED descriptor for `host` SYNCHRONOUSLY, on
    /// the executor's own worker thread, before `run_worker`'s loop can
    /// dequeue the next one. Waiting for the App to process the resulting
    /// `RemoteRoutedRecoveryNeeded` event (which drives the equivalent
    /// `App::cancel_queued_routed_for_reconnect`) races the executor's own
    /// `run_worker` loop — that loop advances to the next `pop_front()` the
    /// instant `run_sequence`/`resolve_normal` returns, so a next-queued
    /// descriptor could begin permit acquisition before the App ever gets to
    /// it. This method uses `self.env.event_tx` (already held by the
    /// executor for `RemoteRoutedRecoveryNeeded` itself) to resolve each
    /// drained sink directly, with NO dependency on the App loop's timing.
    /// The App's own later call to `cancel_queued_routed_for_reconnect` (for
    /// the SAME `RemoteRoutedRecoveryNeeded` event, and for the separate
    /// `RemoteSourceDisconnected`/explicit-`remote.reconnect` paths) stays in
    /// place and is harmless here: by the time it runs, this scoped drain
    /// has already removed everything it would have removed too.
    ///
    /// LOW-3 (diff review round 5, Reviewer A): scoped to descriptors whose
    /// captured `source_generation` is no NEWER than `resolving_generation`
    /// (the resolving sequence's own generation) — mirroring the App-side
    /// generation-admission gate (`remote_source_generation_is_active`,
    /// `api.rs`). Without this, a mutation planned and enqueued AFTER an
    /// intervening `remote.reconnect` already replaced this stale supervisor
    /// (the previous, unscoped `cancel_all_queued(host)` call this method
    /// used to make) would be spuriously swept up and cancelled by a LATER
    /// generation-N sequence that is only NOW resolving indeterminate (e.g.
    /// the documented wedged-process case, which can take an arbitrarily
    /// long time to resolve) even though it targets the fresh, healthy
    /// connection. A descriptor with no captured generation
    /// (`source_generation: None`, no active supervisor handle was found at
    /// plan time) is conservatively treated as eligible — the same
    /// unscoped-equivalent behavior that case already had.
    fn cancel_queued_for_recovery(&self, host: &RemoteHostKey, resolving_generation: u64) {
        let cancelled = {
            let mut inner = self.lock();
            let Some(host_queue) = inner.hosts.get_mut(host) else {
                return;
            };
            let mut remaining = VecDeque::with_capacity(host_queue.queue.len());
            let mut cancelled = Vec::new();
            for descriptor in std::mem::take(&mut host_queue.queue) {
                if descriptor.source_generation.unwrap_or(0) <= resolving_generation {
                    cancelled.push(descriptor);
                } else {
                    remaining.push_back(descriptor);
                }
            }
            host_queue.queue = remaining;
            cancelled
        };
        for descriptor in cancelled {
            match descriptor.sink {
                Some(CompletionSink::UiCreate { token }) => {
                    resolve_create_failure(
                        &self.env,
                        host.clone(),
                        token,
                        "remote host generation changed before this request executed; safe to retry"
                            .to_string(),
                    );
                }
                Some(CompletionSink::ApiResponder {
                    respond_to,
                    request_id,
                }) => {
                    let _ = respond_to.send(encode_error(
                        request_id,
                        "remote_request_cancelled",
                        "remote host generation changed before this request executed; safe to retry",
                    ));
                }
                None => {}
            }
        }
    }

    /// The per-host worker task: drains the FIFO one full sequence at a time.
    /// Exists only while the queue is non-empty; marks itself dead under the
    /// same lock as enqueue (atomic enqueue-or-spawn / check-empty-and-mark-dead).
    /// No quarantine gate: a mutation arriving while the host is unreachable
    /// is already rejected by the pre-existing connectivity precheck.
    fn run_worker(self: Arc<Self>, host_key: RemoteHostKey) {
        loop {
            let next = {
                let mut inner = self.lock();
                match inner.hosts.get_mut(&host_key) {
                    Some(host_queue) => {
                        if host_queue.queue.is_empty() {
                            // check-empty-and-mark-dead under the SAME lock as
                            // enqueue-or-spawn: no lost wakeup (an enqueue
                            // either sees worker_alive and skips spawning, or
                            // runs after we marked dead and spawns a fresh
                            // worker) and no double worker.
                            host_queue.worker_alive = false;
                            return;
                        }
                        host_queue.queue.pop_front()
                    }
                    None => return,
                }
            };
            if let Some(descriptor) = next {
                self.run_sequence(&host_key, descriptor);
            }
        }
    }

    /// Execute one full sequence: permit → transport prep → IO worker →
    /// blocking wait → resolve/emit. The permit is acquired once per sequence
    /// and released on every path (RAII). There is no deadline anywhere past
    /// this point (see the module contract): once a permit is held, transport
    /// prep and the wait on the worker's notification channel are unbounded.
    /// Permit ACQUISITION itself is bounded (FIX-1, `permit_wait_max`) — a
    /// narrower, categorically different bound than the one v11 removed: it
    /// applies before any transport exists (no child, no PID, no bytes), so
    /// expiry never needs a kill or a reconnect, only a busy response.
    fn run_sequence(
        self: &Arc<Self>,
        host_key: &RemoteHostKey,
        descriptor: RoutedSequenceDescriptor,
    ) {
        // 1. Permit acquisition (worker-side loop, off the event loop; 25ms
        //    try_acquire poll; 5s continuous-failure starvation warning;
        //    bounded by `permit_wait_max` — see FIX-1 above).
        let permit_wait_deadline = Instant::now() + self.env.permit_wait_max;
        let permit = {
            let mut starving_since: Option<Instant> = None;
            loop {
                match self.env.limiter.try_acquire(host_key) {
                    Some(permit) => break permit,
                    None => {
                        if Instant::now() >= permit_wait_deadline {
                            tracing::warn!(
                                event = "remote.route.permit_wait_expired",
                                subsystem = "remote",
                                host = %host_key.host,
                                session = %host_key.session,
                                wait = ?self.env.permit_wait_max,
                                "routed executor gave up waiting for the per-host bridge permit; resolving busy (no transport was ever opened, host status is not affected)"
                            );
                            resolve_sink_terminal(
                                &self.env,
                                descriptor,
                                TerminalOutcome::PermitWaitExpired,
                            );
                            return;
                        }
                        let first_fail = *starving_since.get_or_insert_with(Instant::now);
                        if first_fail.elapsed() >= self.env.starvation_after {
                            tracing::warn!(
                                event = "remote.route.permit_starvation",
                                subsystem = "remote",
                                host = %host_key.host,
                                session = %host_key.session,
                                "routed executor failed to acquire the per-host bridge permit for over 5s; background agent dispatch may be saturating the shared limiter"
                            );
                            starving_since = Some(Instant::now());
                        }
                        std::thread::sleep(self.env.permit_poll);
                    }
                }
            }
        };

        // 2. Transport prep (pooled-first routing, pre-write one-shot
        //    fallback) — blocking bridge establishment happens here on the
        //    executor worker thread, never on the event loop.
        let transport = match (self.env.transport_starter)(
            self.env.pool,
            &descriptor.host,
            descriptor.bridge_state.as_ref(),
            &descriptor.spec.primary,
        ) {
            Ok(transport) => transport,
            Err(err) => {
                resolve_sink_terminal(
                    &self.env,
                    descriptor,
                    TerminalOutcome::PreWriteFailure(err.to_string()),
                );
                return;
            }
        };
        let was_pooled = matches!(transport, WorkerTransport::Pooled(_));
        let layout_export_capable = descriptor.spec.layout_export_capable;

        // 3. Move the transport into the IO worker BEFORE any blocking call.
        let (tx, rx) = mpsc::channel();
        let spec = descriptor.spec.clone();
        let prepared_leg = self.env.prepared_leg;
        let full_leg = self.env.full_leg;
        std::thread::spawn(move || {
            routed_sequence_worker(prepared_leg, full_leg, transport, spec, tx);
        });

        // 4. Unbounded wait on the notification channel: the worker sends
        //    exactly one message when it finishes — there is NO timeout
        //    here, deliberately (v11 dropped the deadline/kill/PID/
        //    classification machinery; see the module contract). What
        //    bounds a hung request is the SSH transport's own keepalive
        //    (~10s to detect a dead peer). KNOWN LIMITATION, stated plainly
        //    (corrected, diff review round 4 blocker 3 — a prior version of
        //    this comment falsely claimed `remote reconnect` clears a
        //    wedged host): keepalive detects a dead connection or dead
        //    `sshd`, but NOT a *wedged* remote herdr process (e.g. stopped
        //    by `SIGSTOP`) — keepalives keep succeeding and this `recv()`
        //    blocks forever until the connection is otherwise broken.
        //    `herdr remote reconnect <host>` cancels every STILL-QUEUED
        //    descriptor for the host (`RoutedExecutorPool::cancel_all_queued`)
        //    and starts a fresh supervisor, but it does NOT release THIS
        //    blocked `recv()`: the lifecycle drain a reconnect runs only
        //    reaps IDLE pooled bridges (`RemoteAgentBridgePool::drain_host`,
        //    `src/remote/unix.rs`) — the active checked-out one backing this
        //    worker is left to finish on its own. The host's mutation queue
        //    stays genuinely blocked until the remote process resumes and
        //    answers, or the connection actually breaks; restarting the
        //    local herdr server is the unconditional fix. `Err` below means
        //    the worker thread exited (a panic) without ever publishing a
        //    real final outcome; treated exactly like any other
        //    at-or-after-write failure.
        let WorkerFinished { final_, pooled } = match rx.recv() {
            Ok(finished) => finished,
            Err(_) => WorkerFinished {
                final_: synthetic_indeterminate_final(
                    "routed worker exited without a final message",
                ),
                pooled: None,
            },
        };

        // REQ-3: never park a connection unless the sequence was a CLEAN
        // success — primary Ok, every attempted refresh leg Ok, and the
        // primary parses. Anything else drops the connection and releases
        // its active slot exactly once; only a genuine clean success returns
        // it to the pool. `None if was_pooled` covers the panic path above
        // (the worker never sent a `pooled` handle back at all), which still
        // reserved an active slot that must be released exactly once.
        let clean = sequence_is_clean_success(&final_, layout_export_capable);
        match pooled {
            Some(pooled) if !clean => {
                drop(pooled);
                release_persistent_bridge_active(self.env.pool, host_key);
            }
            Some(pooled) => {
                return_persistent_bridge(self.env.pool, pooled);
            }
            None if was_pooled => {
                release_persistent_bridge_active(self.env.pool, host_key);
            }
            None => {}
        }
        self.resolve_normal(host_key, descriptor, &final_);
        drop(permit);
    }
}

/// REQ-3 (round-3 blocker 4): whether a finished sequence was a CLEAN
/// success — the only condition under which a pooled connection is trusted
/// enough to return to the pool. Primary must be `Ok` AND parse as either a
/// genuine success OR a genuine authoritative error envelope (parsed BEFORE
/// disposal, so malformed data counts as unclean even though the transport
/// layer itself reported `Ok`) — FIX-2 correction, diff review round 5
/// blocker 1: an authoritative error (e.g. `pane_not_found`) means the
/// remote answered correctly and definitively; the TRANSPORT is healthy even
/// though the specific operation was rejected, so the connection stays
/// trusted for reuse exactly like a genuine success. AND every refresh leg
/// that was actually attempted must have succeeded: a failed `tab.list`
/// (`RefreshLegOutcome::Failed`) or a `layout.export` leg that was capable
/// and attempted (an active tab was selected) but silently returned no
/// layout are both "not clean" (refresh legs never run at all for an
/// authoritative error — see `remote::routed::routed_sequence_worker` — so
/// this branch is moot for that case). Anything not clean drops the
/// connection instead of parking it — this governs pool hygiene only; it
/// does not change the caller-facing taxonomy (a refresh-leg failure with a
/// successful primary still reports `PrimarySuccessPreserved` to the caller,
/// see `taxonomy_choice`/`resolve_normal` below — the pooled connection is
/// simply no longer trusted for reuse).
fn sequence_is_clean_success(final_: &SequenceFinal, layout_export_capable: bool) -> bool {
    let Ok(primary) = final_.primary.as_ref() else {
        return false;
    };
    if !crate::remote::primary_response_parses(primary)
        && !crate::remote::primary_response_is_error(primary)
    {
        return false;
    }
    match &final_.refresh {
        RefreshLegOutcome::None => true,
        RefreshLegOutcome::Failed => false,
        RefreshLegOutcome::Data(data) => match &data.active_tab {
            Some(fetch) => !layout_export_capable || fetch.layout.is_some(),
            None => true,
        },
    }
}

/// Terminal sink outcome for executor-side resolutions. `PreWriteFailure` is
/// the transport-starter step failing before a worker was ever spawned.
/// `PermitWaitExpired` (FIX-1) is permit ACQUISITION giving up before a
/// worker was ever spawned — no transport was opened either, so like
/// `PreWriteFailure` it is purely a "nothing happened" resolution, never a
/// reconnect trigger. Every other terminal outcome (indeterminate, success)
/// is resolved directly by `resolve_normal` from the worker's own result.
enum TerminalOutcome {
    PreWriteFailure(String),
    PermitWaitExpired,
}

/// H5: `CompletionSink::resolve_success`/`resolve_error` are deliberate
/// no-ops for `UiCreate` (a create resolves through its own tokenized
/// `AppEvent`s, never through the generic sink). Every terminal path below
/// must therefore pair a `UiCreate` sink with the RIGHT create event itself —
/// a bare `sink.resolve_error(...)`/`resolve_success(...)` call alone
/// silently strands the pending create spinner forever for that branch.
fn resolve_sink_terminal(
    env: &RoutedEnv,
    descriptor: RoutedSequenceDescriptor,
    outcome: TerminalOutcome,
) {
    let RoutedSequenceDescriptor {
        host_key,
        source_generation: _,
        sink,
        ..
    } = descriptor;
    let Some(sink) = sink else {
        return;
    };
    match outcome {
        TerminalOutcome::PreWriteFailure(message) => match sink {
            CompletionSink::ApiResponder { .. } => {
                sink.resolve_error("remote_request_failed", message);
            }
            CompletionSink::UiCreate { token } => {
                resolve_create_failure(env, host_key, token, message);
            }
        },
        TerminalOutcome::PermitWaitExpired => {
            // FIX-1: the SAME busy signal the FIFO-overflow path already
            // uses (`RoutedEnqueueError::Busy`) — no permit was ever held,
            // no transport was ever opened, nothing to retry-unsafely repeat.
            let message =
                "remote host mutation queue is saturated (permit wait exceeded); retry shortly";
            match sink {
                CompletionSink::ApiResponder { .. } => {
                    sink.resolve_error("remote_bridge_busy", message);
                }
                CompletionSink::UiCreate { token } => {
                    resolve_create_failure(env, host_key, token, message.to_string());
                }
            }
        }
    }
}

/// Emit the terminal create event for a definitive failure (pre-write
/// failure or an indeterminate outcome): never a silent no-op that strands
/// the pending create spinner (H5).
fn resolve_create_failure(env: &RoutedEnv, host_key: RemoteHostKey, token: u64, message: String) {
    let _ = env
        .event_tx
        .blocking_send(AppEvent::RemoteWorkspaceCreateFailed {
            host: host_key,
            token,
            message,
        });
}

/// Normal completion: resolve the sink from the worker's final outcome and
/// emit the generation-stamped completion event (state application through
/// the reducer only). A create (`UiCreate`) sink resolves through the create
/// events instead.
impl RoutedExecutorPool {
    /// REQ-4/REQ-5: a post-write transport/protocol error (EOF, broken pipe,
    /// or any other primary-leg failure at or after the first write attempt
    /// — there is no deadline anywhere in this executor, see the module
    /// contract) is a case with no confirmed final outcome, so it triggers
    /// the reconnect escalation below (`RemoteRoutedRecoveryNeeded`, which
    /// also cancels every queued descriptor for the host — see
    /// `RoutedExecutorPool::cancel_all_queued`). There is no mutation-
    /// quarantine gate: a mutation arriving while the host is unreachable is
    /// already rejected by the pre-existing connectivity precheck.
    fn resolve_normal(
        self: &Arc<Self>,
        host_key: &RemoteHostKey,
        descriptor: RoutedSequenceDescriptor,
        final_: &SequenceFinal,
    ) {
        let stale_workspace_id = descriptor
            .spec
            .refresh
            .as_ref()
            .map(|refresh| refresh.workspace_id.clone());
        let apply = descriptor.spec.apply.clone();
        let completion_event = if matches!(descriptor.sink, Some(CompletionSink::UiCreate { .. })) {
            // Create sinks resolve through the create events (which apply the
            // workspace cache), not the generic completion event.
            None
        } else {
            completion_from_parts(final_, stale_workspace_id, apply)
        };

        let RoutedSequenceDescriptor {
            host_key: descriptor_host_key,
            source_generation,
            sink,
            ..
        } = descriptor;
        let generation = source_generation.unwrap_or(0);
        let env = &self.env;
        let taxonomy = taxonomy_choice(final_);
        if taxonomy == TaxonomyChoice::Indeterminate {
            let _ = env
                .event_tx
                .blocking_send(AppEvent::RemoteRoutedRecoveryNeeded {
                    host: host_key.clone(),
                    generation,
                });
            // FIX-1: drain and resolve every still-queued descriptor for
            // this host (as old or older than this generation — LOW-3)
            // RIGHT HERE, synchronously, before returning — see
            // `cancel_queued_for_recovery`'s doc comment for why waiting for
            // the App to process the event above is a race in production.
            self.cancel_queued_for_recovery(host_key, generation);
        }
        let Some(sink) = sink else {
            return;
        };
        match taxonomy {
            TaxonomyChoice::PreWriteFailure => match sink {
                CompletionSink::ApiResponder { .. } => {
                    sink.resolve_error("remote_request_failed", final_.primary_error_message());
                }
                CompletionSink::UiCreate { token } => {
                    // H5: never a silent no-op — the create definitely never
                    // reached the remote (pre-write), resolve it as a plain
                    // failure so the spinner clears and a retry can proceed.
                    resolve_create_failure(
                        env,
                        descriptor_host_key.clone(),
                        token,
                        final_.primary_error_message(),
                    );
                }
            },
            TaxonomyChoice::Completed | TaxonomyChoice::PrimaryPreserved => match sink {
                CompletionSink::ApiResponder { .. } => {
                    if let Ok(primary) = final_.primary.as_ref() {
                        sink.resolve_success(primary);
                    }
                }
                CompletionSink::UiCreate { token } => {
                    resolve_create_events(env, &descriptor_host_key, token, &final_.primary);
                }
            },
            TaxonomyChoice::Indeterminate => match sink {
                CompletionSink::ApiResponder { .. } => {
                    sink.resolve_error(
                        "remote_request_indeterminate",
                        format!(
                            "outcome unknown — refresh before retrying: {}",
                            final_.primary_error_message()
                        ),
                    );
                }
                CompletionSink::UiCreate { token } => {
                    // No claim/uncertain-retry tracking: an indeterminate
                    // create resolves as a plain failure so the spinner
                    // clears and a retry can proceed.
                    resolve_create_failure(
                        env,
                        descriptor_host_key.clone(),
                        token,
                        format!(
                            "outcome unknown — refresh before retrying: {}",
                            final_.primary_error_message()
                        ),
                    );
                }
            },
        }

        // Generation-stamped completion event for state application (refresh
        // / stale marking).
        if let Some(completion) = completion_event {
            let _ = env
                .event_tx
                .blocking_send(AppEvent::RemoteRoutedSequenceCompleted {
                    host: descriptor_host_key,
                    generation,
                    outcome: Box::new(completion),
                });
        }
    }
}

/// Resolve a `UiCreate` sink from any available primary result (authoritative
/// success/error, or a definitive transport error) by parsing it exactly like
/// a normal completion would.
fn resolve_create_events(
    env: &RoutedEnv,
    host_key: &RemoteHostKey,
    token: u64,
    primary: &PrimaryResult,
) {
    let event = match primary.as_ref() {
        Ok(response) => {
            match crate::app::remote_workspace::parse_remote_workspace_create_response(response) {
                Ok(workspace) => AppEvent::RemoteWorkspaceCreateSucceeded {
                    host: host_key.clone(),
                    token,
                    workspace,
                },
                Err(err) => AppEvent::RemoteWorkspaceCreateFailed {
                    host: host_key.clone(),
                    token,
                    message: err.to_string(),
                },
            }
        }
        Err(err) => AppEvent::RemoteWorkspaceCreateFailed {
            host: host_key.clone(),
            token,
            message: err.to_io().to_string(),
        },
    };
    let _ = env.event_tx.blocking_send(event);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaxonomyChoice {
    PreWriteFailure,
    Completed,
    PrimaryPreserved,
    Indeterminate,
}

/// FIX-2 (diff review round 4, blocker 2; corrected round 5, blocker 1): a
/// transport-level `Ok(String)` that is neither a genuine success NOR a
/// genuine authoritative error (`crate::remote::primary_response_parses` /
/// `primary_response_is_error`) is genuinely malformed protocol data,
/// treated exactly like a post-write transport error — `Indeterminate`,
/// never `Completed`/`PrimaryPreserved` — since the mutation may already
/// have been written and no authoritative result exists. An authoritative
/// error (e.g. `pane_not_found`) is NOT malformed: it falls through to
/// `PrimaryPreserved` below like a genuine success, so `resolve_normal`
/// forwards it to the caller UNCHANGED via `CompletionSink::resolve_success`
/// (which rewrites only the "id" field and works identically for a
/// `result`/`error` envelope) instead of reinterpreting it as indeterminate.
/// Mirrors `taxonomy_for_final` in `remote::routed` (the reducer-facing
/// classification) so the caller's error code and the cache's stale-marking
/// never disagree about the same fact.
fn taxonomy_choice(final_: &SequenceFinal) -> TaxonomyChoice {
    match (&final_.primary, &final_.refresh, final_.wrote) {
        (Ok(primary), _, _)
            if !crate::remote::primary_response_parses(primary)
                && !crate::remote::primary_response_is_error(primary) =>
        {
            TaxonomyChoice::Indeterminate
        }
        (Ok(_), RefreshLegOutcome::Data(_), _) => TaxonomyChoice::Completed,
        (Ok(_), _, _) => TaxonomyChoice::PrimaryPreserved,
        (Err(_), _, true) => TaxonomyChoice::Indeterminate,
        (Err(_), _, false) => TaxonomyChoice::PreWriteFailure,
    }
}

impl SequenceFinal {
    fn primary_error_message(&self) -> String {
        match self.primary.as_ref() {
            Err(err) => err.message.clone(),
            Ok(primary) if !crate::remote::primary_response_parses(primary) => {
                "primary response did not parse as a valid remote API response".to_string()
            }
            Ok(_) => String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Reducer-side application (pure AppState domain)
// ---------------------------------------------------------------------------

impl crate::app::state::AppState {
    /// Apply one routed-sequence completion atomically in a single reducer
    /// step: stale marking (no refresh data) or the refresh data (tab
    /// snapshot + projection), per-method primary application, and the
    /// reducer-side reconciliation stamp (test-observability only).
    pub(crate) fn apply_routed_completion(
        &mut self,
        host: &RemoteHostKey,
        generation: u64,
        outcome: RoutedCompletion,
    ) {
        use crate::remote_source::{RemoteProjectionSnapshot, RemoteProjectionStatus};

        // Reducer-side reconciliation record: test/diagnostic observability
        // only — "the reducer applied a routed completion for this
        // generation." Nothing reads this for gating purposes.
        self.remote_routed_reconciled
            .insert(host.clone(), generation);

        // Stale marking when no refresh data is present (indeterminate or
        // primary-preserved-with-failed-refresh): the affected workspace's
        // cached tabs/projection are marked stale so the next refresh
        // reconciles with authoritative remote state.
        if let Some(workspace_id) = &outcome.stale_workspace_id {
            self.remote_sources
                .mark_tab_snapshot_unavailable(host, workspace_id);
            self.remote_sources.upsert_projection_snapshot(
                host,
                RemoteProjectionSnapshot {
                    workspace_id: workspace_id.clone(),
                    tab_id: None,
                    tab_label: None,
                    status: RemoteProjectionStatus::Unavailable,
                    layout: None,
                },
            );
        }

        // Per-method primary application, only for an authoritative primary
        // result that parses as a genuine success.
        let primary_success = outcome
            .primary
            .as_deref()
            .and_then(|primary| serde_json::from_str::<SuccessResponse>(primary).ok());
        if let Some(success) = &primary_success {
            match (&outcome.apply, &success.result) {
                (crate::remote::RoutedApply::TabUpsert, ResponseResult::TabCreated { tab, .. })
                | (crate::remote::RoutedApply::TabUpsert, ResponseResult::TabInfo { tab }) => {
                    self.remote_sources.upsert_tab(host, tab.clone());
                }
                (crate::remote::RoutedApply::TabRemove { .. }, ResponseResult::Ok {}) => {}
                _ => {}
            }
        }
        if let crate::remote::RoutedApply::TabRemove { tab_id } = &outcome.apply {
            if primary_success.is_some() {
                self.remote_sources.remove_tab(host, tab_id);
                if let Some(workspace_id) = &outcome.stale_workspace_id {
                    self.remote_sources
                        .mark_tab_snapshot_unavailable(host, workspace_id);
                }
            }
        }
        if outcome.apply == crate::remote::RoutedApply::WorkspaceUpsert {
            if let Some(ResponseResult::WorkspaceInfo { workspace }) =
                primary_success.as_ref().map(|success| &success.result)
            {
                self.remote_sources
                    .upsert_workspace(host.clone(), workspace.clone());
            }
        }

        // Refresh application (single atomic step with the above).
        if let Some(refresh) = &outcome.refresh {
            self.apply_routed_refresh(host, refresh);
        }
    }

    fn apply_routed_refresh(
        &mut self,
        host: &RemoteHostKey,
        refresh: &crate::remote::RoutedRefreshData,
    ) {
        use crate::remote_source::{RemoteProjectionSnapshot, RemoteProjectionStatus};
        match &refresh.tabs {
            Some(tabs) => {
                self.remote_sources.replace_tab_snapshot(
                    host,
                    refresh.workspace_id.clone(),
                    tabs.clone(),
                );
            }
            None => {
                self.remote_sources
                    .mark_tab_snapshot_unavailable(host, &refresh.workspace_id);
                self.remote_sources.upsert_projection_snapshot(
                    host,
                    RemoteProjectionSnapshot {
                        workspace_id: refresh.workspace_id.clone(),
                        tab_id: None,
                        tab_label: None,
                        status: RemoteProjectionStatus::Unavailable,
                        layout: None,
                    },
                );
                return;
            }
        }
        let projection = match &refresh.active_tab {
            Some(fetch) => RemoteProjectionSnapshot {
                workspace_id: refresh.workspace_id.clone(),
                tab_id: fetch.tab_id.clone(),
                tab_label: fetch.tab_label.clone(),
                status: if fetch.layout.is_some() {
                    RemoteProjectionStatus::Available
                } else {
                    RemoteProjectionStatus::Unavailable
                },
                layout: fetch.layout.clone(),
            },
            None => RemoteProjectionSnapshot {
                workspace_id: refresh.workspace_id.clone(),
                tab_id: None,
                tab_label: None,
                status: RemoteProjectionStatus::Unavailable,
                layout: None,
            },
        };
        self.remote_sources
            .upsert_projection_snapshot(host, projection);
    }
}

// ---------------------------------------------------------------------------
// Planner (pure, on the event loop) and deferred seam
// ---------------------------------------------------------------------------

/// Result of [`App::plan_remote_routed_mutation`].
pub(crate) enum RoutedPlanOutcome {
    /// Not a remote-routed mutating method: the caller runs the synchronous
    /// path unchanged.
    NotHandled,
    /// A route/resolve/connected/capability guard failed: send this response
    /// immediately.
    Immediate(String),
    /// Cleared for the per-host serial mutation executor.
    Deferred(Box<RoutedSequenceDescriptor>),
}

/// Result of [`App::handle_deferred_remote_routed_api_request`].
#[derive(Debug)]
pub(crate) enum DeferredRoutedOutcome {
    /// Ownership returns to the caller for the synchronous path.
    NotHandled {
        request: Box<Request>,
        respond_to: mpsc::Sender<String>,
    },
    /// Enqueued (or an immediate guard response was sent).
    Handled,
}

impl App {
    /// Plan one routed remote MUTATION for the per-host serial executor.
    /// Pure: no thread spawn, no SSH. Returns [`RoutedPlanOutcome::NotHandled`]
    /// for anything this seam does not own (local targets, read-only methods).
    pub(crate) fn plan_remote_routed_mutation(&self, request: &Request) -> RoutedPlanOutcome {
        let id = request.id.clone();
        match request.method.clone() {
            Method::PaneFocus(target) => self.plan_pane_mutation(
                id,
                &target.pane_id,
                crate::api::schema::FederationCapabilities::PANE_FOCUS,
                |id, resolved| super::panes::remote_pane_focus_request(id, &resolved.pane_id),
            ),
            Method::PaneFocusDirection(params) => match params.pane_id.as_deref() {
                None => RoutedPlanOutcome::NotHandled,
                Some(pane_id) => self.plan_pane_mutation(
                    id,
                    pane_id,
                    crate::api::schema::FederationCapabilities::PANE_FOCUS_DIRECTION,
                    move |id, resolved| {
                        super::panes::remote_pane_focus_direction_request(
                            id,
                            &resolved.pane_id,
                            params.direction,
                        )
                    },
                ),
            },
            Method::PaneRename(params) => self.plan_pane_mutation(
                id,
                &params.pane_id,
                crate::api::schema::FederationCapabilities::PANE_RENAME,
                move |id, resolved| {
                    super::panes::remote_pane_rename_request(id, &resolved.pane_id, params.label)
                },
            ),
            Method::PaneSplit(params) => {
                // Split routes on either the target pane id or workspace id.
                let route = match self.plan_pane_split_remote_route(&params) {
                    Ok(route) => route,
                    Err(err) => {
                        return RoutedPlanOutcome::Immediate(encode_error_body(
                            id,
                            remote_route_plan_error_body(err),
                        ))
                    }
                };
                let Some((host, selector)) = route else {
                    return RoutedPlanOutcome::NotHandled;
                };
                self.build_pane_descriptor(
                    id,
                    host,
                    selector,
                    crate::api::schema::FederationCapabilities::PANE_SPLIT,
                    crate::remote::PreferredTabFromPrimary::PaneInfoTab,
                    move |id, resolved| {
                        super::panes::remote_pane_split_request(
                            id,
                            params,
                            &resolved.workspace_id,
                            &resolved.pane_id,
                        )
                    },
                )
            }
            Method::PaneClose(params) => {
                let route = match self.plan_pane_close_remote_route(&params) {
                    Ok(route) => route,
                    Err(err) => {
                        return RoutedPlanOutcome::Immediate(encode_error_body(
                            id,
                            remote_route_plan_error_body(err),
                        ))
                    }
                };
                let Some((host, selector)) = route else {
                    return RoutedPlanOutcome::NotHandled;
                };
                if !params.confirm {
                    return RoutedPlanOutcome::Immediate(encode_error(
                        id,
                        "confirmation_required",
                        "pane.close on a remote pane is destructive; pass confirm: true to proceed",
                    ));
                }
                self.build_pane_descriptor(
                    id,
                    host,
                    selector,
                    crate::api::schema::FederationCapabilities::PANE_CLOSE,
                    crate::remote::PreferredTabFromPrimary::None,
                    move |id, resolved| {
                        super::panes::remote_pane_close_request(id, &resolved.pane_id)
                    },
                )
            }
            Method::TabCreate(params) => {
                let route = match self.plan_tab_create_remote_route(&params) {
                    Ok(route) => route,
                    Err(err) => {
                        return RoutedPlanOutcome::Immediate(encode_error_body(
                            id,
                            remote_route_plan_error_body(err),
                        ))
                    }
                };
                let Some((host, selector)) = route else {
                    return RoutedPlanOutcome::NotHandled;
                };
                self.build_tab_workspace_descriptor(
                    id,
                    host,
                    selector,
                    crate::api::schema::FederationCapabilities::TAB_CREATE,
                    crate::remote::RoutedApply::TabUpsert,
                    Some(crate::remote::PreferredTabFromPrimary::TabCreatedTab),
                    move |id, resolved| {
                        super::tabs::remote_tab_create_request(
                            id,
                            params,
                            &resolved.workspace.workspace_id,
                        )
                    },
                )
            }
            Method::TabFocus(target) => {
                let route = match self.plan_tab_target_remote_route(&target.tab_id) {
                    Ok(route) => route,
                    Err(err) => {
                        return RoutedPlanOutcome::Immediate(encode_error_body(
                            id,
                            remote_route_plan_error_body(err),
                        ))
                    }
                };
                let Some((host, selector)) = route else {
                    return RoutedPlanOutcome::NotHandled;
                };
                self.build_tab_descriptor(
                    id,
                    host,
                    selector,
                    crate::api::schema::FederationCapabilities::TAB_FOCUS,
                    crate::remote::RoutedApply::TabUpsert,
                    Some(crate::remote::PreferredTabFromPrimary::TabInfoTab),
                    move |id, resolved| {
                        super::tabs::remote_tab_focus_request(id, &resolved.tab.tab_id)
                    },
                )
            }
            Method::TabClose(target) => {
                let route = match self.plan_tab_target_remote_route(&target.tab_id) {
                    Ok(route) => route,
                    Err(err) => {
                        return RoutedPlanOutcome::Immediate(encode_error_body(
                            id,
                            remote_route_plan_error_body(err),
                        ))
                    }
                };
                let Some((host, selector)) = route else {
                    return RoutedPlanOutcome::NotHandled;
                };
                // Same destructive-confirmation gate as pane.close: the
                // forwarded request always sets `confirm: true`
                // (`tabs::remote_tab_close_request`), so an unconfirmed
                // close must be rejected HERE, before planning/queuing —
                // never silently promoted to confirmed.
                if !target.confirm {
                    return RoutedPlanOutcome::Immediate(encode_error(
                        id,
                        "confirmation_required",
                        format!(
                            "tab.close on remote target {} is destructive; pass confirm=true to proceed",
                            target.tab_id
                        ),
                    ));
                }
                self.build_tab_descriptor(
                    id,
                    host,
                    selector,
                    crate::api::schema::FederationCapabilities::TAB_CLOSE,
                    crate::remote::RoutedApply::TabRemove {
                        tab_id: String::new(), // filled by the resolver below
                    },
                    None,
                    move |id, resolved| {
                        super::tabs::remote_tab_close_request(id, &resolved.tab.tab_id)
                    },
                )
            }
            Method::TabRename(params) => {
                let route = match self.plan_tab_target_remote_route(&params.tab_id) {
                    Ok(route) => route,
                    Err(err) => {
                        return RoutedPlanOutcome::Immediate(encode_error_body(
                            id,
                            remote_route_plan_error_body(err),
                        ))
                    }
                };
                let Some((host, selector)) = route else {
                    return RoutedPlanOutcome::NotHandled;
                };
                self.build_tab_descriptor(
                    id,
                    host,
                    selector,
                    crate::api::schema::FederationCapabilities::TAB_RENAME,
                    crate::remote::RoutedApply::TabUpsert,
                    Some(crate::remote::PreferredTabFromPrimary::TabInfoTab),
                    move |id, resolved| Request {
                        id,
                        method: Method::TabRename(crate::api::schema::TabRenameParams {
                            tab_id: resolved.tab.tab_id.clone(),
                            label: params.label,
                        }),
                    },
                )
            }
            Method::WorkspaceRename(params) => {
                let route = match self.plan_workspace_target_remote_route(&params.workspace_id) {
                    Ok(route) => route,
                    Err(err) => {
                        return RoutedPlanOutcome::Immediate(encode_error_body(
                            id,
                            remote_route_plan_error_body(err),
                        ))
                    }
                };
                let Some((host, selector)) = route else {
                    return RoutedPlanOutcome::NotHandled;
                };
                self.build_workspace_descriptor(
                    id,
                    host,
                    selector,
                    crate::api::schema::FederationCapabilities::WORKSPACE_RENAME,
                    move |id, resolved| Request {
                        id,
                        method: Method::WorkspaceRename(
                            crate::api::schema::WorkspaceRenameParams {
                                workspace_id: resolved.workspace.workspace_id.clone(),
                                label: params.label,
                            },
                        ),
                    },
                )
            }
            _ => RoutedPlanOutcome::NotHandled,
        }
    }

    /// Plan a pane-family mutation routed by pane id (focus / focus-direction /
    /// rename). `capability` is the method-specific federation capability
    /// (H2: each caller passes its OWN capability so an unsupported op is
    /// correctly rejected and a supported one is never forwarded unadvertised
    /// — this helper must not hardcode a single shared capability for all
    /// three methods).
    fn plan_pane_mutation(
        &self,
        id: String,
        pane_id: &str,
        capability: &'static str,
        build_request: impl FnOnce(String, &crate::remote_target::RemotePaneResolution) -> Request,
    ) -> RoutedPlanOutcome {
        let route = match self.plan_pane_target_remote_route(pane_id) {
            Ok(route) => route,
            Err(err) => {
                return RoutedPlanOutcome::Immediate(encode_error_body(
                    id.to_string(),
                    remote_route_plan_error_body(err),
                ))
            }
        };
        let Some((host, selector)) = route else {
            return RoutedPlanOutcome::NotHandled;
        };
        self.build_pane_descriptor(
            id,
            host,
            selector,
            capability,
            crate::remote::PreferredTabFromPrimary::None,
            build_request,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_pane_descriptor(
        &self,
        id: String,
        host: RemoteHostConfig,
        selector: crate::remote_target::RemoteTargetSelector,
        capability: &'static str,
        preferred_from_primary: crate::remote::PreferredTabFromPrimary,
        build_request: impl FnOnce(String, &crate::remote_target::RemotePaneResolution) -> Request,
    ) -> RoutedPlanOutcome {
        let host_key = match self.remote_pane_host_connected_or_error(&host) {
            Ok(host_key) => host_key,
            Err(body) => return RoutedPlanOutcome::Immediate(encode_error_body(id, body)),
        };
        if !self
            .state
            .remote_sources
            .host_capabilities(&host_key)
            .supports_route_method(capability)
        {
            return RoutedPlanOutcome::Immediate(encode_error_body(
                id,
                remote_capability_unavailable_body(&host.name, capability),
            ));
        }
        let resolved = match crate::remote_target::resolve_remote_pane_target(
            &self.state.remote_sources,
            &host,
            &selector,
        ) {
            Ok(resolved) => resolved,
            Err(err) => {
                return RoutedPlanOutcome::Immediate(encode_error_body(
                    id,
                    super::panes::remote_pane_resolve_error_body(err),
                ))
            }
        };
        let primary = build_request(id, &resolved);
        let refresh = RoutedRefreshSpec {
            workspace_id: resolved.workspace_id.clone(),
            preferred_tab_id: None,
            preferred_from_primary,
        };
        self.finalize_mutation_descriptor(
            host,
            host_key,
            primary,
            Some(refresh),
            crate::remote::RoutedApply::RefreshOnly,
        )
    }

    fn build_tab_descriptor(
        &self,
        id: String,
        host: RemoteHostConfig,
        selector: crate::remote_target::RemoteTargetSelector,
        capability: &'static str,
        apply: crate::remote::RoutedApply,
        preferred_from_primary: Option<crate::remote::PreferredTabFromPrimary>,
        build_request: impl FnOnce(String, &crate::remote_target::RemoteTabResolution) -> Request,
    ) -> RoutedPlanOutcome {
        let host_key = match self.remote_host_connected_or_error(&host, "tab") {
            Ok(host_key) => host_key,
            Err(body) => return RoutedPlanOutcome::Immediate(encode_error_body(id, body)),
        };
        if !self
            .state
            .remote_sources
            .host_capabilities(&host_key)
            .supports_route_method(capability)
        {
            return RoutedPlanOutcome::Immediate(encode_error_body(
                id,
                remote_capability_unavailable_body(&host.name, capability),
            ));
        }
        let resolved = match crate::remote_target::resolve_remote_tab_target(
            &self.state.remote_sources,
            &host,
            &selector,
        ) {
            Ok(resolved) => resolved,
            Err(err) => {
                return RoutedPlanOutcome::Immediate(encode_error_body(
                    id,
                    super::tabs::remote_tab_resolve_error_body(err),
                ))
            }
        };
        let apply = match &apply {
            crate::remote::RoutedApply::TabRemove { .. } => crate::remote::RoutedApply::TabRemove {
                tab_id: resolved.tab.tab_id.clone(),
            },
            other => other.clone(),
        };
        let primary = build_request(id, &resolved);
        let refresh = RoutedRefreshSpec {
            workspace_id: resolved.workspace_id.clone(),
            preferred_tab_id: None,
            preferred_from_primary: preferred_from_primary
                .unwrap_or(crate::remote::PreferredTabFromPrimary::None),
        };
        self.finalize_mutation_descriptor(host, host_key, primary, Some(refresh), apply)
    }

    fn build_tab_workspace_descriptor(
        &self,
        id: String,
        host: RemoteHostConfig,
        selector: crate::remote_target::RemoteTargetSelector,
        capability: &'static str,
        apply: crate::remote::RoutedApply,
        preferred_from_primary: Option<crate::remote::PreferredTabFromPrimary>,
        build_request: impl FnOnce(String, &crate::remote_target::RemoteWorkspaceResolution) -> Request,
    ) -> RoutedPlanOutcome {
        let host_key = match self.remote_host_connected_or_error(&host, "tab") {
            Ok(host_key) => host_key,
            Err(body) => return RoutedPlanOutcome::Immediate(encode_error_body(id, body)),
        };
        if !self
            .state
            .remote_sources
            .host_capabilities(&host_key)
            .supports_route_method(capability)
        {
            return RoutedPlanOutcome::Immediate(encode_error_body(
                id,
                remote_capability_unavailable_body(&host.name, capability),
            ));
        }
        let resolved = match crate::remote_target::resolve_remote_workspace_target(
            &self.state.remote_sources,
            &host,
            &selector,
        ) {
            Ok(resolved) => resolved,
            Err(err) => {
                return RoutedPlanOutcome::Immediate(encode_error_body(
                    id,
                    super::workspaces::remote_workspace_resolve_error_body(err),
                ))
            }
        };
        let primary = build_request(id, &resolved);
        let refresh = RoutedRefreshSpec {
            workspace_id: resolved.workspace.workspace_id.clone(),
            preferred_tab_id: None,
            preferred_from_primary: preferred_from_primary
                .unwrap_or(crate::remote::PreferredTabFromPrimary::None),
        };
        self.finalize_mutation_descriptor(host, host_key, primary, Some(refresh), apply)
    }

    fn build_workspace_descriptor(
        &self,
        id: String,
        host: RemoteHostConfig,
        selector: crate::remote_target::RemoteTargetSelector,
        capability: &'static str,
        build_request: impl FnOnce(String, &crate::remote_target::RemoteWorkspaceResolution) -> Request,
    ) -> RoutedPlanOutcome {
        let host_key =
            crate::remote_source::RemoteHostKey::new(host.name.clone(), host.session.clone());
        let host_status = self.state.remote_sources.host_status(&host_key);
        if !host_status.is_some_and(|status| status.is_connected()) {
            let status = host_status
                .and_then(|status| status.stale_label())
                .unwrap_or("disconnected")
                .to_string();
            return RoutedPlanOutcome::Immediate(encode_error_body(
                id,
                ErrorBody {
                    code: "remote_host_not_connected".to_string(),
                    message: format!(
                        "remote host {} is {status}; wait for it to reconnect before mutating a remote workspace",
                        host.name
                    ),
                },
            ));
        }
        if !self
            .state
            .remote_sources
            .host_capabilities(&host_key)
            .supports_route_method(capability)
        {
            return RoutedPlanOutcome::Immediate(encode_error_body(
                id,
                remote_capability_unavailable_body(&host.name, capability),
            ));
        }
        let resolved = match crate::remote_target::resolve_remote_workspace_target(
            &self.state.remote_sources,
            &host,
            &selector,
        ) {
            Ok(resolved) => resolved,
            Err(err) => {
                return RoutedPlanOutcome::Immediate(encode_error_body(
                    id,
                    super::workspaces::remote_workspace_resolve_error_body(err),
                ))
            }
        };
        let primary = build_request(id, &resolved);
        self.finalize_mutation_descriptor(
            host,
            host_key,
            primary,
            None,
            crate::remote::RoutedApply::WorkspaceUpsert,
        )
    }

    /// Shared tail: prepared-state capture (Slice B), source generation
    /// capture, refresh capability capture. No quarantine gate: a mutation
    /// arriving while the host is unreachable is already rejected by the
    /// pre-existing connectivity precheck.
    fn finalize_mutation_descriptor(
        &self,
        host: RemoteHostConfig,
        host_key: RemoteHostKey,
        primary: Request,
        refresh: Option<RoutedRefreshSpec>,
        apply: crate::remote::RoutedApply,
    ) -> RoutedPlanOutcome {
        let bridge_state = self.state.remote_sources.connected_bridge_state(&host_key);
        let source_generation = self
            .remote_source_supervisors
            .iter()
            .find(|handle| handle.host_key == host_key)
            .map(|handle| handle.generation);
        let capabilities = self.state.remote_sources.host_capabilities(&host_key);
        RoutedPlanOutcome::Deferred(Box::new(RoutedSequenceDescriptor {
            host,
            host_key,
            source_generation,
            bridge_state,
            spec: RoutedSequenceSpec {
                primary,
                refresh,
                tab_list_capable: capabilities.tab_list,
                layout_export_capable: capabilities.layout_export,
                apply,
            },
            sink: None,
        }))
    }

    /// Deferred seam for routed remote mutations: plan on the loop, enqueue
    /// to the per-host serial executor (overflow resolves immediately with a
    /// terminal busy error), and let the executor's sink resolve the API
    /// response exactly once.
    pub(crate) fn handle_deferred_remote_routed_api_request(
        &mut self,
        request: Request,
        respond_to: mpsc::Sender<String>,
    ) -> DeferredRoutedOutcome {
        let plan = self.plan_remote_routed_mutation(&request);
        match plan {
            RoutedPlanOutcome::NotHandled => DeferredRoutedOutcome::NotHandled {
                request: Box::new(request),
                respond_to,
            },
            RoutedPlanOutcome::Immediate(response) => {
                let _ = respond_to.send(response);
                DeferredRoutedOutcome::Handled
            }
            RoutedPlanOutcome::Deferred(mut descriptor) => {
                let request_id = descriptor.spec.primary.id.clone();
                descriptor.sink = Some(CompletionSink::ApiResponder {
                    respond_to: respond_to.clone(),
                    request_id: request_id.clone(),
                });
                match self.routed_executor.enqueue(*descriptor) {
                    Ok(()) => DeferredRoutedOutcome::Handled,
                    Err(RoutedEnqueueError::Busy) => {
                        let _ = respond_to.send(encode_error(
                            request_id,
                            "remote_bridge_busy",
                            "remote host mutation queue is saturated; retry shortly",
                        ));
                        DeferredRoutedOutcome::Handled
                    }
                }
            }
        }
    }

    /// TUI-internal dispatch path (`dispatch_api_request` callers such as the
    /// projected tab/pane close and focus actions): plan the routed mutation
    /// and enqueue it fire-and-forget (the TUI ignores the response string).
    /// Returns `Some(ack_response)` when handled — the caller must NOT run
    /// the synchronous path — or `None` when this seam does not own the
    /// request.
    pub(crate) fn try_defer_remote_routed_internal(&mut self, request: &Request) -> Option<String> {
        let plan = self.plan_remote_routed_mutation(request);
        match plan {
            RoutedPlanOutcome::NotHandled => None,
            RoutedPlanOutcome::Immediate(response) => Some(response),
            RoutedPlanOutcome::Deferred(mut descriptor) => {
                let request_id = descriptor.spec.primary.id.clone();
                // Dropped-receiver channel: the TUI ignores the response.
                let (respond_to, _receiver) = mpsc::channel();
                descriptor.sink = Some(CompletionSink::ApiResponder {
                    respond_to,
                    request_id,
                });
                let request_id = request.id.clone();
                // M2: an enqueue failure (queue saturated) must NOT be
                // silently discarded and reported as "queued". The
                // dropped-receiver channel means the TUI never observes a
                // later resolution either way; if nothing was actually
                // queued, the caller must be told NOW rather than led to
                // believe the remote source cache will eventually update for
                // a mutation that never ran.
                match self.routed_executor.enqueue(*descriptor) {
                    Ok(()) => Some(encode_error(
                        request_id,
                        "remote_request_deferred",
                        "routed remote mutation queued; the result arrives through the remote source cache",
                    )),
                    Err(RoutedEnqueueError::Busy) => Some(encode_error(
                        request_id,
                        "remote_bridge_busy",
                        "remote host mutation queue is saturated; retry shortly",
                    )),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{Method, WorkspaceRenameParams};
    use crate::app::state::AppState;
    use crate::remote::{RoutedApply, RoutedIoError, RoutedTaxonomy};
    use crate::remote_source::{RemoteConnectionStatus, RemoteSourceCapabilities};
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

    /// Per-host fake leg state, keyed by a unique host name per test so
    /// parallel tests never collide. Fn-pointer legs cannot capture, so the
    /// registry is global and keyed.
    struct FakeLeg {
        requests: Mutex<Vec<String>>,
        responses: Mutex<VecDeque<io::Result<String>>>,
        /// When set, the next leg spawns a REAL child (`sleep 30`), publishes
        /// its PID (exactly like a tracked one-shot ssh leg), and blocks
        /// until the child dies — the real kill path unblocks it.
        block_until_child_death: AtomicBool,
        blocked_child_pid: AtomicU32,
        /// FIX-2: when set, the next leg fails BEFORE its `&mut bool`
        /// out-param is ever touched, simulating a real connect/spawn/stdin
        /// failure — `wrote` must stay `false`.
        fail_before_write: AtomicBool,
        /// FIX-1: when armed (`arm_release_gate`), the next leg records its
        /// request, then blocks on this receiver before popping/returning
        /// its queued response — lets a test hold a sequence deterministically
        /// "in flight" (dequeued, running, not yet returned) until it
        /// explicitly releases it, with no real child and no timing race.
        release_gate: Mutex<Option<mpsc::Receiver<()>>>,
    }

    impl FakeLeg {
        fn new() -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                responses: Mutex::new(VecDeque::new()),
                block_until_child_death: AtomicBool::new(false),
                blocked_child_pid: AtomicU32::new(0),
                fail_before_write: AtomicBool::new(false),
                release_gate: Mutex::new(None),
            }
        }

        fn push_response(&self, response: io::Result<String>) {
            self.responses.lock().unwrap().push_back(response);
        }

        /// FIX-1: arm the release gate; returns the sender the test holds to
        /// release the next leg call once it has confirmed (via `requests`)
        /// that the leg has been dequeued and is now blocked.
        fn arm_release_gate(&self) -> mpsc::Sender<()> {
            let (tx, rx) = mpsc::channel();
            *self.release_gate.lock().unwrap() = Some(rx);
            tx
        }
    }

    fn fake_leg_registry() -> &'static Mutex<BTreeMap<String, Arc<FakeLeg>>> {
        static REGISTRY: std::sync::OnceLock<Mutex<BTreeMap<String, Arc<FakeLeg>>>> =
            std::sync::OnceLock::new();
        REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
    }

    fn register_fake_leg(host: &str) -> Arc<FakeLeg> {
        let leg = Arc::new(FakeLeg::new());
        fake_leg_registry()
            .lock()
            .unwrap()
            .insert(host.to_string(), Arc::clone(&leg));
        leg
    }

    fn lookup_fake_leg(host: &str) -> Option<Arc<FakeLeg>> {
        fake_leg_registry().lock().unwrap().get(host).cloned()
    }

    /// FIX-2 correction (diff review round 5, blocker 1): host-keyed
    /// registry mirroring `fake_leg_registry` above, for pooled-transport
    /// starter `fn` pointers (which cannot capture locals) that need to hand
    /// an externally observable `refresh_leg_attempted` flag into a
    /// `FakePooledPrimaryThenRefreshConnection` they construct.
    fn refresh_leg_attempted_registry() -> &'static Mutex<BTreeMap<String, Arc<AtomicBool>>> {
        static REGISTRY: std::sync::OnceLock<Mutex<BTreeMap<String, Arc<AtomicBool>>>> =
            std::sync::OnceLock::new();
        REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
    }

    fn register_refresh_leg_attempted_flag(host: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        refresh_leg_attempted_registry()
            .lock()
            .unwrap()
            .insert(host.to_string(), Arc::clone(&flag));
        flag
    }

    fn lookup_refresh_leg_attempted_flag(host: &str) -> Arc<AtomicBool> {
        refresh_leg_attempted_registry()
            .lock()
            .unwrap()
            .get(host)
            .cloned()
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)))
    }

    fn leg_method_name(request: &Request) -> String {
        let text = serde_json::to_string(&crate::api::schema::Request {
            id: request.id.clone(),
            method: request.method.clone(),
        })
        .unwrap_or_default();
        // Cheap stable label: the first `"method"` key occurrence.
        text.split_whitespace()
            .find(|token| token.contains("workspace.rename") || token.contains("tab.list"))
            .map(str::to_string)
            .unwrap_or_else(|| "method".to_string())
    }

    /// Fake one-shot full leg (no ssh): canned responses, or a REAL blocked
    /// child (`sleep 30`) simulating a hung ssh read — the test itself kills
    /// it directly via `kill_blocked_child`/`blocked_child_pid` (there is no
    /// production kill path anymore, see the module contract). FIX-2: `wrote`
    /// mirrors production ordering — set immediately before the (simulated)
    /// write, left untouched on a `fail_before_write` connect-style failure.
    fn fake_full_leg(
        host: &RemoteHostConfig,
        request: &Request,
        wrote: &mut bool,
    ) -> io::Result<String> {
        let Some(leg) = lookup_fake_leg(&host.name) else {
            // No fake leg registered for this host: a genuine test-harness
            // config problem, not a simulated remote failure.
            return Err(io::Error::other("no fake leg registered"));
        };
        leg.requests.lock().unwrap().push(leg_method_name(request));
        if leg.fail_before_write.swap(false, Ordering::AcqRel) {
            // Simulates a real connect/spawn/stdin-acquisition failure:
            // `wrote` must stay false — nothing was ever sent.
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "fake pre-write connect failure",
            ));
        }
        if leg.block_until_child_death.swap(false, Ordering::AcqRel) {
            let mut child = std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("spawn blocked child");
            leg.blocked_child_pid.store(child.id(), Ordering::Release);
            // The simulated request byte is considered written once the
            // child is up and blocking on its (simulated) read, mirroring
            // production ordering (write succeeds, then the read blocks).
            *wrote = true;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "fake blocked child died",
            ));
        }
        if let Some(rx) = leg.release_gate.lock().unwrap().take() {
            // FIX-1: the request is already recorded above (the test's
            // proof-of-dequeue); mirror production's write-succeeds-then-
            // blocks ordering, then wait for the test's explicit release.
            *wrote = true;
            let _ = rx.recv();
        } else {
            *wrote = true;
        }
        let mut responses = leg.responses.lock().unwrap();
        responses.pop_front().unwrap_or_else(|| {
            // FIX-2: must be a genuinely parseable `SuccessResponse` — the
            // caller-facing taxonomy now parses every `Ok` primary
            // (`crate::remote::primary_response_parses`), so a placeholder
            // that doesn't match the real wire shape would be misclassified
            // as indeterminate instead of a success.
            Ok(r#"{"id":"fake","result":{"type":"ok"}}"#.to_string())
        })
    }

    fn fake_prepared_leg(
        host: &RemoteHostConfig,
        _state: &RemoteApiBridgeState,
        request: &Request,
        wrote: &mut bool,
    ) -> io::Result<String> {
        fake_full_leg(host, request, wrote)
    }

    fn test_transport_starter(
        _pool: &'static RemoteAgentBridgePool,
        host: &RemoteHostConfig,
        _bridge_state: Option<&RemoteApiBridgeState>,
        _primary: &Request,
    ) -> io::Result<WorkerTransport> {
        Ok(WorkerTransport::OneShotFull { host: host.clone() })
    }

    fn failing_transport_starter(
        _pool: &'static RemoteAgentBridgePool,
        _host: &RemoteHostConfig,
        _bridge_state: Option<&RemoteApiBridgeState>,
        _primary: &Request,
    ) -> io::Result<WorkerTransport> {
        Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "test starter failure (pre-write)",
        ))
    }

    struct TestHarness {
        executor: Arc<RoutedExecutorPool>,
        event_rx: tokio_mpsc::Receiver<AppEvent>,
        _limiter: &'static RemoteAgentBridgeLimiter,
    }

    static HOST_COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Unique host name per harness so parallel tests never share fake legs.
    fn unique_host(prefix: &str) -> String {
        format!("{prefix}-{}", HOST_COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    fn harness_with_starter(starter: TransportStarterFn) -> TestHarness {
        let host_label = unique_host("leg");
        let leg = register_fake_leg(&host_label);
        let _ = leg;
        let limiter: &'static RemoteAgentBridgeLimiter =
            Box::leak(Box::new(RemoteAgentBridgeLimiter::new(4)));
        let pool: &'static RemoteAgentBridgePool = Box::leak(Box::new(RemoteAgentBridgePool::new(
            4,
            Duration::from_secs(30),
        )));
        let (event_tx, event_rx) = tokio_mpsc::channel(256);
        let executor = RoutedExecutorPool::new(RoutedEnv {
            limiter,
            pool,
            transport_starter: starter,
            prepared_leg: fake_prepared_leg,
            full_leg: fake_full_leg,
            event_tx,
            permit_poll: Duration::from_millis(5),
            starvation_after: Duration::from_millis(50),
            permit_wait_max: Duration::from_millis(300),
        });
        TestHarness {
            executor,
            event_rx,
            _limiter: limiter,
        }
    }

    #[allow(unused_mut)]
    fn harness() -> TestHarness {
        harness_with_starter(test_transport_starter)
    }

    fn test_host(name: &str) -> RemoteHostConfig {
        RemoteHostConfig::new(name, name, crate::session::DEFAULT_SESSION_NAME, true)
    }

    fn mutation_descriptor(
        host: &RemoteHostConfig,
        respond_to: mpsc::Sender<String>,
    ) -> RoutedSequenceDescriptor {
        let id = format!("test-mut-{}", host.name);
        RoutedSequenceDescriptor {
            host_key: RemoteHostKey::new(&host.name, &host.session),
            host: host.clone(),
            source_generation: None,
            bridge_state: None,
            spec: RoutedSequenceSpec {
                primary: Request {
                    id,
                    method: Method::WorkspaceRename(WorkspaceRenameParams {
                        workspace_id: "ws-1".to_string(),
                        label: "renamed".to_string(),
                    }),
                },
                refresh: None,
                tab_list_capable: false,
                layout_export_capable: false,
                apply: RoutedApply::WorkspaceUpsert,
            },
            sink: Some(CompletionSink::ApiResponder {
                respond_to,
                request_id: format!("test-mut-{}", host.name),
            }),
        }
    }

    fn recv_response(rx: &mpsc::Receiver<String>) -> String {
        rx.recv_timeout(Duration::from_secs(5)).expect("response")
    }

    // --- sink exactly-once, dropped receiver, permit release ---------------

    #[test]
    fn api_sink_resolves_exactly_once_on_normal_completion() {
        let mut harness = harness();
        let host = test_host(&unique_host("sink"));
        let leg = register_fake_leg(&host.name);
        leg.push_response(Ok(serde_json::to_string(&SuccessResponse {
            id: "x".to_string(),
            result: ResponseResult::WorkspaceInfo {
                workspace: crate::api::schema::WorkspaceInfo {
                    workspace_id: "ws-1".to_string(),
                    number: 1,
                    label: "renamed".to_string(),
                    focused: false,
                    pane_count: 1,
                    tab_count: 1,
                    active_tab_id: "tab-1".to_string(),
                    agent_status: crate::api::schema::AgentStatus::Unknown,
                    worktree: None,
                },
            },
        })
        .unwrap()));

        let (respond_to, response_rx) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor(&host, respond_to))
            .expect("enqueue");

        let response = recv_response(&response_rx);
        assert!(response.contains("renamed"), "response: {response}");
        // Exactly once: no second message ever arrives.
        assert!(response_rx
            .recv_timeout(Duration::from_millis(150))
            .is_err());

        // Permit released on the normal path.
        let key = RemoteHostKey::new(&host.name, &host.session);
        let deadline = Instant::now() + Duration::from_secs(2);
        while harness._limiter.in_flight(&key) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(harness._limiter.in_flight(&key), 0);

        // The completion event was emitted (generation-stamped, 0 here).
        let mut completion_seen = false;
        while let Ok(event) = harness.event_rx.try_recv() {
            if matches!(
                event,
                AppEvent::RemoteRoutedSequenceCompleted { generation: 0, .. }
            ) {
                completion_seen = true;
            }
        }
        assert!(completion_seen, "completion event emitted");
    }

    #[test]
    fn api_sink_is_dropped_receiver_safe() {
        let harness = harness();
        let host = test_host(&unique_host("dropped"));
        let (respond_to, response_rx) = mpsc::channel();
        drop(response_rx);
        harness
            .executor
            .enqueue(mutation_descriptor(&host, respond_to))
            .expect("enqueue");
        // No panic; the sequence still completes and releases the permit.
        let key = RemoteHostKey::new(&host.name, &host.session);
        let deadline = Instant::now() + Duration::from_secs(2);
        while harness._limiter.in_flight(&key) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(harness._limiter.in_flight(&key), 0);
    }

    #[test]
    fn pre_write_transport_failure_resolves_retryable_error_and_releases_permit() {
        let mut harness = harness_with_starter(failing_transport_starter);
        let host = test_host(&unique_host("prewrite"));
        let (respond_to, response_rx) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor(&host, respond_to))
            .expect("enqueue");

        let response = recv_response(&response_rx);
        assert!(
            response.contains("remote_request_failed"),
            "response: {response}"
        );
        assert!(!response.contains("indeterminate"));
        let key = RemoteHostKey::new(&host.name, &host.session);
        let deadline = Instant::now() + Duration::from_secs(2);
        while harness._limiter.in_flight(&key) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(harness._limiter.in_flight(&key), 0);
        // No completion event for a pre-write failure (no cache mutation).
        while let Ok(event) = harness.event_rx.try_recv() {
            assert!(
                !matches!(event, AppEvent::RemoteRoutedSequenceCompleted { .. }),
                "pre-write failure must not emit a completion event"
            );
        }
    }

    /// FIX-1: a permanently saturated per-host limiter must resolve
    /// `remote_bridge_busy` once `permit_wait_max` elapses, not hang forever
    /// — this bound applies BEFORE any transport exists (no child, no PID,
    /// no bytes), so it must never emit `RemoteRoutedRecoveryNeeded`: nothing
    /// was dispatched and the host itself may be perfectly healthy.
    #[test]
    fn saturated_permit_wait_expires_with_busy_error_not_a_reconnect() {
        let mut harness = harness();
        let host = test_host(&unique_host("permit-saturated"));
        let key = RemoteHostKey::new(&host.name, &host.session);

        // Hold every permit for the whole test so the executor's own
        // `try_acquire` loop never succeeds.
        let held: Vec<_> = (0..harness._limiter.limit())
            .map(|_| harness._limiter.try_acquire(&key).expect("permit"))
            .collect();
        assert_eq!(held.len(), harness._limiter.limit());

        let (respond_to, response_rx) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor(&host, respond_to))
            .expect("enqueue");

        // Resolves busy once `permit_wait_max` (300ms in this harness)
        // elapses — well inside the test's own generous timeout, proving it
        // does not hang like the wedged-remote-process case.
        let response = response_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("busy response within the bound, not a hang");
        assert!(
            response.contains("remote_bridge_busy"),
            "response: {response}"
        );

        // No transport was ever opened, so this must never look like a
        // connection problem.
        std::thread::sleep(Duration::from_millis(100));
        while let Ok(event) = harness.event_rx.try_recv() {
            assert!(
                !matches!(event, AppEvent::RemoteRoutedRecoveryNeeded { .. }),
                "permit-wait expiry must never trigger a reconnect: {event:?}"
            );
        }

        drop(held);
    }

    struct FakeFailingPooledConnection;

    impl crate::remote::PersistentRemoteBridgeConnection for FakeFailingPooledConnection {
        fn write_request(&mut self, _request: &Request) -> io::Result<()> {
            Ok(())
        }

        fn read_response(&mut self) -> io::Result<String> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "broken pipe"))
        }

        fn is_alive(&mut self) -> bool {
            false
        }
    }

    fn pooled_failing_transport_starter(
        _pool: &'static RemoteAgentBridgePool,
        host: &RemoteHostConfig,
        _bridge_state: Option<&RemoteApiBridgeState>,
        _primary: &Request,
    ) -> io::Result<WorkerTransport> {
        let key = RemoteHostKey::new(&host.name, &host.session);
        Ok(WorkerTransport::Pooled(
            crate::remote::PooledHandle::for_test(Box::new(FakeFailingPooledConnection), key),
        ))
    }

    /// REQ-3/REQ-5: a post-write transport error on the pooled path never
    /// parks the connection back in the pool — its active slot is released
    /// exactly once and the connection itself is dropped, never returned via
    /// `return_persistent_bridge` — and triggers exactly one reconnect
    /// escalation.
    #[test]
    fn post_write_transport_error_never_parks_the_connection_and_reconnects_once() {
        let mut harness = harness_with_starter(pooled_failing_transport_starter);
        let host = test_host(&unique_host("pooled-fail"));
        let key = RemoteHostKey::new(&host.name, &host.session);
        harness.executor.env.pool.seed_active_for_test(&key);

        let (respond_to, response_rx) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor(&host, respond_to))
            .expect("enqueue");

        let response = recv_response(&response_rx);
        assert!(
            response.contains("remote_request_indeterminate"),
            "response: {response}"
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        while harness.executor.env.pool.active_for(&key) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(harness.executor.env.pool.active_for(&key), 0);
        assert_eq!(
            harness.executor.env.pool.idle_for(&key),
            0,
            "a post-write transport error must never park the connection"
        );

        let mut recovery = 0;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            while let Ok(event) = harness.event_rx.try_recv() {
                if matches!(event, AppEvent::RemoteRoutedRecoveryNeeded { .. }) {
                    recovery += 1;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(recovery, 1, "exactly one reconnect request");
    }

    /// Fake pooled connection for the REQ-3 refresh/parse regressions below:
    /// the PRIMARY leg always returns a configurable canned response; the
    /// `tab.list` refresh leg is independently configurable to fail. Keyed by
    /// request id exactly like production's pooled dispatch (one connection,
    /// sequential legs, no multiplexing).
    struct FakePooledPrimaryThenRefreshConnection {
        last_request_id: Mutex<Option<String>>,
        primary_response: io::Result<String>,
        fail_tab_list: bool,
        /// When true, `tab.list` returns one tab (so a `layout.export` leg is
        /// actually attempted) instead of an empty list.
        tab_list_has_active_tab: bool,
        fail_layout_export: bool,
        /// FIX-2 correction (diff review round 5, blocker 1): externally
        /// observable — set whenever ANY refresh leg (`tab.list` or
        /// `layout.export`) is actually written, so a test can prove refresh
        /// legs never ran (an authoritative error or malformed primary must
        /// skip them entirely — see `routed_sequence_worker`) even after the
        /// connection itself has been moved into the pool and is no longer
        /// directly reachable from the test.
        refresh_leg_attempted: Arc<AtomicBool>,
    }

    impl crate::remote::PersistentRemoteBridgeConnection for FakePooledPrimaryThenRefreshConnection {
        fn write_request(&mut self, request: &Request) -> io::Result<()> {
            if request.id == "remote-source.tab-list" || request.id == "remote-source.layout-export"
            {
                self.refresh_leg_attempted.store(true, Ordering::Release);
            }
            *self.last_request_id.lock().unwrap() = Some(request.id.clone());
            Ok(())
        }

        fn read_response(&mut self) -> io::Result<String> {
            let last = self.last_request_id.lock().unwrap().clone();
            match last.as_deref() {
                Some("remote-source.tab-list") => {
                    if self.fail_tab_list {
                        Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "fake tab.list failure",
                        ))
                    } else {
                        let tabs = if self.tab_list_has_active_tab {
                            vec![crate::api::schema::TabInfo {
                                tab_id: "tab-1".to_string(),
                                workspace_id: "ws-1".to_string(),
                                number: 1,
                                label: "tab-1".to_string(),
                                focused: true,
                                pane_count: 1,
                                agent_status: crate::api::schema::AgentStatus::Unknown,
                            }]
                        } else {
                            Vec::new()
                        };
                        Ok(serde_json::to_string(&SuccessResponse {
                            id: "remote-source.tab-list".to_string(),
                            result: ResponseResult::TabList { tabs },
                        })
                        .unwrap())
                    }
                }
                Some("remote-source.layout-export") => {
                    if self.fail_layout_export {
                        Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "fake layout.export failure",
                        ))
                    } else {
                        Ok(serde_json::to_string(&SuccessResponse {
                            id: "remote-source.layout-export".to_string(),
                            result: ResponseResult::LayoutExport {
                                layout: crate::api::schema::LayoutDescription {
                                    workspace_id: "ws-1".to_string(),
                                    tab_id: "tab-1".to_string(),
                                    zoomed: false,
                                    focused_pane_id: "pane-1".to_string(),
                                    root: crate::api::schema::LayoutNode::Pane {
                                        pane: crate::api::schema::LayoutPane {
                                            pane_id: Some("pane-1".to_string()),
                                            terminal_id: Some("term-1".to_string()),
                                            ..Default::default()
                                        },
                                    },
                                },
                            },
                        })
                        .unwrap())
                    }
                }
                _ => match &self.primary_response {
                    Ok(response) => Ok(response.clone()),
                    Err(err) => Err(io::Error::new(err.kind(), err.to_string())),
                },
            }
        }

        fn is_alive(&mut self) -> bool {
            true
        }
    }

    fn workspace_rename_success_response() -> String {
        serde_json::to_string(&SuccessResponse {
            id: "x".to_string(),
            result: ResponseResult::WorkspaceInfo {
                workspace: crate::api::schema::WorkspaceInfo {
                    workspace_id: "ws-1".to_string(),
                    number: 1,
                    label: "renamed".to_string(),
                    focused: false,
                    pane_count: 1,
                    tab_count: 1,
                    active_tab_id: "tab-1".to_string(),
                    agent_status: crate::api::schema::AgentStatus::Unknown,
                    worktree: None,
                },
            },
        })
        .unwrap()
    }

    fn mutation_descriptor_with_refresh(
        host: &RemoteHostConfig,
        respond_to: mpsc::Sender<String>,
    ) -> RoutedSequenceDescriptor {
        let mut descriptor = mutation_descriptor(host, respond_to);
        descriptor.spec.refresh = Some(RoutedRefreshSpec {
            workspace_id: "ws-1".to_string(),
            preferred_tab_id: None,
            preferred_from_primary: crate::remote::PreferredTabFromPrimary::None,
        });
        descriptor.spec.tab_list_capable = true;
        descriptor
    }

    /// REQ-3 (round-3 blocker 4): a refresh-leg failure (`tab.list` fails
    /// after a genuinely successful primary) must never park the pooled
    /// connection — it is dropped and its active slot released exactly once,
    /// same as a primary-leg failure — even though the CALLER still sees the
    /// primary success (`PrimarySuccessPreserved`, unchanged caller-facing
    /// taxonomy; REQ-3 governs pool hygiene only).
    #[test]
    fn refresh_leg_failure_does_not_park_the_connection() {
        let host = test_host(&unique_host("pooled-refresh-fail"));
        let key = RemoteHostKey::new(&host.name, &host.session);
        fn starter(
            _pool: &'static RemoteAgentBridgePool,
            host: &RemoteHostConfig,
            _bridge_state: Option<&RemoteApiBridgeState>,
            _primary: &Request,
        ) -> io::Result<WorkerTransport> {
            let key = RemoteHostKey::new(&host.name, &host.session);
            Ok(WorkerTransport::Pooled(
                crate::remote::PooledHandle::for_test(
                    Box::new(FakePooledPrimaryThenRefreshConnection {
                        last_request_id: Mutex::new(None),
                        primary_response: Ok(workspace_rename_success_response()),
                        fail_tab_list: true,
                        tab_list_has_active_tab: false,
                        fail_layout_export: false,
                        refresh_leg_attempted: Arc::new(AtomicBool::new(false)),
                    }),
                    key,
                ),
            ))
        }
        let harness = harness_with_starter(starter);
        harness.executor.env.pool.seed_active_for_test(&key);

        let (respond_to, response_rx) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor_with_refresh(&host, respond_to))
            .expect("enqueue");

        // The caller still sees the primary success — refresh failures never
        // turn an already-successful mutation into an error.
        let response = recv_response(&response_rx);
        assert!(response.contains("renamed"), "response: {response}");

        let deadline = Instant::now() + Duration::from_secs(2);
        while harness.executor.env.pool.active_for(&key) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(harness.executor.env.pool.active_for(&key), 0);
        assert_eq!(
            harness.executor.env.pool.idle_for(&key),
            0,
            "a refresh-leg failure must never park the connection (REQ-3)"
        );
    }

    /// REQ-3 (round-3 blocker 4, the OTHER half): a `layout.export` leg that
    /// was capable and actually attempted (an active tab was selected by a
    /// successful `tab.list`) but itself fails must ALSO never park the
    /// connection — this is the specific failure mode `run_refresh_legs`
    /// silently swallows into `active_tab.layout = None` inside
    /// `RefreshLegOutcome::Data`, which the plain `RefreshLegOutcome::Failed`
    /// check alone would miss.
    #[test]
    fn layout_export_leg_failure_does_not_park_the_connection() {
        let host = test_host(&unique_host("pooled-layout-fail"));
        let key = RemoteHostKey::new(&host.name, &host.session);
        fn starter(
            _pool: &'static RemoteAgentBridgePool,
            host: &RemoteHostConfig,
            _bridge_state: Option<&RemoteApiBridgeState>,
            _primary: &Request,
        ) -> io::Result<WorkerTransport> {
            let key = RemoteHostKey::new(&host.name, &host.session);
            Ok(WorkerTransport::Pooled(
                crate::remote::PooledHandle::for_test(
                    Box::new(FakePooledPrimaryThenRefreshConnection {
                        last_request_id: Mutex::new(None),
                        primary_response: Ok(workspace_rename_success_response()),
                        fail_tab_list: false,
                        tab_list_has_active_tab: true,
                        fail_layout_export: true,
                        refresh_leg_attempted: Arc::new(AtomicBool::new(false)),
                    }),
                    key,
                ),
            ))
        }
        let harness = harness_with_starter(starter);
        harness.executor.env.pool.seed_active_for_test(&key);

        let (respond_to, response_rx) = mpsc::channel();
        let mut descriptor = mutation_descriptor_with_refresh(&host, respond_to);
        descriptor.spec.layout_export_capable = true;
        harness.executor.enqueue(descriptor).expect("enqueue");

        // The caller still sees the primary success — a layout.export leg
        // failure is silently absorbed into `active_tab.layout = None`, never
        // turning the already-successful mutation into an error.
        let response = recv_response(&response_rx);
        assert!(response.contains("renamed"), "response: {response}");

        let deadline = Instant::now() + Duration::from_secs(2);
        while harness.executor.env.pool.active_for(&key) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(harness.executor.env.pool.active_for(&key), 0);
        assert_eq!(
            harness.executor.env.pool.idle_for(&key),
            0,
            "a layout.export leg failure must never park the connection (REQ-3)"
        );
    }

    /// REQ-3/FIX-2 (diff review round 4, blocker 2; corrected round 5,
    /// blocker 1): a malformed (unparseable) primary response — neither a
    /// valid success NOR a valid authoritative error envelope — is "not
    /// clean" even though the transport layer itself reported `Ok` — parsed
    /// BEFORE disposal, so the connection is dropped rather than parked
    /// (REQ-3). FIX-2: the CALLER must also see the honest outcome — the
    /// mutation may already have happened and no authoritative result
    /// exists, so this must resolve `remote_request_indeterminate` (never
    /// `remote_request_failed`, which would falsely imply nothing was sent
    /// and a bare retry is safe) and trigger exactly one reconnect
    /// escalation, same as a genuine post-write transport error. Round 5
    /// blocker 1 also requires refresh legs to never run first (a
    /// descriptor WITH a refresh spec proves this — `mutation_descriptor`
    /// alone never exercised it, since there was nothing to skip).
    #[test]
    fn primary_parse_failure_does_not_park_the_connection() {
        let host = test_host(&unique_host("pooled-parse-fail"));
        let key = RemoteHostKey::new(&host.name, &host.session);
        // `starter` is a plain `fn` pointer (no captures), so the fake
        // connection's externally observable flag reaches it through the
        // same host-keyed registry pattern `fake_leg_registry` already uses
        // for the one-shot fake legs.
        let refresh_leg_attempted = register_refresh_leg_attempted_flag(&host.name);
        fn starter(
            _pool: &'static RemoteAgentBridgePool,
            host: &RemoteHostConfig,
            _bridge_state: Option<&RemoteApiBridgeState>,
            _primary: &Request,
        ) -> io::Result<WorkerTransport> {
            let key = RemoteHostKey::new(&host.name, &host.session);
            let refresh_leg_attempted = lookup_refresh_leg_attempted_flag(&host.name);
            Ok(WorkerTransport::Pooled(
                crate::remote::PooledHandle::for_test(
                    Box::new(FakePooledPrimaryThenRefreshConnection {
                        last_request_id: Mutex::new(None),
                        primary_response: Ok("not valid json".to_string()),
                        fail_tab_list: false,
                        tab_list_has_active_tab: false,
                        fail_layout_export: false,
                        refresh_leg_attempted,
                    }),
                    key,
                ),
            ))
        }
        let mut harness = harness_with_starter(starter);
        harness.executor.env.pool.seed_active_for_test(&key);

        let (respond_to, response_rx) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor_with_refresh(&host, respond_to))
            .expect("enqueue");

        let response = recv_response(&response_rx);
        assert!(
            response.contains("remote_request_indeterminate"),
            "response: {response}"
        );
        assert!(
            !response.contains("remote_request_failed"),
            "a malformed primary must never be reported as a definite failure: {response}"
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        while harness.executor.env.pool.active_for(&key) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(harness.executor.env.pool.active_for(&key), 0);
        assert_eq!(
            harness.executor.env.pool.idle_for(&key),
            0,
            "a malformed primary response must never park the connection (REQ-3)"
        );
        assert!(
            !refresh_leg_attempted.load(Ordering::Acquire),
            "a malformed primary must never run refresh legs (round 5 blocker 1)"
        );

        let mut recovery = 0;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            while let Ok(event) = harness.event_rx.try_recv() {
                if matches!(event, AppEvent::RemoteRoutedRecoveryNeeded { .. }) {
                    recovery += 1;
                }
            }
            if recovery > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            recovery, 1,
            "a malformed primary must trigger exactly one reconnect escalation"
        );
    }

    /// FIX-2 correction (diff review round 5, blocker 1): a VALID,
    /// authoritative `ErrorResponse` (e.g. `pane_not_found`) is NOT
    /// malformed data — the remote answered correctly and definitively said
    /// no. It must be forwarded to the caller UNCHANGED (the error code is
    /// present verbatim in the response), must never run refresh legs
    /// (nothing succeeded to refresh), must never trigger a reconnect (the
    /// transport itself is healthy), and the pooled connection must be
    /// treated as CLEAN — returned to the pool, not dropped — since a
    /// rejected operation says nothing bad about the connection.
    #[test]
    fn authoritative_error_response_is_forwarded_verbatim_with_no_reconnect_or_refresh() {
        let host = test_host(&unique_host("pooled-authoritative-error"));
        let key = RemoteHostKey::new(&host.name, &host.session);
        let refresh_leg_attempted = register_refresh_leg_attempted_flag(&host.name);
        fn starter(
            _pool: &'static RemoteAgentBridgePool,
            host: &RemoteHostConfig,
            _bridge_state: Option<&RemoteApiBridgeState>,
            _primary: &Request,
        ) -> io::Result<WorkerTransport> {
            let key = RemoteHostKey::new(&host.name, &host.session);
            let refresh_leg_attempted = lookup_refresh_leg_attempted_flag(&host.name);
            Ok(WorkerTransport::Pooled(
                crate::remote::PooledHandle::for_test(
                    Box::new(FakePooledPrimaryThenRefreshConnection {
                        last_request_id: Mutex::new(None),
                        primary_response: Ok(
                            r#"{"id":"x","error":{"code":"pane_not_found","message":"no such pane"}}"#
                                .to_string(),
                        ),
                        fail_tab_list: false,
                        tab_list_has_active_tab: false,
                        fail_layout_export: false,
                        refresh_leg_attempted,
                    }),
                    key,
                ),
            ))
        }
        let mut harness = harness_with_starter(starter);
        harness.executor.env.pool.seed_active_for_test(&key);

        let (respond_to, response_rx) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor_with_refresh(&host, respond_to))
            .expect("enqueue");

        // Forwarded verbatim: the authoritative error code/message survive
        // unchanged (only "id" is rewritten to match the caller's request).
        let response = recv_response(&response_rx);
        assert!(
            response.contains("pane_not_found"),
            "authoritative error must be forwarded verbatim: {response}"
        );
        assert!(
            !response.contains("remote_request_indeterminate")
                && !response.contains("remote_request_failed"),
            "an authoritative error must never be reinterpreted as indeterminate/failed: {response}"
        );

        // Clean for pool purposes: returned to the pool, not dropped — the
        // remote answered correctly, so the transport is trusted.
        let deadline = Instant::now() + Duration::from_secs(2);
        while harness.executor.env.pool.active_for(&key) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(harness.executor.env.pool.active_for(&key), 0);
        assert_eq!(
            harness.executor.env.pool.idle_for(&key),
            1,
            "an authoritative error must be treated as a clean sequence and parked, not dropped"
        );

        assert!(
            !refresh_leg_attempted.load(Ordering::Acquire),
            "an authoritative error must never run refresh legs"
        );

        // No reconnect, and no queue cancellation: give the (empty) queue a
        // moment to prove it, the same way other tests confirm an absence.
        std::thread::sleep(Duration::from_millis(100));
        while let Ok(event) = harness.event_rx.try_recv() {
            assert!(
                !matches!(event, AppEvent::RemoteRoutedRecoveryNeeded { .. }),
                "an authoritative error must never trigger a reconnect: {event:?}"
            );
        }
    }

    /// REQ-4/REQ-5/FIX-2: a primary-leg failure AT OR AFTER the first write
    /// attempt over a ONE-SHOT transport (`leg.push_response(Err(..))` is
    /// popped only after `fake_full_leg` sets `wrote = true`, mirroring
    /// production's "set immediately before the write call" ordering — an
    /// ordinary transport error, since there is no timeout anywhere in this
    /// executor, see the module contract) resolves the caller with the
    /// single indeterminate error, marks the affected workspace's cache
    /// stale in the SAME completion event (M1, kept), and triggers exactly
    /// one reconnect escalation.
    #[test]
    fn post_write_failure_reports_indeterminate_marks_stale_and_reconnects_once() {
        let mut harness = harness();
        let host = test_host(&unique_host("prewrite-indet"));
        let leg = register_fake_leg(&host.name);
        leg.push_response(Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "broken pipe",
        )));

        let (respond_to, response_rx) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor_with_refresh(&host, respond_to))
            .expect("enqueue");

        let response = recv_response(&response_rx);
        assert!(
            response.contains("remote_request_indeterminate"),
            "response: {response}"
        );

        let mut stale_seen = false;
        let mut recovery = 0;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            while let Ok(event) = harness.event_rx.try_recv() {
                match event {
                    AppEvent::RemoteRoutedSequenceCompleted { outcome, .. } => {
                        assert_eq!(outcome.taxonomy, RoutedTaxonomy::IndeterminateAfterWrite);
                        assert_eq!(outcome.stale_workspace_id.as_deref(), Some("ws-1"));
                        stale_seen = true;
                    }
                    AppEvent::RemoteRoutedRecoveryNeeded { .. } => {
                        recovery += 1;
                    }
                    _ => {}
                }
            }
            if stale_seen && recovery > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(stale_seen, "stale-marking completion event emitted (M1)");
        assert_eq!(recovery, 1, "exactly one reconnect request");
    }

    /// FIX-1 (diff review round 4, blocker 1 — recovery race): unlike
    /// `recovery_transition_cancels_queued_descriptors` below, which proves
    /// the App-side `cancel_queued_routed_for_reconnect` mechanism by
    /// injecting `RemoteRoutedRecoveryNeeded` manually while a descriptor is
    /// held blocked forever on a real child (never actually completing
    /// through the executor), THIS test exercises the real production
    /// ordering end to end: descriptor #1 is dequeued and held in flight via
    /// `FakeLeg::arm_release_gate` (no manual event injection), descriptor #2
    /// is enqueued and PROVEN still queued (via `leg.requests.len() == 1`)
    /// while #1 is in flight, #1 is then released to fail post-write — its
    /// own `resolve_normal` genuinely computes `Indeterminate` and
    /// synchronously drains the queue (the FIX-1 mechanism) BEFORE
    /// `run_worker`'s loop ever gets a chance to dequeue #2. If the race the
    /// reviewer named were still open, #2 would reach the transport (a
    /// second entry in `leg.requests`) and resolve as an ordinary success
    /// instead of a cancellation.
    #[test]
    fn indeterminate_completion_cancels_next_queued_descriptor_before_it_runs() {
        let harness = harness();
        let host = test_host(&unique_host("recovery-race"));
        let leg = register_fake_leg(&host.name);
        leg.push_response(Err(io::Error::new(
            io::ErrorKind::BrokenPipe,
            "broken pipe",
        )));
        let release = leg.arm_release_gate();

        // #1: enqueue and wait until the worker has genuinely dequeued it
        // and is blocked inside the leg (proven by the recorded request),
        // not merely "probably running by now".
        let (respond1, response1_rx) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor(&host, respond1))
            .expect("enqueue #1");
        let dequeue_deadline = Instant::now() + Duration::from_secs(2);
        while leg.requests.lock().unwrap().is_empty() && Instant::now() < dequeue_deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            leg.requests.lock().unwrap().len(),
            1,
            "descriptor #1 must be dequeued and in flight before #2 is enqueued"
        );

        // #2: enqueued while #1 is provably still in flight — this is the
        // exact window the reviewer named ("the next mutation can begin...
        // before cancellation"). It must still be sitting in the FIFO right
        // now, since the worker thread is blocked inside #1's leg call and
        // cannot have looped back to dequeue anything yet.
        let (respond2, response2_rx) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor(&host, respond2))
            .expect("enqueue #2");

        // Release #1: it fails post-write, `resolve_normal` classifies it
        // Indeterminate, and (FIX-1) synchronously drains the queue before
        // returning — all on the real executor worker thread, no manually
        // injected event.
        let _ = release.send(());

        let response1 = recv_response(&response1_rx);
        assert!(
            response1.contains("remote_request_indeterminate"),
            "response: {response1}"
        );

        // #2 resolves cancelled — never reaching the transport.
        let response2 = recv_response(&response2_rx);
        assert!(
            response2.contains("remote_request_cancelled"),
            "response: {response2}"
        );
        assert_eq!(
            leg.requests.lock().unwrap().len(),
            1,
            "descriptor #2 must never have reached the transport: {:?}",
            leg.requests.lock().unwrap()
        );
    }

    /// FIX-2: a primary-leg failure BEFORE the first write attempt over a
    /// one-shot transport (`fake_full_leg`'s `fail_before_write` — a
    /// connect/spawn/stdin-acquisition failure, never touching the `&mut
    /// bool` out-param) must report the plain safe-to-retry error, NOT
    /// indeterminate, and must NOT trigger a reconnect escalation — nothing
    /// was ever sent, so there is nothing uncertain about the remote's
    /// state. This is the precise distinction REQ-4's unconditional `wrote =
    /// true` had coarsened away; restoring the leg-level out-param brings it
    /// back for one-shot transports.
    #[test]
    fn pre_write_connect_failure_over_one_shot_leg_reports_safe_to_retry() {
        let mut harness = harness();
        let host = test_host(&unique_host("leg-prewrite"));
        let leg = register_fake_leg(&host.name);
        leg.fail_before_write.store(true, Ordering::Release);

        let (respond_to, response_rx) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor(&host, respond_to))
            .expect("enqueue");

        let response = recv_response(&response_rx);
        assert!(
            response.contains("remote_request_failed"),
            "response: {response}"
        );
        assert!(
            !response.contains("indeterminate"),
            "a pre-write connect failure must never be reported as indeterminate: {response}"
        );

        // No reconnect escalation: nothing was sent, the host status is
        // unaffected.
        std::thread::sleep(Duration::from_millis(100));
        while let Ok(event) = harness.event_rx.try_recv() {
            assert!(
                !matches!(event, AppEvent::RemoteRoutedRecoveryNeeded { .. }),
                "a pre-write connect failure must never trigger a reconnect: {event:?}"
            );
        }
    }

    /// REQ-2 (round-3 blocker 2): the ACTUAL recovery transition —
    /// `start_remote_lifecycle_reconnect`, reached here exactly as
    /// production reaches it, via the internal `RemoteRoutedRecoveryNeeded`
    /// escalation the executor emits on an at-or-after-write failure —
    /// cancels every STILL-QUEUED descriptor for the host: resolving a
    /// queued `ApiResponder` with a retryable cancellation, and resolving a
    /// queued create's `UiCreate` sink with a terminal
    /// `RemoteWorkspaceCreateFailed` event (never a silent drop, which would
    /// strand the pending spinner forever). The already-EXECUTING descriptor
    /// is untouched. Round 3 specifically faulted the prior version of this
    /// test for exercising `RemoteSourceDisconnected` directly instead of the
    /// real transition; this drives the actual transition.
    #[test]
    fn recovery_transition_cancels_queued_descriptors() {
        let host_name = unique_host("recovery-cancels");
        let mut app = app_with_remote_host(&host_name);
        app.lifecycle_supervisor_starter = stub_lifecycle_supervisor_starter;
        let host = test_host(&host_name);
        let host_key = RemoteHostKey::new(&host_name, &host.session);
        let harness = harness();
        app.routed_executor = Arc::clone(&harness.executor);
        // The event admission filter requires an active supervisor handle
        // carrying the exact generation the event names. Deliberately NOT
        // `1`: `next_supervisor_generation()` starts its process-unique
        // counter at 1 (see `remote_supervisor.rs`), and this test's own
        // reconnect is the first caller of it in a fresh nextest process, so
        // the FRESH supervisor `start_lifecycle_supervisor` allocates would
        // collide with a stub generation of `1` and falsely pass the
        // "retired" assertion below.
        let stub =
            crate::remote_supervisor::RemoteSourceSupervisorHandle::test_stub(host_key.clone(), 41);
        app.remote_source_supervisors.push(stub);

        let leg = register_fake_leg(&host.name);
        leg.block_until_child_death.store(true, Ordering::Release);

        // #1: dequeued immediately and blocks the worker (holds the queue
        // open so #2/#3 below are provably still QUEUED, never executing).
        let (respond1, _rx1) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor(&host, respond1))
            .expect("enqueue #1");
        // Give the worker a moment to dequeue #1 and start blocking.
        std::thread::sleep(Duration::from_millis(50));

        // #2: a generic queued mutation (API responder sink).
        let (respond2, rx2) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor(&host, respond2))
            .expect("enqueue #2");

        // #3: a queued CREATE.
        let create_token = 55u64;
        let create_descriptor = RoutedSequenceDescriptor {
            host_key: host_key.clone(),
            host: host.clone(),
            source_generation: None,
            bridge_state: None,
            spec: RoutedSequenceSpec {
                primary: Request {
                    id: "create-55".to_string(),
                    method: Method::WorkspaceCreate(crate::api::schema::WorkspaceCreateParams {
                        cwd: None,
                        focus: true,
                        label: None,
                        env: Default::default(),
                    }),
                },
                refresh: None,
                tab_list_capable: false,
                layout_export_capable: false,
                apply: RoutedApply::WorkspaceCreate {
                    token: create_token,
                },
            },
            sink: Some(CompletionSink::UiCreate {
                token: create_token,
            }),
        };
        harness
            .executor
            .enqueue(create_descriptor)
            .expect("enqueue #3 (create)");

        // Drive the ACTUAL recovery transition (REQ-2): the internal event
        // the executor emits on an at-or-after-write failure, which routes
        // through `App::start_remote_routed_recovery` ->
        // `start_remote_lifecycle_reconnect` — never `RemoteSourceDisconnected`
        // directly. Drains #2 and #3 (still queued); #1 (executing, blocked
        // on its real child) is untouched.
        app.handle_internal_event(AppEvent::RemoteRoutedRecoveryNeeded {
            host: host_key.clone(),
            generation: 41,
        });

        let response2 = recv_response(&rx2);
        assert!(
            response2.contains("remote_request_cancelled"),
            "response: {response2}"
        );
        // The queued create's `UiCreate` sink resolves through a terminal
        // failure event, never silently — the pending spinner would
        // otherwise hang forever.
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut resolved = false;
        while Instant::now() < deadline {
            while let Ok(event) = app.event_rx.try_recv() {
                if let AppEvent::RemoteWorkspaceCreateFailed { token, .. } = event {
                    assert_eq!(token, create_token);
                    resolved = true;
                }
            }
            if resolved {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(resolved, "queued create must resolve as a terminal failure");

        // The recovery transition itself actually ran: the generation-41
        // supervisor was retired and a fresh one started.
        assert!(
            app.remote_source_supervisors
                .iter()
                .all(|handle| !(handle.host_key == host_key && handle.generation == 41)),
            "the recovery transition must retire the stale supervisor"
        );
        assert!(
            app.remote_source_supervisors
                .iter()
                .any(|handle| handle.host_key == host_key),
            "the recovery transition must start a fresh supervisor"
        );

        // Cleanup: #1 is still blocked on a real child — kill it so the test
        // does not leave a live process running.
        kill_blocked_child(&leg);
    }

    // --- lifecycle race + overflow -----------------------------------------

    #[test]
    fn enqueue_racing_worker_exit_never_loses_a_sequence() {
        let harness = harness();
        let host = test_host(&unique_host("race"));
        let key = RemoteHostKey::new(&host.name, &host.session);
        // Fast no-op leg: default "ok" response for every request (the
        // registry has no queued responses, so `fake_full_leg` falls back to
        // its canned success).
        let _ = register_fake_leg(&host.name);

        let responders = Arc::new(Mutex::new(Vec::<mpsc::Receiver<String>>::new()));
        let enqueue_handles: Vec<_> = (0..4)
            .map(|_| {
                let executor = Arc::clone(&harness.executor);
                let responders = Arc::clone(&responders);
                let host = host.clone();
                std::thread::spawn(move || {
                    for _ in 0..12 {
                        let (respond_to, response_rx) = mpsc::channel();
                        responders.lock().unwrap().push(response_rx);
                        // Depth-8 overflow under 4 unthrottled racing
                        // producers is genuine flooding, not a lost wakeup
                        // (the design's own terminal-busy contract, v3
                        // Amendment 1); retry until admitted rather than
                        // asserting overflow never happens.
                        loop {
                            match executor.enqueue(mutation_descriptor(&host, respond_to.clone())) {
                                Ok(()) => break,
                                Err(RoutedEnqueueError::Busy) => {
                                    std::thread::sleep(Duration::from_millis(1));
                                }
                            }
                        }
                    }
                })
            })
            .collect();
        for handle in enqueue_handles {
            handle.join().expect("enqueue thread");
        }

        let responders = match Arc::try_unwrap(responders) {
            Ok(mutex) => mutex
                .into_inner()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            Err(_) => panic!("responders arc still shared"),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut responses: Vec<String> = Vec::with_capacity(responders.len());
        for response_rx in responders {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let response = response_rx
                .recv_timeout(remaining)
                .expect("every enqueued sequence resolves");
            responses.push(response);
        }
        assert_eq!(responses.len(), 48);
        // Queue drained and the worker eventually exits (no zombie worker).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let inner = harness.executor.lock();
            let idle = inner
                .hosts
                .get(&key)
                .map(|hq| hq.queue.is_empty() && !hq.worker_alive)
                .unwrap_or(true);
            drop(inner);
            if idle || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let inner = harness.executor.lock();
        let host_queue = inner.hosts.get(&key).expect("host queue");
        assert!(host_queue.queue.is_empty());
    }

    #[test]
    fn worker_exit_respawns_on_late_enqueue_without_lost_wakeup() {
        let harness = harness();
        let host = test_host(&unique_host("respawn"));
        let key = RemoteHostKey::new(&host.name, &host.session);
        let _ = register_fake_leg(&host.name);

        let (first_tx, first_rx) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor(&host, first_tx))
            .expect("enqueue");
        recv_response(&first_rx);

        // Wait for the worker to mark itself dead (queue empty).
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let inner = harness.executor.lock();
            let dead = inner
                .hosts
                .get(&key)
                .map(|hq| !hq.worker_alive)
                .unwrap_or(true);
            drop(inner);
            if dead || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        // A late enqueue after worker exit still completes (fresh worker).
        let (second_tx, second_rx) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor(&host, second_tx))
            .expect("enqueue");
        let response = recv_response(&second_rx);
        assert!(response.contains("result"), "response: {response}");
    }

    #[test]
    fn queue_overflow_resolves_immediately_with_busy_error() {
        // `RoutedExecutorPool::enqueue` itself just returns `Err(Busy)`
        // synchronously and drops the descriptor (its sink never fires at
        // that layer) — the caller wrapper is what turns that into an
        // immediate `remote_bridge_busy` response. Exercise the real
        // production wrapper (`handle_deferred_remote_routed_api_request`,
        // the API-request seam both `runtime.rs` and `headless.rs` call) so
        // this test proves the actual end-to-end contract.
        let host_name = unique_host("overflow");
        let mut app = app_with_remote_host(&host_name);
        mark_connected_with_workspace(&mut app, &host_name);
        let harness = harness();
        app.routed_executor = Arc::clone(&harness.executor);
        let leg = register_fake_leg(&host_name);
        leg.block_until_child_death.store(true, Ordering::Release);

        let (first_tx, _first_rx) = mpsc::channel();
        match app.handle_deferred_remote_routed_api_request(
            rename_request(&host_name, "of-first"),
            first_tx,
        ) {
            DeferredRoutedOutcome::Handled => {}
            other => panic!("expected handled, got {other:?}"),
        }
        // Wait for the worker to actually DEQUEUE and start executing the
        // first (blocking) sequence — confirmed by its blocked child's PID
        // being published — before filling the queue. Otherwise the fill
        // loop races the worker's own dequeue and can overflow early against
        // a queue that still (transiently) holds the first descriptor too.
        let dequeue_deadline = Instant::now() + Duration::from_secs(2);
        while leg.blocked_child_pid.load(Ordering::Acquire) == 0
            && Instant::now() < dequeue_deadline
        {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_ne!(
            leg.blocked_child_pid.load(Ordering::Acquire),
            0,
            "first sequence must be dequeued and blocked before the fill loop starts"
        );
        // Fill the depth-8 FIFO while the first sequence is blocked.
        let mut fill_receivers = Vec::with_capacity(ROUTED_MUTATION_QUEUE_DEPTH);
        for i in 0..ROUTED_MUTATION_QUEUE_DEPTH {
            let (tx, rx) = mpsc::channel();
            match app.handle_deferred_remote_routed_api_request(
                rename_request(&host_name, &format!("of-fill-{i}")),
                tx,
            ) {
                DeferredRoutedOutcome::Handled => {}
                other => panic!("expected queued fill to be handled, got {other:?}"),
            }
            fill_receivers.push(rx);
        }

        // The overflow request resolves IMMEDIATELY with a terminal busy
        // error through its OWN responder — never queued.
        let (overflow_tx, overflow_rx) = mpsc::channel();
        match app.handle_deferred_remote_routed_api_request(
            rename_request(&host_name, "of-overflow"),
            overflow_tx,
        ) {
            DeferredRoutedOutcome::Handled => {}
            other => panic!("expected handled (immediate busy), got {other:?}"),
        }
        let response = overflow_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("immediate busy response");
        assert!(
            response.contains("remote_bridge_busy"),
            "response: {response}"
        );
        drop(fill_receivers);
        // Test hygiene: there is no deadline anywhere in this executor (see
        // the module contract), so nothing reaps the blocked child on its
        // own — kill it directly here so the test does not leave a real
        // process alive past its return (and the parallel-run pressure that
        // creates for unrelated tests).
        kill_blocked_child(&leg);
    }

    /// M2 regression: `try_defer_remote_routed_internal` (the TUI-internal
    /// dispatch seam — a dropped-receiver channel, so the caller relies
    /// entirely on the remote source cache updating later) must NOT report
    /// "queued" when `enqueue` actually failed. A discarded `Err` here means
    /// the mutation never ran and the cache will never update, but the
    /// caller was told to expect it — silently hanging forever.
    #[test]
    fn tui_internal_enqueue_failure_is_not_reported_as_queued() {
        let host_name = unique_host("tui-overflow");
        let mut app = app_with_remote_host(&host_name);
        mark_connected_with_workspace(&mut app, &host_name);
        let harness = harness();
        app.routed_executor = Arc::clone(&harness.executor);
        let leg = register_fake_leg(&host_name);
        leg.block_until_child_death.store(true, Ordering::Release);

        // Dequeue and block the first sequence.
        assert!(app
            .try_defer_remote_routed_internal(&rename_request(&host_name, "ti-first"))
            .is_some());
        let dequeue_deadline = Instant::now() + Duration::from_secs(2);
        while leg.blocked_child_pid.load(Ordering::Acquire) == 0
            && Instant::now() < dequeue_deadline
        {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_ne!(leg.blocked_child_pid.load(Ordering::Acquire), 0);

        // Fill the depth-8 FIFO while the first sequence is blocked.
        for i in 0..ROUTED_MUTATION_QUEUE_DEPTH {
            let response = app
                .try_defer_remote_routed_internal(&rename_request(
                    &host_name,
                    &format!("ti-fill-{i}"),
                ))
                .expect("fill response");
            assert!(
                response.contains("remote_request_deferred"),
                "response: {response}"
            );
        }

        // The overflow request must NOT claim "queued" — nothing was.
        let response = app
            .try_defer_remote_routed_internal(&rename_request(&host_name, "ti-overflow"))
            .expect("overflow response");
        assert!(
            response.contains("remote_bridge_busy"),
            "response: {response}"
        );
        assert!(
            !response.contains("remote_request_deferred"),
            "M2: an enqueue failure must not be reported as queued: {response}"
        );

        kill_blocked_child(&leg);
    }

    // --- read-class concurrency under an active mutation -------------------

    #[test]
    fn read_class_keeps_limiter_concurrency_under_active_mutation() {
        let harness = harness();
        let host = test_host(&unique_host("reads"));
        let key = RemoteHostKey::new(&host.name, &host.session);
        let leg = register_fake_leg(&host.name);
        leg.block_until_child_death.store(true, Ordering::Release);

        let (tx, _rx) = mpsc::channel();
        harness
            .executor
            .enqueue(mutation_descriptor(&host, tx))
            .expect("enqueue");
        // Wait until the mutating sequence holds its one permit.
        let deadline = Instant::now() + Duration::from_secs(2);
        while harness._limiter.in_flight(&key) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(harness._limiter.in_flight(&key), 1);

        // Read-only dispatch shares the limiter, NOT the serial executor:
        // the remaining three permits stay acquirable while the mutation
        // sequence is in flight (REMOTE_AGENT_BRIDGE_PER_HOST_LIMIT = 4).
        let read_permits: Vec<_> = (0..3)
            .map(|_| harness._limiter.try_acquire(&key).expect("read permit"))
            .collect();
        assert_eq!(read_permits.len(), 3);
        assert!(harness._limiter.try_acquire(&key).is_none());
        drop(read_permits);
        // Test hygiene: see the matching comment in
        // `queue_overflow_resolves_immediately_with_busy_error`.
        kill_blocked_child(&leg);
    }

    /// Directly SIGKILL a `FakeLeg`'s real blocked child (spawned via
    /// `block_until_child_death`) so a test does not leave it running past
    /// its own return. There is no deadline anywhere in this executor (see
    /// the module contract), so nothing else would ever reap it — this is
    /// the only cleanup, and it also avoids the extra live process/thread
    /// while unrelated tests run in parallel.
    fn kill_blocked_child(leg: &FakeLeg) {
        let pid = leg.blocked_child_pid.load(Ordering::Acquire);
        if pid != 0 {
            let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
    }

    // --- planner + generation suppression (App-level) -----------------------

    fn app_with_remote_host(host: &str) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut config = crate::config::Config::default();
        config.remote.enabled = true;
        config.remote.hosts = vec![test_host(host)];
        App::new(&config, true, None, api_rx, crate::api::EventHub::default())
    }

    fn mark_connected_with_workspace(app: &mut App, host: &str) -> RemoteHostKey {
        let host_key = RemoteHostKey::new(host, crate::session::DEFAULT_SESSION_NAME);
        app.state
            .remote_sources
            .replace_connected_snapshot(host_key.clone(), Vec::new());
        app.state
            .remote_sources
            .replace_workspace_snapshot(host_key.clone(), vec![workspace_info("ws-1")]);
        app.state.remote_sources.set_capabilities(
            &host_key,
            RemoteSourceCapabilities {
                workspace_rename: true,
                workspace_create: true,
                ..Default::default()
            },
        );
        // Ping-connected (bridge state applied) but NO snapshot applied yet.
        app.state.remote_sources.set_connected_bridge_state(
            &host_key,
            crate::remote::RemoteApiBridgeState {
                shell_path: "\"$HOME/.local/bin/herdr\"".to_string(),
                capabilities: crate::api::schema::FederationCapabilities::current(),
            },
        );
        assert_eq!(
            app.state.remote_sources.host_status(&host_key),
            Some(RemoteConnectionStatus::Connected)
        );
        host_key
    }

    fn workspace_info(workspace_id: &str) -> crate::api::schema::WorkspaceInfo {
        crate::api::schema::WorkspaceInfo {
            workspace_id: workspace_id.to_string(),
            number: 1,
            label: workspace_id.to_string(),
            focused: true,
            pane_count: 1,
            tab_count: 1,
            active_tab_id: "tab-1".to_string(),
            agent_status: crate::api::schema::AgentStatus::Unknown,
            worktree: None,
        }
    }

    fn rename_request(host: &str, id: &str) -> Request {
        Request {
            id: id.to_string(),
            method: Method::WorkspaceRename(WorkspaceRenameParams {
                workspace_id: format!("{host}/workspace:ws-1"),
                label: "renamed".to_string(),
            }),
        }
    }

    /// C2 regression: `tab.close` must reject an unconfirmed remote close at
    /// the PLANNER, exactly like `pane.close`, never silently promoting it to
    /// confirmed. `tabs::remote_tab_close_request` always forwards
    /// `confirm: true`, so this gate is the only thing standing between an
    /// unconfirmed API request and a destructive remote close; it must fire
    /// before any cache/status resolution (mirrors
    /// `remote_tab_close_requires_confirm_before_status_or_cache_resolution`
    /// for the synchronous handler).
    #[test]
    fn planner_rejects_unconfirmed_remote_tab_close() {
        let host_name = unique_host("tabclose");
        let app = app_with_remote_host(&host_name);

        let request = Request {
            id: "req-1".to_string(),
            method: Method::TabClose(crate::api::schema::TabCloseParams {
                tab_id: format!("{host_name}/tab:remote-tab-1"),
                confirm: false,
            }),
        };
        match app.plan_remote_routed_mutation(&request) {
            RoutedPlanOutcome::Immediate(response) => {
                assert!(
                    response.contains("confirmation_required"),
                    "response: {response}"
                );
            }
            _ => panic!("expected planner rejection"),
        }
    }

    /// C2 regression (positive case): a CONFIRMED remote `tab.close` still
    /// plans normally (deferred), so the new gate does not reject legitimate
    /// confirmed closes.
    #[test]
    fn planner_accepts_confirmed_remote_tab_close() {
        let host_name = unique_host("tabclose-ok");
        let mut app = app_with_remote_host(&host_name);
        let host_key = mark_connected_with_workspace(&mut app, &host_name);
        app.state.remote_sources.replace_tab_snapshot(
            &host_key,
            "ws-1",
            vec![crate::api::schema::TabInfo {
                tab_id: "remote-tab-1".to_string(),
                workspace_id: "ws-1".to_string(),
                number: 1,
                label: "remote-tab-1".to_string(),
                focused: true,
                pane_count: 1,
                agent_status: crate::api::schema::AgentStatus::Unknown,
            }],
        );
        app.state.remote_sources.set_capabilities(
            &host_key,
            RemoteSourceCapabilities {
                tab_close: true,
                ..Default::default()
            },
        );

        let request = Request {
            id: "req-2".to_string(),
            method: Method::TabClose(crate::api::schema::TabCloseParams {
                tab_id: format!("{host_name}/tab:remote-tab-1"),
                confirm: true,
            }),
        };
        match app.plan_remote_routed_mutation(&request) {
            RoutedPlanOutcome::Deferred(_) => {}
            _ => panic!("expected deferred plan"),
        }
    }

    /// H2 regression: `pane.focus`, `pane.focus_direction`, and `pane.rename`
    /// each gate on their OWN federation capability — `plan_pane_mutation`
    /// must not hardcode `PANE_FOCUS` for all three. A host advertising ONLY
    /// `pane_focus_direction` accepts that method (fails later for an
    /// unrelated reason — no pane in cache — never for a capability
    /// mismatch) but rejects `pane.focus` and `pane.rename` by name.
    #[test]
    fn planner_gates_pane_mutations_by_their_own_capability() {
        let host_name = unique_host("panecap");
        let mut app = app_with_remote_host(&host_name);
        let host_key = mark_connected_with_workspace(&mut app, &host_name);
        app.state.remote_sources.set_capabilities(
            &host_key,
            RemoteSourceCapabilities {
                pane_focus_direction: true,
                // pane_focus and pane_rename are deliberately left false.
                ..Default::default()
            },
        );
        let pane_id = format!("{host_name}/pane:p1");

        // Advertised capability: never rejected for `pane_focus_unavailable`
        // via the wrong (shared) flag.
        let focus_direction = Request {
            id: "fd".to_string(),
            method: Method::PaneFocusDirection(crate::api::schema::PaneFocusDirectionParams {
                pane_id: Some(pane_id.clone()),
                direction: crate::api::schema::PaneDirection::Left,
            }),
        };
        if let RoutedPlanOutcome::Immediate(response) =
            app.plan_remote_routed_mutation(&focus_direction)
        {
            assert!(
                !response.contains("remote_capability_unavailable"),
                "pane.focus_direction must not be rejected for capability when \
                 pane_focus_direction is advertised: {response}"
            );
        }

        // Not advertised: pane.focus rejected for ITS OWN capability, not
        // silently allowed via pane_focus_direction.
        let focus = Request {
            id: "f".to_string(),
            method: Method::PaneFocus(crate::api::schema::PaneTarget {
                pane_id: pane_id.clone(),
            }),
        };
        match app.plan_remote_routed_mutation(&focus) {
            RoutedPlanOutcome::Immediate(response) => {
                assert!(
                    response.contains("remote_capability_unavailable"),
                    "response: {response}"
                );
                assert!(response.contains("pane_focus"), "response: {response}");
            }
            _ => panic!("expected pane.focus to be rejected (capability not advertised)"),
        }

        // Not advertised: pane.rename rejected for ITS OWN capability too.
        let rename = Request {
            id: "r".to_string(),
            method: Method::PaneRename(crate::api::schema::PaneRenameParams {
                pane_id: pane_id.clone(),
                label: Some("x".to_string()),
            }),
        };
        match app.plan_remote_routed_mutation(&rename) {
            RoutedPlanOutcome::Immediate(response) => {
                assert!(
                    response.contains("remote_capability_unavailable"),
                    "response: {response}"
                );
                assert!(response.contains("pane_rename"), "response: {response}");
            }
            _ => panic!("expected pane.rename to be rejected (capability not advertised)"),
        }
    }

    /// Superseded-generation apply suppression: a completion event whose
    /// generation no longer matches an active supervisor is dropped by App
    /// admission (no refresh application, no reconciliation stamp).
    #[test]
    fn superseded_generation_completion_is_suppressed() {
        let host_name = unique_host("superseded");
        let mut app = app_with_remote_host(&host_name);
        let host_key = mark_connected_with_workspace(&mut app, &host_name);

        let stub =
            crate::remote_supervisor::RemoteSourceSupervisorHandle::test_stub(host_key.clone(), 11);
        app.remote_source_supervisors.push(stub);

        let completion = crate::remote::RoutedCompletion {
            taxonomy: RoutedTaxonomy::IndeterminateAfterWrite,
            primary: None,
            refresh: None,
            stale_workspace_id: Some("ws-1".to_string()),
            apply: RoutedApply::RefreshOnly,
            route: "pooled",
        };

        // Stale generation (10): admission drops it before the reducer.
        app.handle_internal_event(AppEvent::RemoteRoutedSequenceCompleted {
            host: host_key.clone(),
            generation: 10,
            outcome: Box::new(completion.clone()),
        });
        assert!(
            !app.state.remote_routed_reconciled.contains_key(&host_key),
            "superseded generation must not stamp reconciliation"
        );
        // The projection was NOT marked stale (suppressed application).
        assert!(app
            .state
            .remote_sources
            .projections_for_host(&host_key)
            .is_empty());

        // Current generation (11): admitted, applied, stamped.
        app.handle_internal_event(AppEvent::RemoteRoutedSequenceCompleted {
            host: host_key.clone(),
            generation: 11,
            outcome: Box::new(completion),
        });
        assert_eq!(
            app.state.remote_routed_reconciled.get(&host_key),
            Some(&11),
            "current generation stamps reconciliation"
        );
    }

    /// Fake lifecycle supervisor starter (mirrors `remotes.rs`'s private
    /// `stub_supervisor_starter`, duplicated here since it is not visible
    /// across modules): an inert stub handle, no worker thread, no real SSH.
    fn stub_lifecycle_supervisor_starter(
        host: RemoteHostConfig,
        _event_tx: tokio_mpsc::Sender<AppEvent>,
        generation: u64,
    ) -> crate::remote_supervisor::RemoteSourceSupervisorHandle {
        crate::remote_supervisor::RemoteSourceSupervisorHandle::test_stub(
            RemoteHostKey::new(host.name, host.session),
            generation,
        )
    }

    /// Superseded-generation apply suppression for the executor's recovery
    /// escalation (mirrors `superseded_generation_completion_is_suppressed`
    /// above): a `RemoteRoutedRecoveryNeeded` event whose generation no
    /// longer matches an active supervisor is dropped by App admission
    /// BEFORE it can retire an already-recovered supervisor. A current-
    /// generation event is admitted and drives the named recovery
    /// transition (retire + fresh supervisor) exactly once.
    #[test]
    fn superseded_generation_recovery_needed_is_suppressed() {
        let host_name = unique_host("recovery-superseded");
        let mut app = app_with_remote_host(&host_name);
        app.lifecycle_supervisor_starter = stub_lifecycle_supervisor_starter;
        let host_key = mark_connected_with_workspace(&mut app, &host_name);

        let stub =
            crate::remote_supervisor::RemoteSourceSupervisorHandle::test_stub(host_key.clone(), 11);
        app.remote_source_supervisors.push(stub);

        // Stale generation (10): admission drops it before recovery fires —
        // the generation-11 supervisor (already recovered via another path)
        // must survive untouched, never retired.
        app.handle_internal_event(AppEvent::RemoteRoutedRecoveryNeeded {
            host: host_key.clone(),
            generation: 10,
        });
        assert_eq!(app.remote_source_supervisors.len(), 1);
        assert!(
            app.remote_source_supervisors
                .iter()
                .any(|handle| handle.host_key == host_key && handle.generation == 11),
            "superseded generation must not retire the current supervisor"
        );

        // Current generation (11): admitted, recovery fires — the
        // generation-11 supervisor is retired and a fresh one started.
        app.handle_internal_event(AppEvent::RemoteRoutedRecoveryNeeded {
            host: host_key.clone(),
            generation: 11,
        });
        assert!(
            app.remote_source_supervisors
                .iter()
                .all(|handle| !(handle.host_key == host_key && handle.generation == 11)),
            "current generation must retire the stale supervisor: {:?}",
            app.remote_source_supervisors
                .iter()
                .map(|h| (h.host_key.clone(), h.generation))
                .collect::<Vec<_>>()
        );
        assert!(
            app.remote_source_supervisors
                .iter()
                .any(|handle| handle.host_key == host_key),
            "a fresh supervisor must be started after admitted recovery"
        );
    }

    // --- UI (create) sink ----------------------------------------------------

    #[test]
    fn ui_create_sink_resolves_exactly_once_on_success() {
        let mut harness = harness();
        let host = test_host(&unique_host("create"));
        let leg = register_fake_leg(&host.name);
        let workspace = workspace_info("ws-new");
        leg.push_response(Ok(serde_json::to_string(&SuccessResponse {
            id: "remote-workspace.create.1".to_string(),
            result: ResponseResult::WorkspaceCreated {
                workspace: workspace.clone(),
                tab: crate::api::schema::TabInfo {
                    tab_id: "tab-1".to_string(),
                    workspace_id: "ws-new".to_string(),
                    number: 1,
                    label: "1".to_string(),
                    focused: true,
                    pane_count: 1,
                    agent_status: crate::api::schema::AgentStatus::Unknown,
                },
                root_pane: crate::api::schema::PaneInfo {
                    pane_id: "pane-1".to_string(),
                    terminal_id: "term-1".to_string(),
                    workspace_id: "ws-new".to_string(),
                    tab_id: "tab-1".to_string(),
                    cwd: None,
                    foreground_cwd: None,
                    label: None,
                    focused: true,
                    agent: None,
                    agent_status: crate::api::schema::AgentStatus::Unknown,
                    title: None,
                    display_agent: None,
                    custom_status: None,
                    state_labels: std::collections::HashMap::new(),
                    agent_session: None,
                    revision: 1,
                },
            },
        })
        .unwrap()));

        let descriptor = RoutedSequenceDescriptor {
            host_key: RemoteHostKey::new(&host.name, &host.session),
            host,
            source_generation: None,
            bridge_state: None,
            spec: RoutedSequenceSpec {
                primary: Request {
                    id: "remote-workspace.create.1".to_string(),
                    method: Method::WorkspaceCreate(crate::api::schema::WorkspaceCreateParams {
                        cwd: None,
                        focus: true,
                        label: None,
                        env: Default::default(),
                    }),
                },
                refresh: None,
                tab_list_capable: false,
                layout_export_capable: false,
                apply: RoutedApply::WorkspaceCreate { token: 1 },
            },
            sink: Some(CompletionSink::UiCreate { token: 1 }),
        };
        harness.executor.enqueue(descriptor).expect("enqueue");

        // Exactly ONE create event resolves the UI sink.
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut successes = 0;
        while Instant::now() < deadline {
            while let Ok(event) = harness.event_rx.try_recv() {
                match event {
                    AppEvent::RemoteWorkspaceCreateSucceeded { token, .. } => {
                        assert_eq!(token, 1);
                        successes += 1;
                    }
                    AppEvent::RemoteWorkspaceCreateFailed { .. } => {
                        panic!("unexpected create failure event")
                    }
                    _ => {}
                }
            }
            if successes == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(successes, 1, "exactly one create-success event");
        std::thread::sleep(Duration::from_millis(100));
        while let Ok(event) = harness.event_rx.try_recv() {
            assert!(
                !matches!(
                    event,
                    AppEvent::RemoteWorkspaceCreateSucceeded { .. }
                        | AppEvent::RemoteWorkspaceCreateFailed { .. }
                ),
                "no second terminal create event"
            );
        }
    }

    /// H5 regression: every terminal path that resolves a `UiCreate` sink
    /// must emit a matching create `AppEvent` — `resolve_success`/
    /// `resolve_error` are deliberate no-ops for `UiCreate`, so a branch that
    /// only calls those (without ALSO emitting the event) silently strands
    /// the pending spinner forever. Exercises `resolve_sink_terminal`'s
    /// `PreWriteFailure` (v11 removed the deadline/kill/classification
    /// machinery, so `resolve_normal` resolves every worker-completion
    /// outcome directly from the worker's own result; `PermitWaitExpired`,
    /// FIX-1's other `TerminalOutcome` variant, is covered separately by
    /// `saturated_permit_wait_expires_with_busy_error_not_a_reconnect`), plus
    /// `resolve_normal`'s `PreWriteFailure` and `Indeterminate` (no claim/
    /// uncertain-retry tracking anymore — an indeterminate outcome resolves
    /// as a plain failure).
    #[test]
    fn ui_create_sink_resolves_on_every_terminal_path() {
        fn descriptor_for(token: u64) -> RoutedSequenceDescriptor {
            RoutedSequenceDescriptor {
                host_key: RemoteHostKey::new("h5host", crate::session::DEFAULT_SESSION_NAME),
                host: test_host("h5host"),
                source_generation: None,
                bridge_state: None,
                spec: RoutedSequenceSpec {
                    primary: Request {
                        id: format!("create-{token}"),
                        method: Method::WorkspaceCreate(
                            crate::api::schema::WorkspaceCreateParams {
                                cwd: None,
                                focus: true,
                                label: None,
                                env: Default::default(),
                            },
                        ),
                    },
                    refresh: None,
                    tab_list_capable: false,
                    layout_export_capable: false,
                    apply: RoutedApply::WorkspaceCreate { token },
                },
                sink: Some(CompletionSink::UiCreate { token }),
            }
        }

        fn drain_next(event_rx: &mut tokio_mpsc::Receiver<AppEvent>) -> AppEvent {
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                if let Ok(event) = event_rx.try_recv() {
                    if matches!(
                        event,
                        AppEvent::RemoteWorkspaceCreateSucceeded { .. }
                            | AppEvent::RemoteWorkspaceCreateFailed { .. }
                    ) {
                        return event;
                    }
                }
                assert!(Instant::now() < deadline, "no terminal create event");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        // 1. resolve_sink_terminal / TerminalOutcome::PreWriteFailure.
        {
            let mut harness = harness();
            resolve_sink_terminal(
                &harness.executor.env,
                descriptor_for(101),
                TerminalOutcome::PreWriteFailure("connect refused".to_string()),
            );
            match drain_next(&mut harness.event_rx) {
                AppEvent::RemoteWorkspaceCreateFailed { token, .. } => assert_eq!(token, 101),
                other => panic!("expected create-failed, got {other:?}"),
            }
        }

        // 2. resolve_normal / TaxonomyChoice::Indeterminate: no claim/
        //    uncertain-retry tracking — the create resolves as a plain
        //    failure so the spinner clears and a retry can proceed.
        {
            let mut harness = harness();
            let host_key = RemoteHostKey::new("h5host", crate::session::DEFAULT_SESSION_NAME);
            let final_ = SequenceFinal {
                primary: Err(RoutedIoError {
                    kind: io::ErrorKind::UnexpectedEof,
                    message: "eof".to_string(),
                }),
                refresh: RefreshLegOutcome::None,
                wrote: true,
                route: "pooled",
            };
            harness
                .executor
                .resolve_normal(&host_key, descriptor_for(102), &final_);
            match drain_next(&mut harness.event_rx) {
                AppEvent::RemoteWorkspaceCreateFailed { token, .. } => assert_eq!(token, 102),
                other => panic!("expected create-failed, got {other:?}"),
            }
        }

        // 3. resolve_normal / TaxonomyChoice::PreWriteFailure: the worker
        //    completed normally but never attempted a write.
        {
            let mut harness = harness();
            let host_key = RemoteHostKey::new("h5host", crate::session::DEFAULT_SESSION_NAME);
            let final_ = SequenceFinal {
                primary: Err(RoutedIoError {
                    kind: io::ErrorKind::ConnectionRefused,
                    message: "spawn failed".to_string(),
                }),
                refresh: RefreshLegOutcome::None,
                wrote: false,
                route: "one_shot_full",
            };
            harness
                .executor
                .resolve_normal(&host_key, descriptor_for(103), &final_);
            match drain_next(&mut harness.event_rx) {
                AppEvent::RemoteWorkspaceCreateFailed { token, .. } => assert_eq!(token, 103),
                other => panic!("expected create-failed, got {other:?}"),
            }
        }
    }

    // --- refresh application through the reducer ----------------------------

    #[test]
    fn completion_refresh_data_applies_tabs_and_projection_atomically() {
        let host = RemoteHostKey::new(unique_host("refresh"), crate::session::DEFAULT_SESSION_NAME);
        let mut state = AppState::test_new();
        state
            .remote_sources
            .replace_connected_snapshot(host.clone(), Vec::new());

        let tabs = vec![crate::api::schema::TabInfo {
            tab_id: "tab-9".to_string(),
            workspace_id: "ws-1".to_string(),
            number: 9,
            label: "9".to_string(),
            focused: true,
            pane_count: 2,
            agent_status: crate::api::schema::AgentStatus::Unknown,
        }];
        state.apply_routed_completion(
            &host,
            1,
            crate::remote::RoutedCompletion {
                taxonomy: RoutedTaxonomy::Completed,
                primary: Some("{\"result\":{}}".to_string()),
                refresh: Some(crate::remote::RoutedRefreshData {
                    workspace_id: "ws-1".to_string(),
                    tabs: Some(tabs),
                    active_tab: Some(crate::remote::RoutedActiveTabFetch {
                        tab_id: Some("tab-9".to_string()),
                        tab_label: Some("9".to_string()),
                        layout: None,
                    }),
                }),
                stale_workspace_id: None,
                apply: RoutedApply::RefreshOnly,
                route: "pooled",
            },
        );
        let snapshot =
            state
                .remote_sources
                .tab_snapshot_for_space(&crate::remote_source::RemoteSpaceKey {
                    host: host.host.clone(),
                    session: host.session.clone(),
                    workspace_id: "ws-1".to_string(),
                });
        assert!(snapshot.is_some(), "tab snapshot replaced");
        assert_eq!(
            state.remote_routed_reconciled.get(&host),
            Some(&1),
            "reconciliation stamped"
        );

        // Stale marking when refresh data is absent (indeterminate).
        state.apply_routed_completion(
            &host,
            2,
            crate::remote::RoutedCompletion {
                taxonomy: RoutedTaxonomy::IndeterminateAfterWrite,
                primary: None,
                refresh: None,
                stale_workspace_id: Some("ws-1".to_string()),
                apply: RoutedApply::RefreshOnly,
                route: "pooled",
            },
        );
        let snapshot =
            state
                .remote_sources
                .tab_snapshot_for_space(&crate::remote_source::RemoteSpaceKey {
                    host: host.host.clone(),
                    session: host.session.clone(),
                    workspace_id: "ws-1".to_string(),
                });
        assert!(
            snapshot.is_none_or(
                |entry| entry.status != crate::remote_source::RemoteProjectionStatus::Available
            ),
            "stale marking after indeterminate"
        );
    }
}
