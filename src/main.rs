fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("the window isn't built yet");
        std::process::exit(2);
    }
    std::process::exit(mace::cli::run(&args));
}
