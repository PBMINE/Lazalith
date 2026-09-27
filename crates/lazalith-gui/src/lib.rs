//! Step 77: the SDL3 frontend.
//!
//! # What this is
//!
//! The window a person debugs a Lazalith program in. It shows the ten things the
//! roadmap lists — the machine's screen, the registers, the program counter, the
//! flags, the disassembly, memory, the stack, the console, the processes, and the
//! diagnostics — and it shows them by asking the debug API, never by reaching
//! into a machine.
//!
//! # How it is put together
//!
//! Three pieces, each with one job:
//!
//! - [`view`] decides *what* to show. It is pure: it takes a `DebugController` and
//!   returns panels of lines. It needs no display, which is what lets the tests
//!   check every panel against a real machine.
//! - [`window`] decides *where* to put it. It draws panels of lines with SDL3 and
//!   knows nothing about registers, addresses or source.
//! - [`font`] draws text, because a debugger's output is mostly words and a
//!   frontend that assumed the host had a font would work on one machine and show
//!   nothing on another.
//!
//! # What it is not allowed to do
//!
//! Reach a CPU. There is no `&mut LazalithMachine` in this crate and no path to
//! one, because [`DebugController`] does not hand them out — that is the shape of
//! the debug API, not a promise this crate makes about itself. Every value shown
//! comes from an owned snapshot, so a panel cannot show a program counter from one
//! moment and a stack pointer from another.
//!
//! # Where the `unsafe` is
//!
//! In [`lazalith_sdl3`], and only there. This crate is `unsafe_code = forbid` like
//! the rest of the workspace, and the SDL3 boundary is a separate crate precisely
//! so that stays true. The boundary crate compiles a C probe against SDL3's real
//! headers and asserts its own struct layouts against what the C compiler
//! measured, so "this matches SDL3" is a checked claim rather than a comment.

#![deny(missing_docs)]

pub mod font;
pub mod view;
pub mod window;

pub use view::{
    Diagnostic, DiagnosticKind, Diagnostics, Emphasis, Line, Panel, Screen, Section, SourcePlace,
    View, ViewOptions,
};
pub use window::Windowed;

pub mod control;
pub use control::{Control, Controls, Outcome, Refusal};
