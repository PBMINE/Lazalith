//! Booting an image and running it to completion.
//!
//! Step 96 is the step that runs the whole chain — Lazen source, compiler, object,
//! linker, `.lzx`, loader, process, syscalls, virtual hardware, emulator — and this
//! module is the part of that chain that is not compilation: everything from a
//! serialised image to a program's output and exit status.
//!
//! It used to live in the `lazen` command, which was the wrong place for three
//! reasons. It is not a command's business: a library caller with an image and a
//! machine should not have to shell out to get it run. It was untestable, because
//! the only way to reach it was to build the command and run it, so the one code
//! path in the project that boots a *compiled* program was the one code path with
//! no unit tests under it. And it could not be given a device, because the command
//! has no way to ask for one.
//!
//! So it is here, where the command and the integration test can both call it, and
//! where it can be handed virtual hardware.

use alloc::format;
use alloc::string::{String, ToString};

use alloc::vec::Vec;

use lazalith_boot::{BootImage, KERNEL_LOAD_ADDRESS};
use lazalith_cpu::{Privilege, TrapCause};
use lazalith_devices::{Device, DeviceManager, NoDevice};
use lazalith_isa::{Instruction, Opcode, encode};
use lazalith_machine::LazalithMachine;
use lazalith_os::{
    KernelError, KernelServiceOutcome, LazalithKernel, LzxArchitecture, LzxImage, ProcessId,
    ProcessState, SchedulerError, ThreadId, VirtualFileSystem, VirtualTerminal,
};
use lazalith_types::{ArchitectureConfig, InstructionAddress};

/// How many instructions a program is given before it is reported unfinished.
pub const STEP_BUDGET: u64 = 5_000_000;

/// What a finished run produced.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Finished {
    /// The status the program reported.
    pub exit_code: u32,
    /// Everything the program wrote to the console.
    pub output: Vec<u8>,
    /// How many instructions the machine executed.
    pub instructions: u64,
    /// How many traps the kernel handled, which is how many syscalls the program
    /// made.
    pub syscalls: u64,
}

/// Why an image could not be run to completion.
#[derive(Debug)]
pub enum RunError {
    /// The image was not a usable `.lzx`.
    Image(lazalith_os::LzxError),
    /// The machine, the boot image, or the kernel refused to start.
    Start(String),
    /// The program's own `TRAP`: a bounds check, a runtime check, an explicit trap.
    ///
    /// A guest trap and an emulator failure are different things, and a caller that
    /// cannot tell them apart has a bug in its error handling rather than a program
    /// with a bug. This is the guest's, and it is named.
    GuestTrap {
        /// The trap's cause.
        cause: TrapCause,
        /// The trap's payload.
        payload: i64,
        /// The guest program counter, in the trap frame.
        pc: u64,
    },
    /// The kernel reported a fault on the program's behalf.
    Fault(String),
    /// The program was still running when the budget ran out.
    Budget { budget: u64 },
    /// The program reported a status but the scheduler did not mark it exited.
    Inconsistent {
        /// The status that was reported.
        exit_code: u32,
        /// The state the process is actually in.
        state: ProcessState,
    },
}

impl core::fmt::Display for RunError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Image(error) => write!(formatter, "the image is not a usable .lzx: {error}"),
            Self::Start(detail) => write!(formatter, "the machine did not start: {detail}"),
            Self::GuestTrap { cause, payload, pc } => write!(
                formatter,
                "the program trapped ({cause:?}, payload {payload}) at {pc:#x}"
            ),
            Self::Fault(detail) => write!(formatter, "the program faulted: {detail}"),
            Self::Budget { budget } => {
                write!(
                    formatter,
                    "the program did not finish in {budget} instructions"
                )
            }
            Self::Inconsistent { exit_code, state } => write!(
                formatter,
                "the program reported status {exit_code} but is still {state:?}"
            ),
        }
    }
}

