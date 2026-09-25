use alloc::vec::Vec;
use lazalith_types::InterruptId;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InterruptController {
    pending: Vec<InterruptId>,
}

impl InterruptController {
    pub const fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    pub fn request(
        &mut self,
        id: InterruptId,
    ) -> Result<bool, alloc::collections::TryReserveError> {
        match self.pending.binary_search_by(|pending| pending.cmp(&id)) {
            Ok(_) => Ok(false),
            Err(position) => {
                self.pending.try_reserve(1)?;
                self.pending.insert(position, id);
                Ok(true)
            }
        }
    }

    pub fn peek(&self) -> Option<InterruptId> {
        self.pending.first().copied()
    }

    pub fn acknowledge(&mut self, id: InterruptId) -> bool {
        let Ok(position) = self.pending.binary_search_by(|pending| pending.cmp(&id)) else {
            return false;
        };
        self.pending.remove(position);
        true
    }

    pub fn contains(&self, id: InterruptId) -> bool {
        self.pending
            .binary_search_by(|pending| pending.cmp(&id))
            .is_ok()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = InterruptId> + '_ {
        self.pending.iter().copied()
    }

    pub fn reset(&mut self) {
        self.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn requests_coalesce_and_deliver_in_lowest_id_order() {
        let mut interrupts = InterruptController::new();
        for (id, inserted) in [
            (9, true),
            (3, true),
            (7, true),
            (3, false),
            (1, true),
            (9, false),
        ] {
            assert_eq!(interrupts.request(InterruptId::new(id)).unwrap(), inserted);
        }
        assert_eq!(interrupts.len(), 4);
        assert_eq!(
            interrupts.iter().collect::<Vec<_>>(),
            vec![1, 3, 7, 9]
                .into_iter()
                .map(InterruptId::new)
                .collect::<Vec<_>>()
        );
        assert_eq!(interrupts.peek(), Some(InterruptId::new(1)));
        assert!(interrupts.acknowledge(InterruptId::new(1)));
        assert!(!interrupts.acknowledge(InterruptId::new(1)));
        assert!(interrupts.request(InterruptId::new(1)).unwrap());
        assert_eq!(interrupts.peek(), Some(InterruptId::new(1)));
        assert!(interrupts.acknowledge(InterruptId::new(1)));
        assert_eq!(interrupts.peek(), Some(InterruptId::new(3)));
        interrupts.reset();
        assert!(interrupts.is_empty());
    }
}
