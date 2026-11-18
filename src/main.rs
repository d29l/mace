fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // the first run after an update: the menu as this version has it
    mace::desktop::refresh_after_update();
    let code =if args.is_empty() { mace::ui::run_window() } else { mace::cli::run(&args) };
    std::process::exit(code);
}
