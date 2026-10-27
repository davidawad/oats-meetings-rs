//! Thin control client (`oats-meetings-rs ctl <verb>`); socket protocol arrives in dot-4uzgol.5.

/// Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("version") => {
            println!("{}", oats_core::version_line());
            0
        }
        _ => {
            eprintln!("ctl: unknown verb (skeleton supports: version)");
            2
        }
    }
}
