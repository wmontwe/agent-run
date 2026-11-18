//! The launcher's own termination (SIGTERM, SIGHUP, SIGINT): recorded, so the session it
//! started is still ended and nothing of it is left. The handler only sets a flag; whoever waits
//! checks it.

use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use std::sync::atomic::{AtomicBool, Ordering};

static STOPPED: AtomicBool = AtomicBool::new(false);

/// Records SIGTERM, SIGHUP and SIGINT instead of dying on them.
pub fn install() {
    for signal in [SIGHUP, SIGINT, SIGTERM] {
        // None of these is a signal signal-hook refuses, so registering can't fail.
        let _ = signal_hook::flag::register(signal, &STOPPED);
    }
}

/// Whether the launcher was asked to stop.
pub fn stopped() -> bool {
    STOPPED.load(Ordering::SeqCst)
}
