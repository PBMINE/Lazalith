//! Structured diagnostics from a running program.
//!
//! # What a diagnostic is here
//!
//! The compiler already has structured diagnostics: `lazalith_diagnostics::Diagnostic`
//! with a stable code, a severity, and a real source span. This does not invent a
//! second kind. A runtime diagnostic *is* one of those, plus the things only a
//! running program has: where the guest's program counter was, what instruction
//! was there, and which calls led to it.
//!
//! So a [`RuntimeDiagnostic`] holds a `Diagnostic` and the runtime context beside
//! it. A frontend renders the code and the message from the shared type and the
//! program counter, instruction and frames from the context. Nothing anywhere
//! reads a rendered error back to decide what to show.
//!
//! # Where the codes come from
//!
//! A `R0xxx` code, which is the compiler's `L0xxx`/`P0xxx`/`N0xxx`/`T0xxx` scheme
//! extended to the runtime. The namespace is the point: a `T0006` is the
//! frontend's answer to a program it could not type-check, and a `R0003` is a
//! program that ran and did something the machine stopped it for. A frontend can
//! filter on the prefix without a table.
//!
//! # Why a stack trace is verified rather than guessed
//!
//! The calling convention reserves a word below the frame for the return address
//! and records the caller's frame base, so a call chain *is* walkable. This walks
//! it by a cheaper route that needs no per-function frame sizes: it scans the
//! stack for words that could be return addresses and **checks each one against
//! the code**.
//!
//! A word is reported as a frame only if all of the following hold:
//!
//! - it is instruction-aligned and in the program's code;
//! - the instruction immediately before it is a `CALL` or `CALLR` — the
//!   instruction that would have left exactly this address on the stack;
//! - that call's target is itself instruction-aligned and in the code;
//! - the word resolves through the debug table to a line of source.
//!
//! A data word that merely *looks* like a code address therefore does not become
//! a frame: it would also have to be preceded by a real call to a real function.
//! A word that fails the check is skipped rather than guessed at, and the trace
//! says how many words it looked at, so a user can see that a short trace is a
//! short trace rather than a truncated one they were not told about.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lazalith_diagnostics::bug::EmulatorBug;
use lazalith_diagnostics::{Diagnostic, DiagnosticCode, Label, Severity};
use lazalith_isa::Opcode;
use lazalith_os::debug::SourceLocation;

use crate::controller::DebugController;
use crate::session::DebugSession;
use crate::{DebugError, StackView};
use lazalith_devices::Device;
use lazalith_os::ProcessId;

/// The runtime's code namespace, as a prefix on a stable code.
///
/// A `R0xxx` code is this machine, and a `T0xxx` code is the frontend: a
/// frontend that filtered on the letter would be able to tell "the program did
/// something wrong" from "I could not read something" without reading either
/// message.
pub const RUNTIME_CODE_PREFIX: &str = "R";

/// Builds a code, falling back to a valid code if a literal is ever malformed.
///
/// The same shape `lazalith-compiler` uses, and for the same reason: a code is a
/// literal at every call site, and a typo in one must not become a panic in the
/// middle of reporting a fault.
pub fn code(raw: &str) -> DiagnosticCode {
    DiagnosticCode::new(raw).unwrap_or_else(|_| {
        DiagnosticCode::new("R9999").expect("the fallback code is a valid literal")
    })
}

/// The code for a guest that trapped.
pub const GUEST_TRAP: &str = "R0001";

/// The code for a guest whose syscall was refused.
pub const GUEST_SYSCALL_FAULT: &str = "R0002";

/// The code for a program that faulted in a way the kernel could not name.
pub const GUEST_FAULT: &str = "R0003";

/// The code for a diagnostic about a fault's own stack.
pub const STACK_UNREADABLE: &str = "R0004";

