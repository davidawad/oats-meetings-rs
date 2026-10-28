//! Command line of the app binary (dot-4uzgol.25): which mode `argv` selects, and the usage text.
//! A flag the app does not know is an error (exit 2), never a silent GUI launch.

pub const USAGE: &str = "usage: oats-meetings-rs [--headless | --version | --help]
       oats-meetings-rs ctl <action words> [params]   control a running app (try: ctl list-actions)
       oats-meetings-rs config <subcommand>           inspect or edit the config file

With no arguments the app opens its window and serves the control socket.
  --headless   run the engine and control socket without a window
  --version    print the version and exit
  -h, --help   print this text and exit";

#[derive(Debug, PartialEq, Eq)]
pub enum Mode {
    Ctl,
    Config,
    Version,
    Help,
    Headless,
    Gui,
    /// An argument the app does not know, to print and exit 2.
    Bad(String),
}

pub fn mode(args: &[String]) -> Mode {
    match args.first().map(String::as_str) {
        Some("ctl") => return Mode::Ctl,
        Some("config") => return Mode::Config,
        _ => {}
    }
    let mut m = Mode::Gui;
    for a in args {
        match a.as_str() {
            "-h" | "--help" => return Mode::Help,
            "--version" => m = Mode::Version,
            "--headless" if m == Mode::Gui => m = Mode::Headless,
            "--headless" => {}
            other => return Mode::Bad(other.to_string()),
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(a: &[&str]) -> Mode {
        mode(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn modes() {
        assert_eq!(m(&[]), Mode::Gui);
        assert_eq!(m(&["--headless"]), Mode::Headless);
        assert_eq!(m(&["--version"]), Mode::Version);
        assert_eq!(m(&["--headless", "--version"]), Mode::Version);
        assert_eq!(m(&["ctl", "ping"]), Mode::Ctl);
        assert_eq!(m(&["config", "--help"]), Mode::Config);
        for h in ["--help", "-h"] {
            assert_eq!(m(&[h]), Mode::Help);
            assert_eq!(m(&["--headless", h]), Mode::Help);
        }
        assert_eq!(m(&["--bogus"]), Mode::Bad("--bogus".into()));
        assert_eq!(m(&["--headless", "-x"]), Mode::Bad("-x".into()));
        assert_eq!(m(&["stray"]), Mode::Bad("stray".into()));
    }
}
