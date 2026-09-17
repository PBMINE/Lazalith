use crate::CycleCount;
use core::{error::Error, fmt};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockOverflow {
    pub current: CycleCount,
    pub delta: CycleCount,
}

impl fmt::Display for ClockOverflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "virtual cycle overflow: {} + {}",
            self.current.as_u64(),
            self.delta.as_u64()
        )
    }
}

impl Error for ClockOverflow {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtualClock {
    elapsed: CycleCount,
}

impl Default for VirtualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualClock {
    pub const fn new() -> Self {
        Self::at(CycleCount::new(0))
    }
    pub const fn at(elapsed: CycleCount) -> Self {
        Self { elapsed }
    }
    pub const fn elapsed(&self) -> CycleCount {
        self.elapsed
    }

    pub fn advanced(&self, delta: CycleCount) -> Result<Self, ClockOverflow> {
        self.elapsed
            .checked_add(delta)
            .map(Self::at)
            .ok_or(ClockOverflow {
                current: self.elapsed,
                delta,
            })
    }

    pub fn advance(&mut self, delta: CycleCount) -> Result<(), ClockOverflow> {
        *self = self.advanced(delta)?;
        Ok(())
    }

    pub fn prepare_advance(
        &mut self,
        delta: CycleCount,
    ) -> Result<PreparedAdvance<'_>, ClockOverflow> {
        let next = self.advanced(delta)?;
        Ok(PreparedAdvance { clock: self, next })
    }
}

#[must_use]
pub struct PreparedAdvance<'a> {
    clock: &'a mut VirtualClock,
    next: VirtualClock,
}

impl PreparedAdvance<'_> {
    pub const fn elapsed(&self) -> CycleCount {
        self.next.elapsed
    }
    pub fn commit(self) {
        *self.clock = self.next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_boundaries_preserve_clock_on_overflow() {
        let mut clock = VirtualClock::new();
        clock.advance(CycleCount::new(u64::MAX)).unwrap();
        let before = clock;
        assert_eq!(
            clock.advance(CycleCount::new(1)),
            Err(ClockOverflow {
                current: CycleCount::new(u64::MAX),
                delta: CycleCount::new(1)
            })
        );
        assert_eq!(clock, before);
        clock.advance(CycleCount::new(0)).unwrap();
        assert_eq!(clock, before);
    }

    #[test]
    fn prepared_advance_is_pure_until_single_use_commit() {
        let mut clock = VirtualClock::at(CycleCount::new(9));
        {
            let plan = clock.prepare_advance(CycleCount::new(5)).unwrap();
            assert_eq!(plan.elapsed(), CycleCount::new(14));
        }
        assert_eq!(clock.elapsed(), CycleCount::new(9));
        clock.prepare_advance(CycleCount::new(5)).unwrap().commit();
        assert_eq!(clock.elapsed(), CycleCount::new(14));
    }

    #[test]
    fn repeated_clock_sequences_are_deterministic_and_independent() {
        let mut first = VirtualClock::new();
        let mut second = VirtualClock::new();
        for delta in [0, 1, 17, 500, u64::MAX - 518] {
            first.advance(CycleCount::new(delta)).unwrap();
            second.advance(CycleCount::new(delta)).unwrap();
            assert_eq!(first, second);
        }
        assert_eq!(first.elapsed(), CycleCount::new(u64::MAX));
        second = VirtualClock::new();
        assert_eq!(first.elapsed(), CycleCount::new(u64::MAX));
        assert_eq!(second.elapsed(), CycleCount::new(0));
    }
}
