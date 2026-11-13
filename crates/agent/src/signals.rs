//! The launcher's own termination (SIGTERM, SIGHUP, SIGINT): recorded, so the session it
//! started is still ended and nothing of it is left. The handler only sets a flag; whoever waits
//! checks it.

use std::sync::atomic::{AtomicBool, Ordering};

static STOPPED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: i32) {
    STOPPED.store(true, Ordering::SeqCst);
}

unsafe extern "C" {
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
}

const SIGHUP: i32 = 1;
const SIGINT: i32 = 2;
const SIGTERM: i32 = 15;

/// Records SIGTERM, SIGHUP and SIGINT instead of dying on them.
pub fn install() {
    for signum in [SIGHUP, SIGINT, SIGTERM] {
        // Safety: the handler only stores into an atomic, which is async-signal-safe.
        unsafe {
            signal(signum, on_signal);
        }
    }
}

/// Whether the launcher was asked to stop.
pub fn stopped() -> bool {
    STOPPED.load(Ordering::SeqCst)
}
