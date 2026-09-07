//! End-to-end integration tests for host glass across view contexts.
//!
//! These spawn real `herdr server` processes and attach real clients over the
//! unix client socket, declaring `ViewContext::Standalone` (a human's terminal)
//! or `ViewContext::Embedded` (another Herdr streaming this host through its
//! own glass) in the Hello handshake. No mocks and no SSH: an embedded client
//! is just a second wire client with a different Hello field, which is enough
//! to exercise both host-glass invariants on a single machine.
//!
//! Isolation discipline matches the other integration suites: every test gets
//! its own `/tmp` base, its own `XDG_CONFIG_HOME`/`XDG_RUNTIME_DIR`, its own
//! client and API sockets, and full teardown through `cleanup_test_base`. No
//! real herdr session, socket, or config on the machine is touched.

mod support;

use std::collections::VecDeque;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Deserialize;
use serde_json::Value;
use support::{
    cleanup_test_base, encode_varint_u16, encode_varint_u32, frame_message, register_runtime_dir,
    register_spawned_herdr_pid, unregister_spawned_herdr_pid, CURRENT_PROTOCOL,
};

/// `ViewContext` discriminants as bincode encodes them, mirroring the field
/// order of `src/protocol/wire.rs::ViewContext`. Standalone is declared first,
/// so it is 0 and Embedded is 1; `view_context_discriminants_match_the_wire_enum`
/// pins that against the shipped `herdr api schema`-adjacent wire definition by
/// asserting the server accepts both and treats them differently.
const VIEW_CONTEXT_STANDALONE: u32 = 0;
const VIEW_CONTEXT_EMBEDDED: u32 = 1;

// ---------------------------------------------------------------------------
// Process + filesystem isolation
// ---------------------------------------------------------------------------

fn unique_test_dir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!(
        "/tmp/herdr-host-glass-e2e-{label}-{}-{nanos}",
        std::process::id()
    ))
}

struct SpawnedHerdr {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Drop for SpawnedHerdr {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();

        if let Some(pid) = pid {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let mut status = 0;
                let result =
                    unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
                if result == pid as libc::pid_t || result == -1 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }

            unregister_spawned_herdr_pid(Some(pid));
        }
    }
}

/// Kills the spawned server and removes the whole temp base, including the
/// registered runtime dir. Runs on the success path; the support module's panic
/// hook, atexit hook and watchdog cover the failure path.
fn cleanup_spawned_herdr(spawned: SpawnedHerdr, base: PathBuf) {
    drop(spawned);
    cleanup_test_base(&base);
}

fn test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_for_socket(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("socket did not accept connections at {}", path.display());
}

/// The config/state subdirectory the binary under test actually uses. A debug
/// build reads `herdr-dev`, so writing to `herdr/` would silently produce a
/// default-configured server.
fn app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        "herdr-dev"
    } else {
        "herdr"
    }
}

