//! The record of what the outside world did, in an order that can be replayed.
//!
//! # Why a log and not a recording
//!
//! A program does not read the keyboard; it reads a *device*, and a device is a
//! function of two things: the input the host gave it and the virtual time it has
//! been ticked. So a run is reproducible from the input alone, provided the input
//! arrives at the same cycle every time. That is what [`InputLog`] is: events with
//! the cycle they arrived on, ordered, and serializable.
//!
//! # Why the order is checked and not sorted
//!
//! Two events with the same cycle could go either way, and a log that put them in
//! an arbitrary order would replay into a different run than the one that was
//! recorded — the worst possible failure, because the reproduction would be
//! confidently wrong. So [`InputLog::push`] refuses an event that arrives before
//! the last one, and the log is a list rather than a map. Two events on the same
//! cycle keep the order they were pushed in, and that order is part of the
//! artifact.
//!
//! # Why a byte format at all
//!
//! Because the step's promise is that a bug is reproducible from a *file*. A log
//! that only existed in memory would be reproducible on the machine that recorded
//! it and nowhere else, which is half the promise. The format is small and fixed,
//! and it carries the same "keep what you do not understand" rule the input
//! device's own records use: an unknown event kind is a `u32` this build may not
//! recognise, and refusing the file would make a newer host's log unreadable by an
//! older build, which is the opposite of what a reproduction artifact is for.

use alloc::vec::Vec;

use lazalith_devices::input::{EVENT_BYTES, Event};
use lazalith_types::CycleCount;

/// The magic at the front of an encoded log.
const MAGIC: [u8; 4] = *b"LZIN";
/// The format version this build writes.
const VERSION: u16 = 1;
/// The bytes one event takes inside a record, before the cycle.
const EVENT_RECORD_BYTES: usize = EVENT_BYTES as usize;
/// The bytes one record takes: the cycle, then the event.
const RECORD_BYTES: usize = 8 + EVENT_RECORD_BYTES;

/// What can be wrong with an input log.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputLogError {
    /// The bytes do not start with the magic.
    NotALog,
    /// The version is one this build does not write.
    Version(u16),
    /// The file ended in the middle of something.
    Truncated {
        /// What was being read.
        part: &'static str,
    },
    /// The file has bytes left over after everything it declared.
    Trailing {
        /// How many bytes were left.
        bytes: usize,
    },
    /// The event would have arrived before the one before it.
    OutOfOrder {
        /// The cycle it claims.
        at: u64,
        /// The cycle already recorded, which is later.
        after: u64,
    },
    /// The log claims more records than its bytes can hold.
    TooManyRecords {
        /// The count in the header.
        records: u32,
        /// How many the bytes could hold.
        possible: usize,
    },
}

impl core::fmt::Display for InputLogError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotALog => f.write_str("these bytes are not an input log"),
            Self::Version(version) => {
                write!(f, "input log version {version} is not one this build reads")
            }
            Self::Truncated { part } => write!(f, "the input log ends in the middle of its {part}"),
            Self::Trailing { bytes } => {
                write!(f, "{bytes} bytes are left over after the log ends")
            }
            Self::OutOfOrder { at, after } => write!(
                f,
                "an event at cycle {at} was pushed after one at cycle {after}"
            ),
            Self::TooManyRecords { records, possible } => write!(
                f,
                "the log claims {records} records, which is more than {possible} could fit"
            ),
        }
    }
}

/// One event, and the cycle it arrived on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputRecord {
    /// The cycle the host delivered the event on.
    ///
    /// Virtual time, not wall time. A log recorded with wall time would not replay
    /// on a slower machine, which defeats the point of having one.
    pub cycle: CycleCount,
    /// What happened.
    pub event: Event,
}

impl InputRecord {
    /// A record for `event` at `cycle`.
    pub const fn new(cycle: CycleCount, event: Event) -> Self {
        Self { cycle, event }
    }
}

/// Every event the outside world produced, in the order it produced them.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InputLog {
    records: Vec<InputRecord>,
}

impl InputLog {
    /// A log with nothing in it.
    pub const fn new() -> Self {
        Self {
            records: Vec::new(),
        }
    }

    /// Adds an event, if it did not arrive before the last one.
    ///
    /// The refusal is the point: see the module documentation on ordering.
    pub fn push(&mut self, cycle: CycleCount, event: Event) -> Result<(), InputLogError> {
        if let Some(last) = self.records.last() {
            if cycle < last.cycle {
                return Err(InputLogError::OutOfOrder {
                    at: cycle.as_u64(),
                    after: last.cycle.as_u64(),
                });
            }
        }
        self.records.push(InputRecord::new(cycle, event));
        Ok(())
    }

    /// How many events the log holds.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Whether the log is empty.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Every record, in order.
    pub fn records(&self) -> &[InputRecord] {
        &self.records
    }

    /// The events that arrived at or before `cycle`, and how many there are.
    ///
    /// The count is what a session needs to advance its own cursor, so the two
    /// cannot disagree about how far the log has been read.
    pub fn events_through(&self, cycle: CycleCount) -> usize {
        self.records.partition_point(|record| record.cycle <= cycle)
    }

    /// The last cycle the log mentions, or zero if it is empty.
    pub fn last_cycle(&self) -> CycleCount {
        self.records
            .last()
            .map_or(CycleCount::new(0), |record| record.cycle)
    }

    /// The log as bytes.
    ///
    /// The header carries the record count rather than the log deriving it from its
    /// own length, so a truncated file is a *truncation* and not a shorter log: the
    /// two are different bugs and only one of them is a corrupted artifact.
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.extend_from_slice(&VERSION.to_le_bytes());
        bytes.extend_from_slice(
            &u32::try_from(self.records.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        for record in &self.records {
            bytes.extend_from_slice(&record.cycle.as_u64().to_le_bytes());
            bytes.extend_from_slice(&record.event.encode());
        }
        bytes
    }

    /// A log from bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, InputLogError> {
        if bytes.len() < 10 || bytes[0..4] != MAGIC {
            return Err(InputLogError::NotALog);
        }
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version != VERSION {
            return Err(InputLogError::Version(version));
        }
        let records = u32::from_le_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]);
        let possible = bytes.len().saturating_sub(10) / RECORD_BYTES;
        if usize::try_from(records).unwrap_or(usize::MAX) > possible {
            return Err(InputLogError::TooManyRecords { records, possible });
        }
        let mut log = Self::new();
        for index in 0..usize::try_from(records).unwrap_or(0) {
            let at = 10 + index * RECORD_BYTES;
            let end = at + RECORD_BYTES;
            let record = bytes
                .get(at..end)
                .ok_or(InputLogError::Truncated { part: "records" })?;
            let cycle = CycleCount::new(u64::from_le_bytes(
                record[0..8].try_into().unwrap_or([0; 8]),
            ));
            let event =
                Event::decode(&record[8..]).ok_or(InputLogError::Truncated { part: "an event" })?;
            log.push(cycle, event)
                .map_err(|_| InputLogError::OutOfOrder { at: 0, after: 0 })?;
        }
        let left = bytes
            .len()
            .saturating_sub(10 + records as usize * RECORD_BYTES);
        if left != 0 {
            return Err(InputLogError::Trailing { bytes: left });
        }
        Ok(log)
    }
}