impl core::error::Error for RunError {}

/// A two-instruction supervisor kernel: a `NOP` and the `RFE` that hands off.
///
/// The kernel is one instruction longer than the trap vector it shares an address
/// with, which is why the trap vector is set to the load address plus eight.
pub fn supervisor_kernel(architecture: ArchitectureConfig) -> Vec<u8> {
    let mut kernel = Vec::with_capacity(16);
    kernel.extend_from_slice(
        &encode(
            architecture,
            &Instruction::new(architecture, Opcode::Nop, &[]).expect("a NOP encodes"),
        )
        .expect("the NOP encodes"),
    );
    kernel.extend_from_slice(
        &encode(
            architecture,
            &Instruction::new(architecture, Opcode::Rfe, &[]).expect("an RFE encodes"),
        )
        .expect("the RFE encodes"),
    );
    kernel
}

/// Boots a machine with the two-instruction kernel and steps it once, so that the
/// machine is in user mode before anything else happens to it.
///
/// A separate function because the ordering is the point: a program is never given
/// the chance to run before the machine has left supervisor mode, and a test that
/// asserted the privilege *after* the run would not notice if that changed.
///
/// The devices are attached here rather than after the machine is built, because a
/// device attached later is a device the program could already have been given a
/// reason to believe was not there.
pub fn boot<D: Device>(
    architecture: ArchitectureConfig,
    devices: DeviceManager<D>,
) -> Result<LazalithMachine<D>, RunError> {
    let kernel = supervisor_kernel(architecture);
    let image = BootImage::new(architecture, kernel, 0)
        .map_err(|error| RunError::Start(error.to_string()))?;
    let mut machine = image
        .start(devices)
        .map_err(|error| RunError::Start(error.to_string()))?;
    machine
        .set_trap_vector(InstructionAddress::new(KERNEL_LOAD_ADDRESS + 8))
        .map_err(|error| RunError::Start(error.to_string()))?;
    if machine.architectural_state().privilege() != Privilege::Supervisor {
        return Err(RunError::Start(String::from(
            "the machine did not start in supervisor mode",
        )));
    }
    machine
        .step()
        .map_err(|error| RunError::Start(error.to_string()))?;
    Ok(machine)
}

/// Runs a serialised image to completion, with no devices attached.
///
/// The convenience form of [`run_image_with`], and the one a terminal session wants:
/// a program that reaches for a device gets a fault it can report, rather than a
/// device nobody asked for.
pub fn run_image(image: &[u8], architecture: ArchitectureConfig) -> Result<Finished, RunError> {
    run_image_with(image, architecture, DeviceManager::<NoDevice>::new())
}

/// Runs a serialised image to completion, with the given devices attached.
///
/// This is the whole of step 96's chain after compilation. The image is read back
/// from its own bytes through the `.lzx` reader rather than being used as the
/// in-memory image it was built as, because a run that skipped the reader would not
/// be the run a user's image gets — and a file format whose reader is never
/// exercised by the only end-to-end test is a file format that can be wrong.
pub fn run_image_with<D: Device>(
    image: &[u8],
    architecture: ArchitectureConfig,
    devices: DeviceManager<D>,
) -> Result<Finished, RunError> {
    run_image_on(image, architecture, devices, STEP_BUDGET)
}

/// The same, with an instruction budget the caller chooses.
pub fn run_image_on<D: Device>(
    image: &[u8],
    architecture: ArchitectureConfig,
    devices: DeviceManager<D>,
    budget: u64,
) -> Result<Finished, RunError> {
    let loaded = LzxImage::from_bytes(image).map_err(RunError::Image)?;
    run_loaded(loaded, architecture, devices, budget)
}

