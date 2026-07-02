use std::collections::BTreeSet;

/// Holds back live SlotComplete emission until a slot is `window` slots behind the
/// highest BlockMeta seen, so a transaction update Yellowstone delivers after
/// BlockMeta(S) still lands in slot S's buffer before S is finalized.
pub(crate) struct SlotCompletionWindow {
    // Slots a completion is held behind the tip; 0 emits immediately.
    window: u64,
    // Highest BlockMeta slot seen, used as the release clock.
    high_meta: u64,
    // Observed-but-not-yet-released slots, ordered so they release ascending.
    pending: BTreeSet<u64>,
    // Highest slot already released, so a duplicate/late BlockMeta never re-emits it.
    last_emitted: Option<u64>,
}

impl SlotCompletionWindow {
    pub(crate) fn new(window: u64) -> Self {
        Self {
            window,
            high_meta: 0,
            pending: BTreeSet::new(),
            last_emitted: None,
        }
    }

    /// Record a BlockMeta for `slot` and return the slots now eligible for
    /// SlotComplete, ascending. `window == 0` returns `slot` immediately,
    /// except a slot already released is never repeated.
    pub(crate) fn observe(&mut self, slot: u64) -> Vec<u64> {
        self.high_meta = self.high_meta.max(slot);

        // At or below the release frontier means already finalized (duplicate/reorder); never re-queue.
        if self.last_emitted.is_none_or(|emitted| slot > emitted) {
            self.pending.insert(slot);
        }

        let threshold = self.high_meta.saturating_sub(self.window);
        let mut ready = Vec::new();
        while let Some(&s) = self.pending.first() {
            if s <= threshold {
                self.pending.pop_first();
                self.last_emitted = Some(s);
                ready.push(s);
            } else {
                break;
            }
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_zero_is_passthrough() {
        let mut w = SlotCompletionWindow::new(0);
        assert_eq!(w.observe(1), vec![1]);
        assert_eq!(w.observe(2), vec![2]);
        assert_eq!(w.observe(3), vec![3]);
    }

    #[test]
    fn lags_by_window() {
        let mut w = SlotCompletionWindow::new(3);
        assert_eq!(w.observe(1), Vec::<u64>::new());
        assert_eq!(w.observe(2), Vec::<u64>::new());
        assert_eq!(w.observe(3), Vec::<u64>::new());
        assert_eq!(w.observe(4), vec![1]);
        assert_eq!(w.observe(5), vec![2]);
    }

    #[test]
    fn startup_holds_until_high_ge_window() {
        let mut w = SlotCompletionWindow::new(5);
        assert_eq!(w.observe(1), Vec::<u64>::new());
        assert_eq!(w.observe(2), Vec::<u64>::new());
    }

    #[test]
    fn out_of_order_meta() {
        let mut w = SlotCompletionWindow::new(1);
        // The higher slot arrives first and is held (it is the tip).
        assert_eq!(w.observe(5), Vec::<u64>::new());
        // The lower slot arrives late; it is now far enough behind to release,
        // but the tip (5) stays pending.
        assert_eq!(w.observe(3), vec![3]);
    }

    #[test]
    fn duplicate_meta_emits_once() {
        let mut w = SlotCompletionWindow::new(0);
        assert_eq!(w.observe(4), vec![4]);
        // A repeated BlockMeta for the same slot must not re-finalize it.
        assert_eq!(w.observe(4), Vec::<u64>::new());
    }

    #[test]
    fn skipped_slots_flush_in_order() {
        let mut w = SlotCompletionWindow::new(2);
        let mut emitted = Vec::new();
        for slot in [1u64, 2, 3, 10] {
            emitted.extend(w.observe(slot));
        }
        // Slot 10 lifts the threshold to 8, flushing every earlier observed slot
        // in ascending order. The absent slots (4..=9) never appear.
        assert_eq!(emitted, vec![1, 2, 3]);
    }

    #[test]
    fn emits_strictly_ascending() {
        let mut w = SlotCompletionWindow::new(2);
        let sequence = [7u64, 5, 6, 9, 8, 12, 4, 20];
        let mut all = Vec::new();
        for slot in sequence {
            let ready = w.observe(slot);
            // Each individual return is itself ascending.
            let mut sorted = ready.clone();
            sorted.sort_unstable();
            assert_eq!(ready, sorted, "each return must be ascending");
            all.extend(ready);
        }
        // The global emission order is strictly ascending (frontier-friendly, no
        // slot released twice, none out of order).
        for pair in all.windows(2) {
            assert!(pair[0] < pair[1], "global order must be strictly ascending");
        }
    }
}
