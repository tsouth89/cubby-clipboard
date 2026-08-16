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
pub(crate) struct AnimationGuard {
    /// The flag this guard released on drop. Always `&IS_ANIMATING` in the app;
    /// tests point it at their own static so they can exercise `Drop`.
    flag: &'static AtomicBool,
}

#[allow(dead_code)] // used from lib.rs; rustc --test compiles this file alone
impl AnimationGuard {
    pub(crate) fn acquire() -> Option<Self> {
        Self::acquire_on(&IS_ANIMATING)
    }

    /// Wait briefly for an in-flight show/hide instead of dropping the request.
    /// A missed toggle leaves the flyout in the opposite state from the keypress.
    pub(crate) fn acquire_within(timeout: Duration) -> Option<Self> {
        Self::acquire_within_on(&IS_ANIMATING, timeout)
    }

    /// `then`, never `then_some`: `then_some` builds the guard before it checks
    /// the bool, so a failed acquire would drop a guard and clear a lock that
    /// another show/hide still holds.
    fn acquire_on(flag: &'static AtomicBool) -> Option<Self> {
        try_lock_animation(flag).then(|| Self { flag })
    }

    fn acquire_within_on(flag: &'static AtomicBool, timeout: Duration) -> Option<Self> {
        lock_animation_within(flag, timeout).then(|| Self { flag })
    }
}

impl Drop for AnimationGuard {
    fn drop(&mut self) {
        unlock_animation(self.flag);
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
        unlock_animation, AnimationGuard, BLUR_DEBOUNCE_MS,
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
    fn dropping_the_animation_guard_lets_the_next_toggle_acquire() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        {
            let _guard = AnimationGuard::acquire_on(&FLAG).expect("the lock starts free");
            assert!(
                AnimationGuard::acquire_on(&FLAG).is_none(),
                "the toggle is dropped if a second animation can start"
            );
            // `is_none()` alone would not catch `then_some`: it builds the
            // guard before it reads the bool, so a *failed* acquire would
            // construct one and immediately drop it, clearing the flag out
            // from under the show/hide that still holds it. Win+V takes this
            // path (`acquire`) before it ever reaches `acquire_within`.
            assert!(
                FLAG.load(Ordering::SeqCst),
                "a failed acquire must leave the in-flight animation holding the lock"
            );
        }
        assert!(
            !FLAG.load(Ordering::SeqCst),
            "drop must reset the lock or the next Win+V is lost"
        );
        let next = AnimationGuard::acquire_on(&FLAG)
            .expect("drop must reset the lock or the next Win+V is lost");
        drop(next);
        assert!(!FLAG.load(Ordering::SeqCst));
    }

    /// Guards the deadlock in AGENTS.md: a panic between "lock" and "unlock"
    /// leaves the flyout permanently unable to open. Only `Drop` saves it, so
    /// this test must fail if the `Drop` body is ever emptied.
    #[test]
    fn a_panicking_worker_still_releases_the_animation_lock() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        let panicked = std::panic::catch_unwind(|| {
            let _guard = AnimationGuard::acquire_on(&FLAG).expect("the lock starts free");
            assert!(FLAG.load(Ordering::SeqCst), "the guard must hold the lock");
            panic!("a show/hide worker unwound");
        });
        assert!(panicked.is_err(), "the worker must really have unwound");
        assert!(
            !FLAG.load(Ordering::SeqCst),
            "an unwinding worker must release the lock, not wedge the flyout shut"
        );
        assert!(
            AnimationGuard::acquire_on(&FLAG).is_some(),
            "the next Win+V must still be able to open the flyout after a panic"
        );
    }

    /// No wall-clock upper bound: a loaded runner can stall the wait loop for
    /// far longer than the timeout, and that is not a product bug.
    #[test]
    fn acquire_within_gives_up_while_the_lock_is_held() {
        static FLAG: AtomicBool = AtomicBool::new(true);
        let started = std::time::Instant::now();
        assert!(
            AnimationGuard::acquire_within_on(&FLAG, Duration::from_millis(40)).is_none(),
            "a held lock must never hand out a second animation"
        );
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(40),
            "the wait must not give up early, got {:?}",
            elapsed
        );
        assert!(
            FLAG.load(Ordering::SeqCst),
            "a failed wait must leave the in-flight show/hide holding the lock"
        );
    }

    /// Goes through `acquire_within_on`, not `lock_animation_within`. Testing
    /// the raw helper leaves the guard-returning wrapper uncovered on its
    /// success path: a body that waited, took the lock, and still returned
    /// `None` would pass every other test here, and `kick_flyout_worker` reads
    /// that `None` as a dropped request while `IS_ANIMATING` stays true — so
    /// every later Win+V waits a second and gives up until restart.
    #[test]
    fn acquire_within_succeeds_after_the_holder_drops() {
        static FLAG: AtomicBool = AtomicBool::new(true);
        let unlocker = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(25));
            unlock_animation(&FLAG);
        });
        // Seconds, not milliseconds: the assertion is "the wait succeeds once
        // the holder drops", not "the runner scheduled the unlocker on time".
        let guard = AnimationGuard::acquire_within_on(&FLAG, Duration::from_secs(30))
            .expect("waiting for the lock must succeed once the in-flight show/hide drops it");
        assert!(
            FLAG.load(Ordering::SeqCst),
            "a successful wait must hold the lock, not merely report success"
        );
        drop(guard);
        assert!(
            !FLAG.load(Ordering::SeqCst),
            "the waited-for guard must release the lock like any other"
        );
        unlocker.join().expect("the unlocker thread must not panic");
    }

    /// The raw helper on its own, kept because `acquire_within_on` is a thin
    /// wrapper over it and a bug in either one is worth naming separately.
    #[test]
    fn lock_animation_within_succeeds_after_the_holder_drops() {
        let flag = Arc::new(AtomicBool::new(true));
        let released = flag.clone();
        let unlocker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            unlock_animation(&released);
        });
        assert!(
            lock_animation_within(&flag, Duration::from_secs(30)),
            "waiting for the lock must succeed once the in-flight show/hide drops it"
        );
        assert!(flag.load(Ordering::SeqCst));
        unlocker.join().expect("the unlocker thread must not panic");
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