fn spawn_server(
    config_home: &Path,
    state_home: &Path,
    runtime_dir: &Path,
    api_socket_path: &Path,
    extra_config: &str,
) -> SpawnedHerdr {
    fs::create_dir_all(config_home.join(app_dir_name())).unwrap();
    fs::create_dir_all(state_home.join(app_dir_name())).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    register_runtime_dir(runtime_dir);
    fs::write(
        config_home.join(app_dir_name()).join("config.toml"),
        format!("onboarding = false\n{extra_config}"),
    )
    .unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    // Without this the server would fall back to the machine's real
    // `~/.local/state/herdr-dev` session/log state. Isolation here is a hard
    // requirement, not a convenience.
    cmd.env("XDG_STATE_HOME", state_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", api_socket_path);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("HERDR_DISABLE_SOUND", "1");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);

    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn server_log_path(config_home: &Path) -> PathBuf {
    config_home.join(app_dir_name()).join("herdr-server.log")
}

fn log_tail(path: &Path, lines: usize) -> String {
    let Ok(text) = fs::read_to_string(path) else {
        return format!("could not read {}", path.display());
    };
    let mut tail = VecDeque::with_capacity(lines);
    for line in text.lines() {
        if tail.len() == lines {
            tail.pop_front();
        }
        tail.push_back(line.to_string());
    }
    tail.into_iter().collect::<Vec<_>>().join("\n")
}

// ---------------------------------------------------------------------------
// Wire client with an explicit view context
// ---------------------------------------------------------------------------

fn encode_varint_enum(variant_idx: u32, fields: &[&[u8]]) -> Vec<u8> {
    let mut buf = encode_varint_u32(variant_idx);
    for field in fields {
        buf.extend_from_slice(field);
    }
    buf
}

fn decode_varint_u32(payload: &[u8], offset: usize) -> Result<(u32, usize), String> {
    support::decode_varint_u32(payload, offset)
}

/// Hello handshake with an explicit `view_context`. Field order mirrors
/// `ClientMessage::Hello`; only the final field differs from the standalone
/// handshake in `tests/support`.
fn client_handshake(
    stream: &mut UnixStream,
    version: u32,
    cols: u16,
    rows: u16,
    view_context: u32,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;

    let hello_payload = encode_varint_enum(
        0,
        &[
            &encode_varint_u32(version),
            &encode_varint_u16(cols),
            &encode_varint_u16(rows),
            &encode_varint_u32(0), // cell_width_px
            &encode_varint_u32(0), // cell_height_px
            &encode_varint_u32(0), // RenderEncoding::SemanticFrame
            &encode_varint_u32(0), // ClientKeybindings::Server
            &encode_varint_u32(0), // ClientLaunchMode::App
            &encode_varint_u32(view_context),
        ],
    );
    stream
        .write_all(&frame_message(&hello_payload))
        .map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).map_err(|e| e.to_string())?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > 2 * 1024 * 1024 {
        return Err(format!("oversized welcome: {len}"));
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).map_err(|e| e.to_string())?;

    let mut offset = 0;
    let (variant, consumed) = decode_varint_u32(&payload, offset)?;
    offset += consumed;
    if variant != 0 {
        return Err(format!("expected Welcome variant 0, got {variant}"));
    }
    let (_server_version, consumed) = decode_varint_u32(&payload, offset)?;
    offset += consumed;
    let (_encoding, consumed) = decode_varint_u32(&payload, offset)?;
    offset += consumed;
    if offset >= payload.len() {
        return Err("payload too short for Welcome.error option tag".into());
    }
    let option_tag = payload[offset];
    offset += 1;
    if option_tag == 1 {
        let (str_len, consumed) = decode_varint_u32(&payload, offset)?;
        offset += consumed;
        let str_len = str_len as usize;
        if offset + str_len > payload.len() {
            return Err("payload too short for welcome error string".into());
        }
        let err = String::from_utf8(payload[offset..offset + str_len].to_vec())
            .map_err(|e| e.to_string())?;
        return Err(format!("handshake rejected: {err}"));
    }

    Ok(())
}

fn connect_client(client_socket: &Path, cols: u16, rows: u16, view_context: u32) -> UnixStream {
    let mut stream = UnixStream::connect(client_socket).expect("should connect to client socket");
    client_handshake(&mut stream, CURRENT_PROTOCOL, cols, rows, view_context)
        .expect("handshake should succeed");
    stream
}

fn send_client_input(stream: &mut UnixStream, data: &[u8]) {
    // ClientMessage::Input = variant 1
    let mut payload = encode_varint_u32(1);
    payload.extend_from_slice(&encode_varint_u32(data.len() as u32));
    payload.extend_from_slice(data);
    stream.write_all(&frame_message(&payload)).unwrap();
    stream.flush().unwrap();
}

fn send_client_resize(stream: &mut UnixStream, cols: u16, rows: u16) {
    // ClientMessage::Resize = variant 3
    let mut payload = encode_varint_u32(3);
    payload.extend_from_slice(&encode_varint_u16(cols));
    payload.extend_from_slice(&encode_varint_u16(rows));
    payload.extend_from_slice(&encode_varint_u32(0));
    payload.extend_from_slice(&encode_varint_u32(0));
    stream.write_all(&frame_message(&payload)).unwrap();
    stream.flush().unwrap();
}

fn send_client_detach(stream: &mut UnixStream) {
    // ClientMessage::Detach = variant 4
    let payload = encode_varint_u32(4);
    stream.write_all(&frame_message(&payload)).unwrap();
    stream.flush().unwrap();
}

/// SGR mouse press/release for a 0-based screen cell. The server parses these
/// out of the raw input stream exactly as it does for a real terminal.
fn sgr_mouse_click(col0: u16, row0: u16) -> Vec<u8> {
    format!(
        "\x1b[<0;{};{}M\x1b[<0;{};{}m",
        col0 + 1,
        row0 + 1,
        col0 + 1,
        row0 + 1
    )
    .into_bytes()
}

// ---------------------------------------------------------------------------
// Frame decoding
// ---------------------------------------------------------------------------

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct FrameWire {
    cells: Vec<CellWire>,
    width: u16,
    height: u16,
    cursor: Option<CursorWire>,
    hyperlinks: Vec<String>,
    graphics: Vec<u8>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct CellWire {
    symbol: String,
    fg: u32,
    bg: u32,
    modifier: u16,
    skip: bool,
    hyperlink: Option<u32>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct CursorWire {
    x: u16,
    y: u16,
    visible: bool,
    shape: u8,
}

fn decode_frame_payload(payload: &[u8]) -> io::Result<FrameWire> {
    bincode::serde::decode_from_slice(payload, bincode::config::standard())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))
        .map(|(frame, _consumed): (FrameWire, usize)| frame)
}

fn frame_rows(frame: &FrameWire) -> Vec<String> {
    if frame.cells.is_empty() {
        return Vec::new();
    }
    let row_width = frame.width.max(1) as usize;
    frame
        .cells
        .chunks(row_width)
        .map(|row| {
            row.iter()
                .map(|cell| cell.symbol.as_str())
                .collect::<String>()
        })
        .collect()
}

fn frame_text(frame: &FrameWire) -> String {
    frame_rows(frame).join("\n")
}

fn is_timeout(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

fn read_server_message_payload(
    stream: &mut UnixStream,
    timeout: Duration,
) -> io::Result<(u32, Vec<u8>)> {
    stream.set_read_timeout(Some(timeout))?;

    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "zero-length payload",
        ));
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;

    let (variant, consumed) = decode_varint_u32(&payload, 0)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((variant, payload[consumed..].to_vec()))
}

