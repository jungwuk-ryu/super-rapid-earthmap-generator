#[cfg(feature = "fast-allocator")]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    std::process::exit(earthmap_cli::run(std::env::args().skip(1)));
}
