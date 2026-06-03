fn main() {
    std::process::exit(earthmap_cli::run(std::env::args().skip(1)));
}
