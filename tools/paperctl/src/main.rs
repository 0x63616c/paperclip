//! Thin entry point. Everything lives in the `paperctl` library (see
//! `lib.rs`) so it can be exercised by tests and by `cargo xtask docs`
//! (WWW-48); this binary is nothing but a call into it.

use std::process::ExitCode;

fn main() -> ExitCode {
    paperctl::main()
}
