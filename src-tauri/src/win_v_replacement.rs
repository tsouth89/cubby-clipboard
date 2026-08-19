use std::net::UdpSocket;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;
use tauri::AppHandle;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Default)]
struct HelperState {
    child: Option<Child>,
    desired: bool,
    /// The Cubby hotkey handed to the running helper. When this changes the
    /// helper is restarted so it re-parses the new activation chord.
    hotkey: Option<String>,
}

struct Inner {
    state: Mutex<HelperState>,
    watchdog_started: AtomicBool,
    activation_port: u16,
    activation_token: String,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(state) = self.state.get_mut() {
            stop_child(&mut state.child);
        }
    }
}

pub struct WinVReplacementManager {
    inner: Arc<Inner>,
}

impl WinVReplacementManager {
    pub fn new(app: AppHandle) -> Result<Self, String> {
        // Fail closed if we cannot mint a token: an unauthenticated bind is
        // the SBS-809 bug. Do not bind first and "add a token later".
        let activation_token = crate::win_v_activation::generate_token()?;
        let socket = UdpSocket::bind(("127.0.0.1", 0))
            .map_err(|error| format!("Could not create the Cubby shortcut channel: {error}"))?;
        let activation_port = socket
            .local_addr()
            .map_err(|error| format!("Could not inspect the Cubby shortcut channel: {error}"))?
            .port();
        let listener_token = activation_token.clone();
        std::thread::spawn(move || {
            let mut buffer = [0_u8; crate::win_v_activation::RECV_BUFFER_LEN];
            let mut rate = crate::win_v_activation::ActivationRateLimit::default();
            loop {
                match socket.recv_from(&mut buffer) {
                    Ok((length, source)) => {
                        match crate::win_v_activation::decide_activation(
                            &buffer[..length],
                            source,
                            &listener_token,
                            &mut rate,
                            std::time::Instant::now(),
                        ) {
                            crate::win_v_activation::ActivationDecision::Accept => {
                                log::debug!("WIN_V: Received authorized shortcut activation");
                                crate::shortcuts::toggle_main_window(&app);
                            }
                            crate::win_v_activation::ActivationDecision::RejectUnauthenticated => {
                                log::debug!("WIN_V: Ignored unauthenticated shortcut activation");
                            }
                            crate::win_v_activation::ActivationDecision::RejectOrigin => {
                                log::debug!(
                                    "WIN_V: Ignored shortcut activation from outside loopback"
                                );
                            }
                            crate::win_v_activation::ActivationDecision::RejectRateLimited => {
                                log::debug!("WIN_V: Ignored shortcut activation (rate limited)");
                            }
                        }
                    }
                    Err(error) => {
                        if !crate::win_v_activation::activation_recv_error_is_fatal(&error) {
                            log::debug!(
                                "WIN_V: Ignored oversized or transient shortcut datagram: {error}"
                            );
                            continue;
                        }
                        log::error!("WIN_V: Shortcut activation listener failed: {error}");
                        return;
                    }
                }
            }
        });

        Ok(Self {
            inner: Arc::new(Inner {
                state: Mutex::new(HelperState::default()),
                watchdog_started: AtomicBool::new(false),
                activation_port,
                activation_token,
            }),
        })
    }

    pub fn configure(&self, enabled: bool, hotkey: Option<String>) -> Result<(), String> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| "Win+V helper state is unavailable".to_string())?;
        let hotkey_changed = state.hotkey != hotkey;
        state.desired = enabled;
        state.hotkey = hotkey;

        if !enabled {
            stop_child(&mut state.child);
            log::info!("WIN_V: Replacement helper stopped");
            return Ok(());
        }

        // Restart a running helper so it re-parses the new activation hotkey.
        if hotkey_changed {
            stop_child(&mut state.child);
        }

        ensure_child_running(
            &mut state,
            self.inner.activation_port,
            &self.inner.activation_token,
        )?;
        drop(state);
        self.start_watchdog();
        Ok(())
    }

    fn start_watchdog(&self) {
        if self.inner.watchdog_started.swap(true, Ordering::SeqCst) {
            return;
        }

        let weak = Arc::downgrade(&self.inner);
        std::thread::spawn(move || watchdog_loop(weak));
    }
}

fn watchdog_loop(inner: Weak<Inner>) {
    loop {
        std::thread::sleep(Duration::from_secs(2));
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let Ok(mut state) = inner.state.lock() else {
            log::error!("WIN_V: Watchdog could not lock helper state");
            continue;
        };
        if !state.desired {
            continue;
        }

        let exited = match state.child.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(Some(status)) => {
                    log::warn!("WIN_V: Helper exited unexpectedly with {status}");
                    true
                }
                Ok(None) => false,
                Err(error) => {
                    log::error!("WIN_V: Could not inspect helper: {error}");
                    true
                }
            },
            None => true,
        };

        if exited {
            state.child = None;
            if let Err(error) =
                ensure_child_running(&mut state, inner.activation_port, &inner.activation_token)
            {
                log::error!("WIN_V: Helper restart failed: {error}");
            }
        }
    }
}

fn ensure_child_running(
    state: &mut HelperState,
    activation_port: u16,
    activation_token: &str,
) -> Result<(), String> {
    if let Some(child) = state.child.as_mut() {
        match child.try_wait() {
            Ok(None) => return Ok(()),
            Ok(Some(_)) | Err(_) => state.child = None,
        }
    }

    let executable =
        std::env::current_exe().map_err(|error| format!("Could not locate Cubby: {error}"))?;
    let mut command = Command::new(executable);
    command
        .arg("--win-v-helper")
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .arg("--activation-port")
        .arg(activation_port.to_string())
        .arg("--activation-token")
        .arg(activation_token)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    if let Some(hotkey) = state.hotkey.as_deref() {
        command.arg("--activation-hotkey").arg(hotkey);
    }

    #[cfg(target_os = "windows")]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start the Win+V helper: {error}"))?;

    std::thread::sleep(Duration::from_millis(100));
    if let Some(status) = child
        .try_wait()
        .map_err(|error| format!("Could not verify the Win+V helper: {error}"))?
    {
        return Err(format!("Win+V helper exited during startup with {status}"));
    }

    // Do not log the port or the token. The port was previously printed at
    // Info and made the unauthenticated channel easier to find (SBS-809).
    log::info!("WIN_V: Replacement helper started (pid {})", child.id());
    state.child = Some(child);
    Ok(())
}

fn stop_child(child: &mut Option<Child>) {
    if let Some(mut running) = child.take() {
        let _ = running.kill();
        let _ = running.wait();
    }
}