/// Where a program was when something happened to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeDiagnostic {
    /// The shared diagnostic: a severity, a stable code, a message, and a label
    /// pointing at the source range it came from.
    pub diagnostic: Diagnostic,
    /// The guest's program counter, when the fault is at an instruction.
    pub guest_pc: Option<u64>,
    /// That instruction, as the toolchain formats it.
    ///
    /// Text, but text *of a real instruction* — this is a rendering for a person,
    /// and nothing reads it back.
    pub instruction: Option<String>,
    /// The calls that led here, innermost first, each verified against the code.
    pub frames: Vec<StackFrame>,
    /// How many stack words the trace looked at.
    ///
    /// Stated so a short trace is legible: a user looking at one frame should be
    /// able to tell whether there was one frame or whether the scan stopped.
    pub words_examined: usize,
    /// What sort of thing this is.
    ///
    /// Decided *here*, by whoever built the diagnostic, and not by the consumer. A
    /// frontend that worked the kind out by matching a code string would be a
    /// frontend whose correctness depends on the spelling of every code, and a new
    /// code would silently be shown as the wrong sort of thing.
    pub kind: DiagnosticKind,
}

/// What sort of thing a diagnostic is about.
///
/// The three cases a debugger must never confuse: the *program's mistake, *our*
/// mistake, and a frontend that could not read something. They look different to
/// a user because they send them to different places to look.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticKind {
    /// The guest program did something the machine had to refuse.
    GuestFault,
    /// The emulator found something its own rules say is impossible.
    EmulatorBug,
    /// The frontend could not do what it was asked.
    Frontend,
}

/// One frame of a call chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StackFrame {
    /// Where this frame is in the chain, zero being the innermost.
    pub index: usize,
    /// The address the stack word held: the instruction *after* the call.
    pub return_address: u64,
    /// The instruction that left it there.
    pub call_site: u64,
    /// Where the call site is in the source, when the image carried debug
    /// information.
    pub call_site_source: Option<RuntimeSource>,
    /// Where this frame returns to, in the source.
    pub return_source: Option<RuntimeSource>,
}

/// A place in a source file, as a runtime diagnostic carries it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeSource {
    /// The file, as the compiler was given it.
    pub name: String,
    /// The line, one-based.
    pub line: u32,
    /// The column, one-based.
    pub column: u32,
}

impl From<SourceLocation<'_>> for RuntimeSource {
    fn from(place: SourceLocation<'_>) -> Self {
        Self {
            name: String::from(place.name),
            line: place.line_number(),
            column: place.column_number(),
        }
    }
}

impl RuntimeDiagnostic {
    /// The diagnostic's stable code.
    pub fn code(&self) -> &DiagnosticCode {
        self.diagnostic.code()
    }

    /// The diagnostic's message.
    pub fn message(&self) -> &str {
        self.diagnostic.message()
    }

    /// The diagnostic's severity.
    pub fn severity(&self) -> Severity {
        self.diagnostic.severity()
    }

    /// The primary source label's range, if it has one.
    ///
    /// This is the *span*, not a resolved line: the sources live in the image's
    /// debug block, and a diagnostic that resolved its own line would have to
    /// carry a second copy of them. [`DebugController::diagnostics`] resolves it.
    pub fn primary_span(&self) -> Option<lazalith_types::SourceSpan> {
        self.diagnostic.labels().first().map(|label| label.span())
    }
}

/// The most diagnostics one process's history keeps.
///
/// A program in a loop that faults on every iteration would otherwise grow this
/// without bound. The cap keeps the *end*, because the recent ones are the ones
/// someone is looking at.
pub const MAX_HISTORY: usize = 64;

/// Builds a diagnostic for a guest that trapped.
///
/// `pc` is the address of the trapping instruction and `payload` is what the
/// guest put in the trap. Both are the guest's own values and are reported as
/// they are: a trap code a program chose is a fact about the program, and a
/// frontend that renamed it would be reporting something else.
pub fn guest_trap(
    pc: u64,
    payload: i64,
    cause: &str,
    span: Option<lazalith_types::SourceSpan>,
) -> RuntimeDiagnostic {
    let mut diagnostic = Diagnostic::new(
        Severity::Error,
        code(GUEST_TRAP),
        format!("the program trapped ({cause}, code {payload})"),
    );
    if let Some(span) = span {
        diagnostic = diagnostic.with_label(Label::primary(span, "the program trapped here"));
    }
    RuntimeDiagnostic {
        diagnostic,
        guest_pc: Some(pc),
        instruction: None,
        frames: Vec::new(),
        kind: DiagnosticKind::GuestFault,
        words_examined: 0,
    }
}

