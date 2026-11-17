fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = if args.is_empty() { mace::ui::run_window() } else { mace::cli::run(&args) };
    std::process::exit(code);
}
