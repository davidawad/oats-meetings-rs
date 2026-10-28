//! Stderr that cannot kill the process. `eprintln!` panics when stderr is closed (a launcher that
//! exited, an ssh pipe that dropped: EPIPE), and the control socket outlives that launcher
//! (dot-4uzgol.33). Everything the app prints for humans goes through here and ignores a failed write.

use std::io::Write;

/// Write `msg` and a newline to stderr; a closed or full stderr is ignored.
pub fn line(msg: impl std::fmt::Display) {
    let _ = writeln!(std::io::stderr(), "{msg}");
}

/// Same for stdout.
pub fn out(msg: impl std::fmt::Display) {
    let _ = writeln!(std::io::stdout(), "{msg}");
}

/// `eprintln!` that ignores a failed write: `oats_core::elog!("cannot open {x}")`.
#[macro_export]
macro_rules! elog {
    ($($t:tt)*) => {
        $crate::log::line(format_args!($($t)*))
    };
}
