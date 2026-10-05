fn main() -> std::process::ExitCode {
    deskjet::run(std::env::args().skip(1).collect())
}
