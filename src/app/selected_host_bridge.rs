//! Runtime owner for the selected remote host's glass transport bridge.
//!
//! `AppState` remains pure. The bridge listener, accepted stream worker,
//! socket, and retirement reaper live here on `App`; host glass opens the one
//! full-App byte stream through this owner.

#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

#[cfg(unix)]
use crate::remote_source::RemoteHostKey;

#[cfg(unix)]
const SELECTED_HOST_BRIDGE_MAX_STREAMS: usize = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectedHostBridgeConsumer {
    HostGlass,
}

#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
struct SelectedHostBridgeSignature {
    host: RemoteHostKey,
    prepared_shell_path: String,
}

#[cfg(unix)]
struct RetiredSelectedHostBridge {
    bridge: crate::remote::SshStdioBridge,
    local_socket: PathBuf,
}

#[cfg(unix)]
struct SelectedHostBridgeReaper {
    tx: std::sync::mpsc::Sender<RetiredSelectedHostBridge>,
}

#[cfg(unix)]
type SelectedHostBridgeStarter = Arc<
    dyn Fn(
            &crate::remote_target::RemoteHostConfig,
            &crate::remote::RemoteApiBridgeState,
            PathBuf,
            usize,
        ) -> std::io::Result<crate::remote::SshStdioBridge>
        + Send
        + Sync,
>;

#[cfg(unix)]
impl Default for SelectedHostBridgeReaper {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<RetiredSelectedHostBridge>();
        std::thread::spawn(move || {
            while let Ok(retired) = rx.recv() {
                // SshStdioBridge owns its bounded shutdown budget. Keeping the
                // drop on this dedicated worker makes selected-host switches
                // independent of that budget on the App/event loop.
                drop(retired.bridge);
                let _ = std::fs::remove_file(retired.local_socket);
            }
        });
        Self { tx }
    }
}

/// Sole bridge owner for the selected host's full-App glass stream.
///
/// A retired bridge may coexist only while its off-loop reaper drains the
/// predecessor worker.
#[cfg_attr(not(unix), derive(Default))]
pub(crate) struct SelectedHostBridgeRuntime {
    #[cfg(unix)]
    signature: Option<SelectedHostBridgeSignature>,
    #[cfg(unix)]
    bridge: Option<crate::remote::SshStdioBridge>,
    #[cfg(unix)]
    local_socket: Option<PathBuf>,
    #[cfg(unix)]
    glass_lease: bool,
    #[cfg(unix)]
    reaper: SelectedHostBridgeReaper,
    #[cfg(unix)]
    starter: SelectedHostBridgeStarter,
}

#[cfg(unix)]
impl Default for SelectedHostBridgeRuntime {
    fn default() -> Self {
        Self {
            signature: None,
            bridge: None,
            local_socket: None,
            glass_lease: false,
            reaper: SelectedHostBridgeReaper::default(),
            starter: Arc::new(crate::remote::start_projection_bridge),
        }
    }
}

impl SelectedHostBridgeRuntime {
    #[cfg(unix)]
    pub(crate) fn is_acquired_by(
        &self,
        consumer: SelectedHostBridgeConsumer,
        host: &RemoteHostKey,
        prepared: &crate::remote::RemoteApiBridgeState,
    ) -> bool {
        self.signature.as_ref()
            == Some(&SelectedHostBridgeSignature {
                host: host.clone(),
                prepared_shell_path: prepared.shell_path.clone(),
            })
            && self.bridge.is_some()
            && match consumer {
                SelectedHostBridgeConsumer::HostGlass => self.glass_lease,
            }
    }

    #[cfg(unix)]
    pub(crate) fn acquire(
        &mut self,
        consumer: SelectedHostBridgeConsumer,
        host: &RemoteHostKey,
        host_config: &crate::remote_target::RemoteHostConfig,
        prepared: &crate::remote::RemoteApiBridgeState,
    ) -> std::io::Result<PathBuf> {
        let signature = SelectedHostBridgeSignature {
            host: host.clone(),
            prepared_shell_path: prepared.shell_path.clone(),
        };
        if self.signature.as_ref() != Some(&signature) {
            self.retire();
            self.glass_lease = false;
            let socket = selected_host_bridge_socket_path();
            let bridge = (self.starter)(
                host_config,
                prepared,
                socket.clone(),
                SELECTED_HOST_BRIDGE_MAX_STREAMS,
            )?;
            self.signature = Some(signature);
            self.local_socket = Some(socket);
            self.bridge = Some(bridge);
        }
        match consumer {
            SelectedHostBridgeConsumer::HostGlass => self.glass_lease = true,
        }
        self.local_socket
            .clone()
            .ok_or_else(|| std::io::Error::other("selected-host bridge socket unavailable"))
    }

