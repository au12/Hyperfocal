//! The `hyperfocal` binary. Everything lives in the library crate, so the fuzz targets
//! and the integration tests can reach the parsers and the API.

fn main() -> std::process::ExitCode {
    hyperfocal::main()
}