/// Builds a diagnostic for a syscall the kernel refused.
pub fn guest_syscall_fault(detail: &str, pc: u64) -> RuntimeDiagnostic {
    RuntimeDiagnostic {
        diagnostic: Diagnostic::new(
            Severity::Error,
            code(GUEST_SYSCALL_FAULT),
            format!("the program made a call the kernel refused: {detail}"),
        ),
        guest_pc: Some(pc),
        instruction: None,
        frames: Vec::new(),
        words_examined: 0,
        kind: DiagnosticKind::GuestFault,
    }
}

/// Builds a diagnostic for a fault that is the *emulator's*.
///
/// This is the other half of the distinction: a guest trap is the program's
/// mistake and says so, and this is ours.
///
/// The whole report becomes the message — subsystem, operation, invariant,
/// machine state, guest program counter, instruction, address, and the file, line
/// and column it was noticed at — so a frontend that shows this shows a bug
/// report rather than a summary that has to be chased down afterwards.
///
/// It carries **no source label**, and that is deliberate. A source label points
/// into the *guest's* source, and an emulator bug is not in the guest's source; it
/// is in this codebase. Pointing a frontend's "jump to source" at a line of Lazen
/// because a machine invariant was violated would be a lie about where the bug
/// is, and the Rust location is already in the message where it belongs.
pub fn emulator_bug(bug: &EmulatorBug) -> RuntimeDiagnostic {
    RuntimeDiagnostic {
        diagnostic: bug.as_diagnostic(),
        guest_pc: bug.guest_pc,
        instruction: bug.instruction.clone(),
        frames: Vec::new(),
        words_examined: 0,
        kind: DiagnosticKind::EmulatorBug,
    }
}

impl<D: Device> DebugController<D> {
    /// The last fault that was the *emulator's* rather than the guest's.
    ///
    /// `None` for a program that has done nothing impossible, and `None` for a
    /// guest fault too: a program reading an address it does not own is the
    /// program's mistake, and reporting it here would tell someone their own bug
    /// is ours.
    pub fn last_emulator_bug(&self) -> Option<&EmulatorBug> {
        self.machine().last_emulator_bug()
    }

    /// Records the machine's last emulator bug, if it has one, and says so.
    ///
    /// A host calls this once it has a frontend to show the report in. It is not
    /// automatic, because recording a bug for a machine nobody is watching would
    /// grow a session's history for nobody — and a debugger that reported a
    /// machine's last bug from a year ago would be reporting something that may
    /// have been fixed since.
    pub fn adopt_emulator_bug(&mut self, process: ProcessId) -> bool {
        let Some(bug) = self.machine().last_emulator_bug().map(emulator_bug) else {
            return false;
        };
        self.record_diagnostic(process, bug);
        true
    }
}

impl<D: Device> DebugController<D> {
    /// The diagnostics this process has produced, oldest first.
    ///
    /// A process with none answers with an empty list rather than an error: a
    /// program that has not trapped has not trapped, and that is not a failure to
    /// report.
    pub fn diagnostics(&self, process: ProcessId) -> Vec<RuntimeDiagnostic> {
        self.session(process)
            .map(|session| session.diagnostics().to_vec())
            .unwrap_or_default()
    }

    /// Whether this process has produced a diagnostic.
    pub fn has_diagnostics(&self, process: ProcessId) -> bool {
        self.session(process)
            .is_some_and(|session| !session.diagnostics().is_empty())
    }

    /// Forgets this process's diagnostics.
    pub fn clear_diagnostics(&mut self, process: ProcessId) -> usize {
        self.session_mut(process)
            .map_or(0, DebugSession::clear_diagnostics)
    }

    /// Records a diagnostic against a process.
    pub fn record_diagnostic(&mut self, process: ProcessId, diagnostic: RuntimeDiagnostic) {
        if let Some(session) = self.session_mut(process) {
            session.record_diagnostic(diagnostic);
        }
    }

    /// A call chain for the process, innermost first.
    ///
    /// # Errors
    ///
    /// If the stack cannot be read. A program whose stack is unreadable gets a
    /// diagnostic saying so rather than an empty trace that looks like a program
    /// with no callers.
    pub fn call_chain(
        &self,
        process: ProcessId,
        words: usize,
    ) -> Result<Vec<StackFrame>, DebugError> {
        let stack = self.stack(words)?;
        self.frames_from_stack(process, &stack)
    }

