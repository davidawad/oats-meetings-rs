mod cli;

use cli::Mode;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = cli::mode(&args);
    // sherpa-onnx models run in a child process of this binary, which ends when they unload
    // (dot-4uzgol.108).
    #[cfg(feature = "parakeet")]
    if let Ok(exe) = std::env::current_exe() {
        oats_core::stt::worker::set_command(exe, vec![]);
    }
    match mode {
        Mode::Ctl => std::process::exit(oats_ctl::run(&args[1..])),
        Mode::Mcp => std::process::exit(oats_ctl::mcp::run(&args[1..])),
        Mode::Config => std::process::exit(oats_core::config::cli::run_env(&args[1..])),
        Mode::Version => oats_core::log::out(oats_core::version_line()),
        Mode::Help => oats_core::log::out(cli::USAGE),
        Mode::Bad(a) => {
            oats_core::elog!("oats-meetings-rs: unknown argument '{a}'\n{}", cli::USAGE);
            std::process::exit(2);
        }
        #[cfg(feature = "parakeet")]
        Mode::SttWorker => std::process::exit(oats_core::stt::worker::serve()),
        #[cfg(not(feature = "parakeet"))]
        Mode::SttWorker => {
            oats_core::elog!("built without the `parakeet` feature: no stt worker");
            std::process::exit(2);
        }
        Mode::Headless => std::process::exit(oats_core::control::run_headless_env()),
        Mode::Gui => {
            #[cfg(feature = "gui")]
            std::process::exit(oats_ui::run_env());
            #[cfg(not(feature = "gui"))]
            {
                oats_core::elog!("built without the `gui` feature; use --headless");
                std::process::exit(2);
            }
        }
    }
}
