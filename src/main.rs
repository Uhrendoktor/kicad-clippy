#![forbid(unsafe_code)]

//! Executable entry point for the kicad-clippy command.

/// Starts the command-line application and returns its process exit status.
///
/// # Returns
///
/// The exit status returned by the shared command-line implementation.
fn main() -> std::process::ExitCode {
    kicad_clippy::run()
}
