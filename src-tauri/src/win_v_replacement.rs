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
}

struct Inner {
    state: Mutex<HelperState>,
    watchdog_started: AtomicBool,
    activation_port: u16,
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
        let socket = UdpSocket::bind(("127.0.0.1", 0))
            .map_err(|error| format!("Could not create the Cubby shortcut channel: {error}"))?;
        let activation_port = socket
            .local_addr()
            .map_err(|error| format!("Could not inspect the Cubby shortcut channel: {error}"))?
            .port();
        std::thread::spawn(move || {
            let mut buffer = [0_u8; 32];
            loop {
                match socket.recv_from(&mut buffer) {
                    Ok((length, _)) if &buffer[..length] == b"activate" => {
                        log::debug!("WIN_V: Received direct shortcut activation");
                        crate::shortcuts::toggle_main_window(&app);
                    }
                    Ok(_) => {}
                    Err(error) => {
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
            }),
        })
    }

    pub fn configure(&self, enabled: bool) -> Result<(), String> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| "Win+V helper state is unavailable".to_string())?;
        state.desired = enabled;

        if !enabled {
            stop_child(&mut state.child);
            log::info!("WIN_V: Replacement helper stopped");
            return Ok(());
        }

        ensure_child_running(&mut state, self.inner.activation_port)?;
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
            if let Err(error) = ensure_child_running(&mut state, inner.activation_port) {
                log::error!("WIN_V: Helper restart failed: {error}");
            }
        }
    }
}

fn ensure_child_running(state: &mut HelperState, activation_port: u16) -> Result<(), String> {
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
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

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

    log::info!(
        "WIN_V: Replacement helper started (pid {}, direct activation port {})",
        child.id(),
        activation_port
    );
    state.child = Some(child);
    Ok(())
}

fn stop_child(child: &mut Option<Child>) {
    if let Some(mut running) = child.take() {
        let _ = running.kill();
        let _ = running.wait();
    }
}
