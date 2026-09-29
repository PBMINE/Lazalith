//! `lazcc` — compile a Lazen or C program to a `.lzo` object.
//!
//! One stage, one tool. See `lazalith_driver::tools` for what each one does and why
//! the tools are thin.

use std::ffi::OsString;

fn main() -> std::process::ExitCode {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    lazalith_driver::tools::CcTool::main(&arguments)
}
