//! The frame contract between a front end and the native backend.
//!
//! # Why this is here and not in a front end
//!
//! A lowered function is a list of IR values, and every one of those values has
//! to live *somewhere* while the function runs. Two components have to agree on
//! where: the front end that decides, and the backend that emits the address.
//!
//! That agreement is a contract, and a contract needs a home both sides can see.
//! It used to live in the Lazen front end, which meant the native backend
//! depended on the Lazen front end to be handed its own input — and a C program
//! could only be compiled by importing Lazen's types. So the contract lives here,
//! in the layer both already depend on, and it says nothing about any language.
//!
//! # What the backend actually reads
//!
//! Only three things: how big the frame is, which slots a prologue must fill
//! from the argument registers, and where each slot starts. Everything else in
//! [`FrameSlot`] is there so a front end can describe its own reasoning, and
//! because a slot record that cannot say why it exists is a slot record nobody
//! can review.

use alloc::string::String;
use alloc::vec::Vec;

use crate::Type;

/// Why a slot exists.
///
/// Recorded rather than inferred, because the *reason* is what a reader needs and
/// the offset alone does not carry it: a slot holding a `loop`'s bound and a slot
/// holding a loop body's temporary can share an address in a later revision, and
/// only one of them may be read before it is written.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SlotPurpose {
    /// A local, parameter or temporary the front end allocated.
    Local,
    /// A value that the arms of an `if` write and the join reads.
    JoinValue,
    /// A loop's end value, computed once before the loop.
    LoopBound,
    /// The result of a short-circuiting operator.
    ShortCircuit,
    /// Somewhere to put a value whose width is changing.
    CastScratch,
}

/// One slot in a lowered function's frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameSlot {
    /// The slot's byte offset from the frame base.
    pub offset: u32,
    /// The slot's size in bytes.
    pub size: u32,
    /// The type the slot holds.
    ///
    /// The *IR* type rather than a source-language one, because this is what the
    /// backend will actually store there. A source type that has no IR form — a
    /// `void *` spelled one way or another, say — has been translated by the time
    /// a slot exists.
    pub ty: Type,
    /// Why the slot exists.
    pub purpose: SlotPurpose,
    /// Whether this slot receives one of the function's parameters.
    ///
    /// A backend needs this to know which slots a prologue must fill from the
    /// argument registers, and a parameter is otherwise just a local.
    pub is_parameter: bool,
    /// The local's name, when it has one.
    pub name: Option<String>,
}

/// A lowered function's frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FrameLayout {
    /// The function's qualified name.
    pub function: String,
    /// The frame's total size in bytes, rounded up to a whole word.
    pub size: u32,
    /// The slots, in offset order.
    pub slots: Vec<FrameSlot>,
}
