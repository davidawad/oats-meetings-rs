fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("ctl") {
        std::process::exit(oats_ctl::run(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("config") {
        std::process::exit(oats_core::config::cli::run_env(&args[1..]));
    }
    if args.iter().any(|a| a == "--version") {
        println!("{}", oats_core::version_line());
        return;
    }
    if args.iter().any(|a| a == "--headless") {
        // Engine-only mode; the control server arrives in dot-4uzgol.5.
        return;
    }
    #[cfg(feature = "gui")]
    oats_ui::run_hello();
    #[cfg(not(feature = "gui"))]
    eprintln!("built without the `gui` feature; use --headless");
}