/// Runs an image that has already been read, with an instruction budget.
///
/// The reader and the runner are separate so that a caller holding a decoded image
/// — the integration test, mostly — can run it without re-serialising it, and so
/// that the file format is exercised by exactly one function that both paths share.
pub fn run_loaded<D: Device>(
    image: LzxImage,
    architecture: ArchitectureConfig,
    devices: DeviceManager<D>,
    budget: u64,
) -> Result<Finished, RunError> {
    let mut machine = boot(architecture, devices)?;
    let mut kernel = LazalithKernel::new(
        budget,
        VirtualTerminal::new(b"").expect("a terminal over an empty byte string"),
        VirtualFileSystem::with_defaults().map_err(|error| RunError::Start(error.to_string()))?,
    )
    .map_err(|error| RunError::Start(error.to_string()))?;
    // One and one: a single-threaded program, which is all Lazen v1 can be. Zero is
    // not a usable identifier because the ids are `NonZeroU32`, so this cannot be a
    // silently wrong value.
    let process_id = ProcessId::new(1).expect("one is a valid process id");
    let thread_id = ThreadId::new(1).expect("one is a valid thread id");
    kernel
        .start_image(image, process_id, thread_id)
        .map_err(|error| RunError::Start(error.to_string()))?;

    let mut exit_code = None;
    let mut syscalls = 0u64;
    let mut instructions = 0u64;
    for _ in 0..budget {
        instructions = instructions.saturating_add(1);
        // A program that has exited leaves nothing to step. That is the normal end
        // of a run, not a failure, so it ends the loop — the status was recorded on
        // the step before. A scheduler that says so with *nothing* exited is a real
        // problem and falls through to the error below.
        let step = match kernel.step(&mut machine) {
            Ok(step) => step,
            Err(KernelError::Scheduler(SchedulerError::NoRunnableProcess))
                if exit_code.is_some() =>
            {
                break;
            }
            Err(error) => return Err(RunError::Fault(error.to_string())),
        };
        match step.outcome {
            Some(KernelServiceOutcome::Exit(code)) => {
                exit_code = Some(code);
                break;
            }
            Some(KernelServiceOutcome::Fault(error)) => {
                return Err(RunError::Fault(format!("{error:?}")));
            }
            // A guest trap is the program's own `TRAP` — a bounds check, a runtime
            // check, an explicit trap. It is not an emulator failure, and the
            // distinction is the whole reason this variant exists.
            Some(KernelServiceOutcome::GuestTrap { cause, payload }) => {
                return Err(RunError::GuestTrap {
                    cause,
                    payload,
                    pc: machine.architectural_state().pc().as_u64(),
                });
            }
            // A dispatched syscall comes back as `Return`, and the last one as
            // `Exit`. That pair is the count of syscalls the program made, and it
            // is the kernel reporting them rather than this runner inferring them
            // from a trap it no longer has: the kernel replaces the step it trapped
            // on with the step that returned from the syscall, so the trap event is
            // not visible here by the time the run has moved on from it.
            Some(KernelServiceOutcome::Return(_)) => syscalls = syscalls.saturating_add(1),
            None => {}
        }
    }
    let exit_code = exit_code.ok_or(RunError::Budget { budget })?;
    // The process's own record is checked as well as the exit code, because a status
    // that reached the caller but left the process marked running would mean the
    // kernel and the exit path disagree.
    let process = kernel
        .scheduler()
        .process(process_id)
        .ok_or_else(|| RunError::Start(String::from("the process is no longer known")))?;
    if process.state() != ProcessState::Exited {
        return Err(RunError::Inconsistent {
            exit_code,
            state: process.state(),
        });
    }
    Ok(Finished {
        exit_code,
        output: kernel.terminal().terminal().output().to_vec(),
        instructions,
        syscalls,
    })
}

/// The architecture a path's extension asks for, defaulting to 64-bit.
pub fn architecture_for(path: &str) -> ArchitectureConfig {
    if path.ends_with(".lzx32") {
        LzxArchitecture::Lz32.config()
    } else {
        ArchitectureConfig::lz64()
    }
}