    pub(crate) fn release(&mut self, consumer: SelectedHostBridgeConsumer) {
        #[cfg(unix)]
        {
            match consumer {
                SelectedHostBridgeConsumer::HostGlass => self.glass_lease = false,
            }
            if !self.glass_lease {
                self.retire();
            }
        }
        #[cfg(not(unix))]
        let _ = consumer;
    }

    #[cfg(unix)]
    fn retire(&mut self) {
        self.signature = None;
        let retired_bridge = self.bridge.take();
        let retired_socket = self.local_socket.take();
        self.reap_retired(retired_bridge, retired_socket);
    }

    #[cfg(unix)]
    fn reap_retired(
        &self,
        retired_bridge: Option<crate::remote::SshStdioBridge>,
        retired_socket: Option<PathBuf>,
    ) {
        let Some(bridge) = retired_bridge else {
            return;
        };
        let Some(local_socket) = retired_socket else {
            // This state cannot be constructed through `acquire`; if it is
            // ever reached, leak rather than synchronously run bridge Drop on
            // the App loop.
            std::mem::forget(bridge);
            return;
        };
        if let Err(err) = self.reaper.tx.send(RetiredSelectedHostBridge {
            bridge,
            local_socket,
        }) {
            // The reaper is process-lifetime infrastructure. If it died, keep
            // the event loop nonblocking even under this impossible invariant
            // violation; process teardown will reclaim the leaked bridge.
            std::mem::forget(err.0);
            tracing::error!("selected-host bridge reaper stopped unexpectedly");
        }
    }

    #[cfg(all(test, unix))]
    fn with_starter(starter: SelectedHostBridgeStarter) -> Self {
        Self {
            signature: None,
            bridge: None,
            local_socket: None,
            glass_lease: false,
            reaper: SelectedHostBridgeReaper::default(),
            starter,
        }
    }
}

impl Drop for SelectedHostBridgeRuntime {
    fn drop(&mut self) {
        #[cfg(unix)]
        self.retire();
    }
}

#[cfg(unix)]
fn selected_host_bridge_socket_path() -> PathBuf {
    static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);
    let serial = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
    let name = format!("herdr-selected-host-{}-{serial}.sock", std::process::id());
    let in_tmp = std::env::temp_dir().join(&name);
    use std::os::unix::ffi::OsStrExt;
    if in_tmp.as_os_str().as_bytes().len() <= 103 {
        in_tmp
    } else {
        PathBuf::from("/tmp").join(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn selected_host_bridge_reserves_exactly_one_stream_for_glass() {
        assert_eq!(SELECTED_HOST_BRIDGE_MAX_STREAMS, 1);
    }

    #[test]
    #[cfg(unix)]
    fn selected_host_bridge_reuses_glass_bridge_and_lease() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::sync::atomic::AtomicUsize;

        let program = selected_host_bridge_socket_path().with_extension("sh");
        std::fs::write(&program, "#!/bin/sh\nexec cat\n").expect("write fake ssh program");
        let mut permissions = std::fs::metadata(&program)
            .expect("fake ssh metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&program, permissions).expect("make fake ssh executable");

        let starts = Arc::new(AtomicUsize::new(0));
        let starter_starts = Arc::clone(&starts);
        let starter_program = program.clone();
        let starter: SelectedHostBridgeStarter = Arc::new(move |_, _, socket, max| {
            assert_eq!(max, 1);
            starter_starts.fetch_add(1, Ordering::Relaxed);
            crate::remote::start_test_projection_bridge(socket, max, starter_program.clone())
        });
        let mut owner = SelectedHostBridgeRuntime::with_starter(starter);
        let host_key = RemoteHostKey::new("remote-a", "default");
        let host = crate::remote_target::RemoteHostConfig::new(
            "remote-a",
            "ignored-target",
            "default",
            true,
        );
        let prepared = crate::remote::RemoteApiBridgeState {
            shell_path: "/ignored/herdr".into(),
            capabilities: crate::api::schema::FederationCapabilities::current(),
        };

        let first_socket = owner
            .acquire(
                SelectedHostBridgeConsumer::HostGlass,
                &host_key,
                &host,
                &prepared,
            )
            .expect("start glass bridge");
        let reused_socket = owner
            .acquire(
                SelectedHostBridgeConsumer::HostGlass,
                &host_key,
                &host,
                &prepared,
            )
            .expect("reuse glass bridge");

        assert_eq!(reused_socket, first_socket);
        assert_eq!(starts.load(Ordering::Relaxed), 1);
        assert!(owner.is_acquired_by(SelectedHostBridgeConsumer::HostGlass, &host_key, &prepared,));

        owner.release(SelectedHostBridgeConsumer::HostGlass);
        drop(owner);
        let _ = std::fs::remove_file(program);
    }
}
