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
}

impl SlotCompletionWindow {
    pub(crate) fn new(window: u64) -> Self {
        Self {
            window,
            high_meta: 0,
            pending: BTreeSet::new(),
        }
    }

    /// Record a BlockMeta for `slot` and return the slots now eligible for
    /// SlotComplete, ascending. Every slot is released once it falls `window`
    /// behind the tip; a slot re-observed after release re-emits an idempotent
    /// SlotComplete rather than being dropped, so none is ever stranded.
    pub(crate) fn observe(&mut self, slot: u64) -> Vec<u64> {
        self.high_meta = self.high_meta.max(slot);
        self.pending.insert(slot);

        let threshold = self.high_meta.saturating_sub(self.window);
        let mut ready = Vec::new();
        while let Some(&s) = self.pending.first() {
            if s <= threshold {
                self.pending.pop_first();
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
    fn duplicate_meta_while_pending_dedups() {
        // A BlockMeta redelivered while its slot is still held releases the slot once.
        let mut w = SlotCompletionWindow::new(2);
        assert!(w.observe(4).is_empty());
        assert!(w.observe(4).is_empty());
        assert_eq!(w.observe(6), vec![4]);
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
    fn every_observed_slot_is_eventually_emitted() {
        // A lower slot arriving after higher ones were already released must still
        // be emitted, not dropped; its buffered transactions depend on it.
        let mut w = SlotCompletionWindow::new(2);
        let observed = [7u64, 5, 6, 9, 8, 12, 4, 20];
        let mut all = Vec::new();
        for slot in observed {
            let ready = w.observe(slot);
            // Each individual return is itself ascending.
            let mut sorted = ready.clone();
            sorted.sort_unstable();
            assert_eq!(ready, sorted, "each return must be ascending");
            all.extend(ready);
        }
        all.sort_unstable();

        // No slot is released more than once.
        let mut deduped = all.clone();
        deduped.dedup();
        assert_eq!(all, deduped, "no slot released twice");

        // Every slot at least `window` behind the highest observed is released.
        let high = *observed.iter().max().unwrap();
        let mut expected: Vec<u64> = observed
            .iter()
            .copied()
            .filter(|s| *s <= high - 2)
            .collect();
        expected.sort_unstable();
        assert_eq!(
            all, expected,
            "every eligible observed slot is emitted exactly once"
        );
    }
}
