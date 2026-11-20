//! A capped undo/redo history.

/// A snapshot-based undo/redo stack with a fixed capacity (decision D-016).
///
/// Snapshots are pushed by mutating operations. The oldest snapshot is dropped
/// once the cap is reached, so only the most recent [`UndoStack::capacity`]
/// operations can be undone.
#[derive(Clone, Debug)]
pub struct UndoStack<T> {
    past: Vec<T>,
    future: Vec<T>,
    cap: usize,
}

impl<T> UndoStack<T> {
    /// Creates a stack holding at most `cap` undo snapshots.
    pub fn new(cap: usize) -> Self {
        UndoStack {
            past: Vec::new(),
            future: Vec::new(),
            cap: cap.max(1),
        }
    }

    /// The configured capacity.
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// The number of undo steps currently available.
    pub fn undo_len(&self) -> usize {
        self.past.len()
    }

    /// The number of redo steps currently available.
    pub fn redo_len(&self) -> usize {
        self.future.len()
    }

    /// Whether an undo is available.
    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    /// Whether a redo is available.
    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }
}

impl<T: Clone> UndoStack<T> {
    /// Records a snapshot before a mutation, discarding any redo history.
    pub fn record(&mut self, snapshot: T) {
        self.past.push(snapshot);
        if self.past.len() > self.cap {
            self.past.remove(0);
        }
        self.future.clear();
    }

    /// Undoes, returning the previous snapshot and storing `current` for redo.
    pub fn undo(&mut self, current: T) -> Option<T> {
        let previous = self.past.pop()?;
        self.future.push(current);
        Some(previous)
    }

    /// Redoes, returning the next snapshot and storing `current` for undo.
    pub fn redo(&mut self, current: T) -> Option<T> {
        let next = self.future.pop()?;
        self.past.push(current);
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_and_redo_round_trip() {
        let mut stack = UndoStack::new(64);
        stack.record(1);
        stack.record(2);
        assert_eq!(stack.undo(3), Some(2));
        assert_eq!(stack.undo(2), Some(1));
        assert_eq!(stack.undo(1), None);
        assert!(stack.can_redo());
        assert_eq!(stack.redo(1), Some(2));
        assert_eq!(stack.redo(2), Some(3));
        assert_eq!(stack.redo(3), None);
    }

    #[test]
    fn recording_discards_redo_history() {
        let mut stack = UndoStack::new(64);
        stack.record(1);
        assert_eq!(stack.undo(2), Some(1));
        stack.record(3);
        assert!(!stack.can_redo());
    }

    #[test]
    fn capacity_caps_the_number_of_undo_steps() {
        let mut stack = UndoStack::new(64);
        for i in 0..100 {
            stack.record(i);
        }
        assert_eq!(stack.undo_len(), 64);
        for _ in 0..64 {
            assert!(stack.undo(999).is_some());
        }
        assert!(
            !stack.can_undo(),
            "only the most recent 64 operations are undoable"
        );
    }
}