/// Every frame this client sends us within `window`. Returns an empty vec when
/// the client is idle; callers that need a frame use [`wait_for_frame_matching`].
fn collect_frames(stream: &mut UnixStream, window: Duration) -> Vec<FrameWire> {
    let deadline = Instant::now() + window;
    let mut frames = Vec::new();
    while Instant::now() < deadline {
        let slice = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(80));
        match read_server_message_payload(stream, slice) {
            Ok((1, payload)) => match decode_frame_payload(&payload) {
                Ok(frame) => frames.push(frame),
                Err(err) => panic!("frame decode failed: {err}"),
            },
            Ok(_) => {}
            Err(err) if is_timeout(&err) => {}
            Err(_) => break,
        }
    }
    frames
}

fn wait_for_frame_matching(
    stream: &mut UnixStream,
    timeout: Duration,
    predicate: impl Fn(&FrameWire) -> bool,
) -> Option<FrameWire> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let slice = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(80));
        match read_server_message_payload(stream, slice) {
            Ok((1, payload)) => match decode_frame_payload(&payload) {
                Ok(frame) => {
                    if predicate(&frame) {
                        return Some(frame);
                    }
                }
                Err(err) => panic!("frame decode failed: {err}"),
            },
            Ok(_) => {}
            Err(err) if is_timeout(&err) => {}
            Err(_) => return None,
        }
    }
    None
}

fn wait_for_any_frame(stream: &mut UnixStream, timeout: Duration) -> Option<FrameWire> {
    wait_for_frame_matching(stream, timeout, |_| true)
}

// ---------------------------------------------------------------------------
// JSON API helpers (used only to build local state and to read real PTY sizes)
// ---------------------------------------------------------------------------

fn send_json_request(socket_path: &Path, request: &str) -> Value {
    let mut stream = UnixStream::connect(socket_path).expect("should connect to API socket");
    writeln!(stream, "{request}").unwrap();

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();

    serde_json::from_str(&response).expect("response should be valid JSON")
}