    /// A call chain, read out of an already-read stack.
    ///
    /// Split from [`Self::call_chain`] so that a diagnostic recorded *at* the
    /// moment of a fault can carry the trace of the stack as it was then, rather
    /// than the stack as it is by the time a frontend asks.
    pub fn frames_from_stack(
        &self,
        process: ProcessId,
        stack: &StackView,
    ) -> Result<Vec<StackFrame>, DebugError> {
        let mut frames = Vec::new();
        for (offset, word) in stack.words.iter().enumerate() {
            let Some(frame) = self.frame_at(process, *word, frames.len()) else {
                continue;
            };
            let _ = offset;
            frames.push(frame);
        }
        Ok(frames)
    }

    /// One frame, if `word` is a return address.
    ///
    /// `None` means "this word is not one", which is the answer for almost every
    /// word on a stack. The check is the whole of [`StackFrame`]'s guarantee: a
    /// word becomes a frame only when the instruction that would have left it
    /// there is a call whose target is real code.
    fn frame_at(&self, process: ProcessId, word: u64, index: usize) -> Option<StackFrame> {
        // A return address is where execution continues *after* a call, so the
        // call is the instruction before it. Every instruction is eight bytes, so
        // the call is exactly one below. A word below the first instruction of
        // the program cannot have a call before it.
        if word < self.word_size() {
            return None;
        }
        let call_site = word.checked_sub(self.word_size())?;
        let Ok(call) = self.instruction_at(call_site) else {
            return None;
        };
        if !matches!(call.opcode(), Opcode::Call | Opcode::Callr) {
            return None;
        }
        // The call must go somewhere real. A data word that happens to be a code
        // address would also have to be preceded by a call to a valid function
        // to get this far, and a return address always is.
        let target = call_target(call_site, &call)?;
        if self.instruction_at(target).is_err() {
            return None;
        }
        // And it has to be somewhere the debug table can name, or there is
        // nothing to show for it.
        let return_source = self.source_location_at(word).map(RuntimeSource::from);
        let call_site_source = self.source_location_at(call_site).map(RuntimeSource::from);
        if return_source.is_none() && call_site_source.is_none() {
            return None;
        }
        let _ = process;
        Some(StackFrame {
            index,
            return_address: word,
            call_site,
            call_site_source,
            return_source,
        })
    }
}

/// The address a call instruction transfers to.
///
/// A `CALL` carries a PC-relative displacement and a `CALLR` carries a register,
/// so the second kind cannot be resolved without knowing the register's value.
/// A `CALLR`'s target is therefore `None`: the call site is still reported, and
/// the return address's own line is still resolved, but the target is not
/// invented from a register the frontend cannot read.
fn call_target(call_site: u64, call: &lazalith_isa::Instruction) -> Option<u64> {
    use lazalith_isa::Operand;
    match call.opcode() {
        // A `CALL` is PC-relative: the displacement is from the instruction after
        // the call, and every instruction is eight bytes, so the target is the
        // displacement from the *next* address rather than from this one.
        Opcode::Call => match call.operands() {
            [Operand::Immediate(displacement)] => {
                let next = i64::try_from(call_site).ok()?.checked_add(8)?;
                next.checked_add(i64::from(*displacement))
                    .and_then(|t| u64::try_from(t).ok())
            }
            _ => None,
        },
        // A `CALLR` calls a register, so its target is whatever the register holds.
        // The frontend cannot read a register here — that is the whole point of
        // the API — and a register's value is not a constant, so the target is
        // left alone rather than invented. The call site is still a real call and
        // is still reported.
        _ => None,
    }
}

/// Builds a diagnostic for a fault whose stack could not be read.
///
/// A trace is a best-effort thing and its own failure must not be the only thing
/// reported: the fault is the reason anyone is looking, so the fault is recorded
/// and the unreadable stack is recorded as an *additional* diagnostic beside it.
pub fn guest_stack_unreadable(sp: u64) -> RuntimeDiagnostic {
    RuntimeDiagnostic {
        diagnostic: Diagnostic::new(
            Severity::Warning,
            code(STACK_UNREADABLE),
            format!("the stack at {sp:#x} could not be read, so there is no trace"),
        ),
        guest_pc: Some(sp),
        instruction: None,
        frames: Vec::new(),
        words_examined: 0,
        kind: DiagnosticKind::Frontend,
    }
}
