mod cli;

use cli::Mode;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::mode(&args) {
        Mode::Ctl => std::process::exit(oats_ctl::run(&args[1..])),
        Mode::Config => std::process::exit(oats_core::config::cli::run_env(&args[1..])),
        Mode::Version => oats_core::log::out(oats_core::version_line()),
        Mode::Help => oats_core::log::out(cli::USAGE),
        Mode::Bad(a) => {
            oats_core::elog!("oats-meetings-rs: unknown argument '{a}'\n{}", cli::USAGE);
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
