use std::sync::atomic::{AtomicBool, Ordering};

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_sig: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::Relaxed);
}

/// RAII guard that restores previous signal handlers on drop.
pub struct SignalGuard {
    prev: (libc::sighandler_t, libc::sighandler_t),
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        restore_handlers(self.prev);
    }
}

/// Install SIGINT and SIGTERM handlers that set a shutdown flag
/// instead of terminating the process immediately.
///
/// Returns a guard that restores the previous handlers on drop.
pub fn install_handlers() -> SignalGuard {
    // SAFETY: `handle_signal` only performs an atomic store, which is
    // async-signal-safe. We read the previous handlers to restore later.
    let prev = unsafe {
        let prev_int = libc::signal(
            libc::SIGINT,
            handle_signal as *const () as libc::sighandler_t,
        );
        let prev_term = libc::signal(
            libc::SIGTERM,
            handle_signal as *const () as libc::sighandler_t,
        );
        (prev_int, prev_term)
    };
    SignalGuard { prev }
}

fn restore_handlers(prev: (libc::sighandler_t, libc::sighandler_t)) {
    // SAFETY: restoring handlers that were returned by a prior `signal()` call.
    unsafe {
        libc::signal(libc::SIGINT, prev.0);
        libc::signal(libc::SIGTERM, prev.1);
    }
}

/// Returns `true` if a SIGINT or SIGTERM has been received.
pub fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::Relaxed)
}

/// Check for pending shutdown and return an error if one was requested.
pub fn check_shutdown() -> anyhow::Result<()> {
    if shutdown_requested() {
        anyhow::bail!("Interrupted by signal — cleaning up");
    }
    Ok(())
}
