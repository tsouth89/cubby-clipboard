use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Held for the whole show or hide so two animations cannot run at once.
#[allow(dead_code)] // used from lib.rs; rustc --test compiles this file alone
pub(crate) static IS_ANIMATING: AtomicBool = AtomicBool::new(false);

#[allow(dead_code)]
pub(crate) const ANIMATION_LOCK_WAIT: Duration = Duration::from_millis(1_000);

/// Blur events this soon after a show are the focus handoff, not a real dismiss.
pub(crate) const BLUR_DEBOUNCE_MS: i64 = 500;

/// Releases the show/hide lock even if a worker unwinds. A detached window
/// thread panic must not leave Cubby permanently unable to open again.
#[allow(dead_code)]
pub(crate) struct AnimationGuard;

#[allow(dead_code)] // used from lib.rs; rustc --test compiles this file alone
impl AnimationGuard {
    pub(crate) fn acquire() -> Option<Self> {
        try_lock_animation(&IS_ANIMATING).then_some(Self)
    }

    /// Wait briefly for an in-flight show/hide instead of dropping the request.
    /// A missed toggle leaves the flyout in the opposite state from the keypress.
    pub(crate) fn acquire_within(timeout: Duration) -> Option<Self> {
        lock_animation_within(&IS_ANIMATING, timeout).then_some(Self)
    }
}

impl Drop for AnimationGuard {
    fn drop(&mut self) {
        unlock_animation(&IS_ANIMATING);
    }
}

pub(crate) fn try_lock_animation(flag: &AtomicBool) -> bool {
    flag.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

pub(crate) fn unlock_animation(flag: &AtomicBool) {
    flag.store(false, Ordering::SeqCst);
}

pub(crate) fn lock_animation_within(flag: &AtomicBool, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if try_lock_animation(flag) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) fn should_ignore_blur(now_ms: i64, last_show_ms: i64) -> bool {
    now_ms - last_show_ms < BLUR_DEBOUNCE_MS
}

/// Rising edge only. Seeding `buttons_were_down` from the current buttons
/// means a drag that opened the flyout is not a press on the first poll.
pub(crate) fn is_new_mouse_press(buttons_down: bool, buttons_were_down: bool) -> bool {
    buttons_down && !buttons_were_down
}

/// True when the top-level window under the cursor is one of our visible
/// owned HWNDs. Rect overlap is not enough: History/image can sit behind
/// another app and still cover most of the monitor.
pub(crate) fn click_hits_owned_hwnd(
    top_hwnd: isize,
    top_visible: bool,
    owned_visible: &[isize],
) -> bool {
    top_visible && owned_visible.contains(&top_hwnd)
}

pub(crate) fn outside_click_watcher_should_exit(
    current_generation: u64,
    watcher_generation: u64,
    window_visible: bool,
) -> bool {
    current_generation != watcher_generation || !window_visible
}

#[cfg(test)]
mod tests {
    use super::{
        click_hits_owned_hwnd, is_new_mouse_press, lock_animation_within,
        outside_click_watcher_should_exit, should_ignore_blur, try_lock_animation,
        unlock_animation, BLUR_DEBOUNCE_MS,
    };
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn acquire_fails_while_the_animation_lock_is_held() {
        let flag = AtomicBool::new(false);
        assert!(try_lock_animation(&flag));
        assert!(
            !try_lock_animation(&flag),
            "a second acquire must fail so two animations cannot overlap"
        );
    }

    #[test]
    fn dropping_the_animation_lock_lets_the_next_toggle_acquire() {
        let flag = AtomicBool::new(false);
        assert!(try_lock_animation(&flag));
        assert!(
            !try_lock_animation(&flag),
            "the toggle is dropped if the lock is never released"
        );
        unlock_animation(&flag);
        assert!(
            try_lock_animation(&flag),
            "drop must reset the lock or the next Win+V is lost"
        );
    }

    #[test]
    fn acquire_within_times_out_while_the_lock_is_held() {
        let flag = AtomicBool::new(true);
        let started = std::time::Instant::now();
        assert!(!lock_animation_within(&flag, Duration::from_millis(40)));
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(40),
            "timeout must wait the full window, got {:?}",
            elapsed
        );
        assert!(
            elapsed < Duration::from_millis(200),
            "timeout must not hang, got {:?}",
            elapsed
        );
    }

    #[test]
    fn acquire_within_succeeds_after_the_holder_drops() {
        let flag = Arc::new(AtomicBool::new(true));
        let released = flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            unlock_animation(&released);
        });
        assert!(
            lock_animation_within(&flag, Duration::from_millis(300)),
            "waiting for the lock must succeed once the in-flight show/hide drops it"
        );
        assert!(flag.load(Ordering::SeqCst));
    }

    #[test]
    fn blur_within_500ms_of_show_is_ignored() {
        let shown_at = 10_000;
        assert!(should_ignore_blur(shown_at, shown_at));
        assert!(should_ignore_blur(
            shown_at + BLUR_DEBOUNCE_MS - 1,
            shown_at
        ));
        assert!(!should_ignore_blur(shown_at + BLUR_DEBOUNCE_MS, shown_at));
        assert!(!should_ignore_blur(shown_at + 1_000, shown_at));
    }

    #[test]
    fn first_poll_with_a_button_already_down_is_not_a_press() {
        let buttons_were_down = true;
        let buttons_down = true;
        assert!(
            !is_new_mouse_press(buttons_down, buttons_were_down),
            "seeding from the current buttons must not look like a rising edge"
        );
    }

    #[test]
    fn a_rising_edge_after_release_is_a_new_press() {
        assert!(!is_new_mouse_press(false, true));
        assert!(!is_new_mouse_press(false, false));
        assert!(is_new_mouse_press(true, false));
    }

    #[test]
    fn a_click_on_settings_or_history_is_inside_cubby() {
        let main = 10;
        let settings = 20;
        let history = 30;
        let owned = [main, settings, history];
        assert!(click_hits_owned_hwnd(settings, true, &owned));
        assert!(click_hits_owned_hwnd(history, true, &owned));
        assert!(click_hits_owned_hwnd(main, true, &owned));
    }

    #[test]
    fn a_click_on_another_app_is_outside_even_if_it_overlaps_an_owned_rect() {
        let owned = [10, 20, 30];
        assert!(
            !click_hits_owned_hwnd(99, true, &owned),
            "another app must hide even when it sits over Settings or History"
        );
        assert!(!click_hits_owned_hwnd(20, true, &[]));
        assert!(!click_hits_owned_hwnd(20, false, &[20]));
    }

    #[test]
    fn a_newer_show_invalidates_the_outside_click_watcher() {
        assert!(outside_click_watcher_should_exit(2, 1, true));
        assert!(outside_click_watcher_should_exit(1, 1, false));
        assert!(!outside_click_watcher_should_exit(1, 1, true));
    }
}