fn create_workspace_and_root_pane(socket_path: &Path, label: &str) -> String {
    let response = send_json_request(
        socket_path,
        &format!(
            "{{\"id\":\"ws_create\",\"method\":\"workspace.create\",\"params\":{{\"label\":\"{label}\"}}}}"
        ),
    );
    if response.get("error").is_some() {
        panic!("workspace.create failed: {response}");
    }

    response
        .pointer("/result/root_pane/pane_id")
        .and_then(Value::as_str)
        .expect("workspace.create should return root pane id")
        .to_string()
}

fn pane_send_input(socket_path: &Path, pane_id: &str, text: &str) {
    let request = format!(
        "{{\"id\":\"send_input\",\"method\":\"pane.send_input\",\"params\":{{\"pane_id\":\"{pane_id}\",\"text\":\"{}\",\"keys\":[\"Enter\"]}}}}",
        text.replace('"', "\\\"")
    );
    let response = send_json_request(socket_path, &request);
    if response.get("error").is_some() {
        panic!("pane.send_input failed: {response}");
    }
}

fn pane_read_recent(socket_path: &Path, pane_id: &str, lines: usize) -> String {
    let response = send_json_request(
        socket_path,
        &format!(
            "{{\"id\":\"pane_read\",\"method\":\"pane.read\",\"params\":{{\"pane_id\":\"{pane_id}\",\"source\":\"recent\",\"lines\":{lines}}}}}"
        ),
    );
    if response.get("error").is_some() {
        panic!("pane.read failed: {response}");
    }
    response
        .pointer("/result/read/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn pane_read_recent_contains(
    socket_path: &Path,
    pane_id: &str,
    needle: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if pane_read_recent(socket_path, pane_id, 200).contains(needle) {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn parse_size_after_marker(text: &str, marker: &str) -> Option<(u16, u16)> {
    let mut seen_marker = false;
    for line in text.lines() {
        if !seen_marker {
            if line.contains(marker) {
                seen_marker = true;
            }
            continue;
        }
        let mut parts = line.split_whitespace();
        let rows = parts.next()?.parse::<u16>().ok();
        let cols = parts.next().and_then(|raw| raw.parse::<u16>().ok());
        if let (Some(rows), Some(cols)) = (rows, cols) {
            return Some((rows, cols));
        }
    }
    None
}

/// The pane's real PTY window size, read out of the pane by running
/// `stty size` in it. This is the shared display size the server derives from
/// whichever client owns the display, so it is a direct observation of display
/// ownership rather than a proxy.
fn read_pane_tty_size(socket_path: &Path, pane_id: &str, timeout: Duration) -> (u16, u16) {
    let marker = format!(
        "SIZE_MARKER_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    pane_send_input(socket_path, pane_id, &format!("echo {marker}; stty size"));

    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let text = pane_read_recent(socket_path, pane_id, 200);
        if let Some(size) = parse_size_after_marker(&text, &marker) {
            return size;
        }
        thread::sleep(Duration::from_millis(50));
    }

    panic!(
        "did not observe tty size after marker. pane output:\n{}",
        pane_read_recent(socket_path, pane_id, 200)
    );
}

fn wait_for_pane_tty_size_change(
    socket_path: &Path,
    pane_id: &str,
    baseline: (u16, u16),
    timeout: Duration,
) -> Option<(u16, u16)> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let size = read_pane_tty_size(socket_path, pane_id, Duration::from_secs(5));
        if size != baseline {
            return Some(size);
        }
        thread::sleep(Duration::from_millis(80));
    }
    None
}

// ---------------------------------------------------------------------------
// E1 - display ownership
// ---------------------------------------------------------------------------

/// An embedded (host glass) client must never take the shared display size away
/// from a standalone client sitting on this host.
///
/// The observable is the pane's real PTY window size (`stty size` inside the
/// pane), which the server sizes from `effective_size`, which comes from
/// whichever client owns the display. Before the fix, connecting, clicking in,
/// or resizing an embedded client promoted it to foreground and resized every
/// pane on this host to the viewer's geometry, then the human's terminal took
/// it back on its next interaction - the size flap this test would catch.
///
/// The final step is a deliberate control: once the standalone client detaches,
/// the embedded client legitimately becomes the display owner as the fallback,
/// and the PTY size DOES move. That proves the probe is sensitive enough to have
/// detected the pre-fix flap, so the stability asserted above is a real result
/// rather than a blind observable.
#[test]
fn embedded_client_does_not_steal_display_size_from_standalone() {
    let _lock = test_lock();
    let base = unique_test_dir("display-ownership");
    let config_home = base.join("config");
    let state_home = base.join("state");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &state_home, &runtime_dir, &api_socket, "");
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let pane_id = create_workspace_and_root_pane(&api_socket, "glass-ownership");

    // The human's terminal on this host.
    let mut standalone = connect_client(&client_socket, 160, 45, VIEW_CONTEXT_STANDALONE);
    let first_standalone_frame = wait_for_any_frame(&mut standalone, Duration::from_secs(5))
        .expect("standalone client should receive frames");
    assert_eq!(
        (first_standalone_frame.width, first_standalone_frame.height),
        (160, 45),
        "the standalone client renders at its own size"
    );

    let baseline = read_pane_tty_size(&api_socket, &pane_id, Duration::from_secs(10));
    assert!(
        baseline.1 > 100,
        "the 160-column owner must give the pane more columns than a 100-column \
         embedded viewer ever could, or the two candidate sizes are not \
         distinguishable: {baseline:?}"
    );

    // A second Herdr streams this host through its own glass.
    let mut embedded = connect_client(&client_socket, 100, 30, VIEW_CONTEXT_EMBEDDED);
    let first_embedded_frame = wait_for_any_frame(&mut embedded, Duration::from_secs(5))
        .expect("embedded client should receive frames");
    assert_eq!(
        (first_embedded_frame.width, first_embedded_frame.height),
        (100, 30),
        "the embedded client renders at its own size without owning the display"
    );

    assert_eq!(
        read_pane_tty_size(&api_socket, &pane_id, Duration::from_secs(10)),
        baseline,
        "an embedded client attaching must not resize this host's panes. server log tail:\n{}",
        log_tail(&server_log_path(&config_home), 60)
    );

    // Interaction from the embedded client. A mouse click is used rather than a
    // key so nothing is left in the shell's line buffer for the next `stty size`
    // probe; it is still a full interaction event on the server's input path,
    // which is what used to promote the client to foreground.
    send_client_input(&mut embedded, &sgr_mouse_click(50, 15));
    assert_eq!(
        read_pane_tty_size(&api_socket, &pane_id, Duration::from_secs(10)),
        baseline,
        "an embedded client's interaction must not resize this host's panes"
    );

    // An explicit resize from the embedded client.
    send_client_resize(&mut embedded, 90, 28);
    let resized_embedded_frame =
        wait_for_frame_matching(&mut embedded, Duration::from_secs(5), |frame| {
            (frame.width, frame.height) == (90, 28)
        });
    assert!(
        resized_embedded_frame.is_some(),
        "the embedded client's own render should follow its resize"
    );
    assert_eq!(
        read_pane_tty_size(&api_socket, &pane_id, Duration::from_secs(10)),
        baseline,
        "an embedded client's resize must not resize this host's panes"
    );

    // Throughout all of that, the standalone client's own geometry never moved.
    let standalone_frames = collect_frames(&mut standalone, Duration::from_millis(600));
    assert!(
        !standalone_frames.is_empty(),
        "the standalone client should still be receiving frames"
    );
    for frame in &standalone_frames {
        assert_eq!(
            (frame.width, frame.height),
            (160, 45),
            "the standalone client's frames must keep their geometry"
        );
    }

    // Control: with no standalone client attached the embedded viewer becomes
    // the display owner by design, and the PTY size must actually move. This is
    // what makes the assertions above meaningful.
    send_client_detach(&mut standalone);
    drop(standalone);

    let after_fallback =
        wait_for_pane_tty_size_change(&api_socket, &pane_id, baseline, Duration::from_secs(10));
    assert!(
        after_fallback.is_some(),
        "with the standalone client gone the embedded viewer must own the display \
         (otherwise glass would render an unattended host at 80x24), and the pane \
         PTY size must move off {baseline:?}. server log tail:\n{}",
        log_tail(&server_log_path(&config_home), 60)
    );

    cleanup_spawned_herdr(server, base);
}

