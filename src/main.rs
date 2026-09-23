fn main() {
    std::process::exit(ai_dev_orchestrator::cli::run(std::env::args().skip(1)));
}