// ---------------------------------------------------------------------------
// E2 - glass does not nest
// ---------------------------------------------------------------------------

/// Glass does not nest: while this host has a remote selected, a standalone
/// client here sees the glass surface for that remote, and an embedded client
/// streaming this host sees this host's OWN local pane content.
///
/// The remote host is declared with `connection_policy = "manual"`, so it shows
/// up in the host rail and is selectable but is never probed or dialled - no SSH
/// process, no second machine. The glass sits in `Connecting`, which is all the
/// precondition the bug needed: `sidebar_source == Remote(..)`.
///
/// Host selection is driven the way a human drives it, by clicking the host's
/// row in the rail over the wire; the socket API has no method that sets the
/// sidebar source (`src/api/schema/remotes.rs` exposes only
/// connect/reconnect/disconnect lifecycle, which is aggregation state, not
/// selection).
#[test]
fn embedded_client_sees_local_content_when_host_has_remote_selected() {
    let _lock = test_lock();
    let base = unique_test_dir("no-nesting");
    let config_home = base.join("config");
    let state_home = base.join("state");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    // `manual` keeps the host visible and selectable while guaranteeing the
    // supervisor never probes it, so this test dials nothing.
    let host_alias = "ghost1";
    let remote_config = format!(
        "\n[remote]\nenabled = true\n\n[[remote.hosts]]\nname = \"{host_alias}\"\n\
         target = \"{host_alias}.invalid\"\nsession = \"default\"\n\
         connection_policy = \"manual\"\n"
    );

    let server = spawn_server(
        &config_home,
        &state_home,
        &runtime_dir,
        &api_socket,
        &remote_config,
    );
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let pane_id = create_workspace_and_root_pane(&api_socket, "glass-nesting");
    let marker = format!(
        "LOCALPANE{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    pane_send_input(&api_socket, &pane_id, &format!("echo {marker}"));
    assert!(
        pane_read_recent_contains(&api_socket, &pane_id, &marker, Duration::from_secs(10)),
        "the local pane should hold recognisable content"
    );

    let mut standalone = connect_client(&client_socket, 120, 40, VIEW_CONTEXT_STANDALONE);
    let local_frame = wait_for_frame_matching(&mut standalone, Duration::from_secs(10), |frame| {
        frame_text(frame).contains(&marker)
    })
    .unwrap_or_else(|| {
        panic!(
            "the standalone client should show local pane content before a host is selected. \
             server log tail:\n{}",
            log_tail(&server_log_path(&config_home), 60)
        )
    });

    // Locate the configured host's row inside the 10-column host rail and click
    // it, exactly as a human would.
    let rail_width = 10usize;
    let host_row = frame_rows(&local_frame)
        .into_iter()
        .enumerate()
        .find(|(_, row)| {
            row.chars()
                .take(rail_width)
                .collect::<String>()
                .contains(host_alias)
        })
        .map(|(idx, _)| idx as u16)
        .unwrap_or_else(|| {
            panic!(
                "configured host {host_alias:?} should appear in the host rail:\n{}",
                frame_text(&local_frame)
            )
        });
    send_client_input(&mut standalone, &sgr_mouse_click(2, host_row));

    // The human's terminal on this host now sees the glass for that remote.
    let glass_frame = wait_for_frame_matching(&mut standalone, Duration::from_secs(10), |frame| {
        let text = frame_text(frame);
        text.contains(" glass ") && text.contains(host_alias)
    })
    .unwrap_or_else(|| {
        panic!(
            "the standalone client should present the host glass after selecting {host_alias:?}. \
             server log tail:\n{}",
            log_tail(&server_log_path(&config_home), 60)
        )
    });
    let glass_text = frame_text(&glass_frame);
    assert!(
        !glass_text.contains(&marker),
        "the glass surface replaces this host's local panes for a standalone viewer:\n{glass_text}"
    );

    // A second Herdr now streams this host through its own glass. It must see
    // THIS host's local workspace, not a nested mirror of the remote this host
    // has selected.
    let mut embedded = connect_client(&client_socket, 120, 40, VIEW_CONTEXT_EMBEDDED);
    let embedded_frame = wait_for_frame_matching(&mut embedded, Duration::from_secs(10), |frame| {
        frame_text(frame).contains(&marker)
    })
    .unwrap_or_else(|| {
        panic!(
            "the embedded viewer must see this host's own local pane content while this \
                 host has {host_alias:?} selected. server log tail:\n{}",
            log_tail(&server_log_path(&config_home), 60)
        )
    });
    let embedded_text = frame_text(&embedded_frame);
    assert!(
        !embedded_text.contains(" glass "),
        "an embedded viewer must never be shown a nested glass:\n{embedded_text}"
    );

    // The two contexts diverge at the same instant: the human's terminal is
    // still on the glass while the embedded viewer is on local panes, and this
    // host's own selection was never disturbed by the embedded attach.
    let still_glass = wait_for_frame_matching(&mut standalone, Duration::from_secs(10), |frame| {
        let text = frame_text(frame);
        text.contains(" glass ") && text.contains(host_alias) && !text.contains(&marker)
    });
    assert!(
        still_glass.is_some(),
        "the standalone client must keep the glass while an embedded viewer is attached. \
         server log tail:\n{}",
        log_tail(&server_log_path(&config_home), 60)
    );

    cleanup_spawned_herdr(server, base);
}

/// Pins the `ViewContext` discriminants this file hand-encodes. A standalone
/// and an embedded Hello are both accepted, and only the standalone one takes
/// the display - which is only true if 0 and 1 map to `Standalone` and
/// `Embedded` as `src/protocol/wire.rs` declares them.
#[test]
fn view_context_discriminants_match_the_wire_enum() {
    let _lock = test_lock();
    let base = unique_test_dir("view-context");
    let config_home = base.join("config");
    let state_home = base.join("state");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let server = spawn_server(&config_home, &state_home, &runtime_dir, &api_socket, "");
    wait_for_socket(&api_socket, Duration::from_secs(10));
    wait_for_socket(&client_socket, Duration::from_secs(10));

    let pane_id = create_workspace_and_root_pane(&api_socket, "glass-view-context");

    // Two clients that differ only in the final Hello field. Connecting order
    // is embedded-then-standalone so that "latest attach wins" would give the
    // display to the standalone client either way; the discriminating fact is
    // that the earlier embedded client does not get it back.
    let mut embedded = connect_client(&client_socket, 100, 30, VIEW_CONTEXT_EMBEDDED);
    assert!(
        wait_for_any_frame(&mut embedded, Duration::from_secs(5)).is_some(),
        "an embedded Hello must be accepted as a full-app client"
    );
    let embedded_only = read_pane_tty_size(&api_socket, &pane_id, Duration::from_secs(10));

    let mut standalone = connect_client(&client_socket, 160, 45, VIEW_CONTEXT_STANDALONE);
    assert!(
        wait_for_any_frame(&mut standalone, Duration::from_secs(5)).is_some(),
        "a standalone Hello must be accepted as a full-app client"
    );
    let with_standalone = wait_for_pane_tty_size_change(
        &api_socket,
        &pane_id,
        embedded_only,
        Duration::from_secs(10),
    )
    .unwrap_or_else(|| {
        panic!(
            "a standalone client attaching must take the display from a lone embedded \
             owner (was {embedded_only:?})"
        )
    });
    assert!(
        with_standalone.1 > embedded_only.1,
        "the standalone owner is wider than the embedded one: {with_standalone:?} vs \
         {embedded_only:?}"
    );

    // The embedded client interacting must not win it back.
    send_client_input(&mut embedded, &sgr_mouse_click(50, 15));
    assert_eq!(
        read_pane_tty_size(&api_socket, &pane_id, Duration::from_secs(10)),
        with_standalone,
        "field value 1 must decode as ViewContext::Embedded, which cannot take the \
         display while a standalone client is attached"
    );

    cleanup_spawned_herdr(server, base);
}
