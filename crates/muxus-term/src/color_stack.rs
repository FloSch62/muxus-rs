//! xterm's color palette stack: XTPUSHCOLORS (`CSI Pm # P`), XTPOPCOLORS (`CSI Pm # Q`) and
//! XTREPORTCOLORS (`CSI # R`). An entry holds the colors programs set on top of the theme (OSC 4
//! palette and special colors, OSC 10/11/12), so popping one brings back what was in effect at
//! the push, the theme's own colors included.
//!
//! The slots and counters follow xterm's `xtermPushColors` / `xtermPopColors` (misc.c): ten
//! slots; a push without a parameter stores into the slot past the top and raises the count, a
//! pop without one restores the top slot and lowers it; `Pm` names a slot (1 to 10) to store
//! into or restore from, and leaves the count at that slot (a store at slot `n` makes it `n`, a
//! restore makes it `n − 1`). Slots keep their colors after a pop. The report is
//! `CSI ? count ; most # Q`, where `most` is the highest count unnumbered pushes reached.
//! A parameter of 0 is the default, as xterm's documentation has it.

use crate::style::ColorOverrides;

/// Slots on the stack, as in xterm.
pub const MAX_SAVED_COLORS: usize = 10;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColorStack {
    slots: [Option<ColorOverrides>; MAX_SAVED_COLORS],
    /// Entries in use (xterm's `used`).
    used: usize,
    /// The most entries unnumbered pushes reached (xterm's `last`).
    last: usize,
}

impl ColorStack {
    /// XTPUSHCOLORS with one parameter (0 when it has none): store `colors` past the top, or
    /// into slot `slot`.
    pub fn push(&mut self, slot: u16, colors: &ColorOverrides) {
        let index = if slot == 0 {
            self.used
        } else {
            usize::from(slot) - 1
        };
        if index >= MAX_SAVED_COLORS {
            return;
        }
        self.slots[index] = Some(colors.clone());
        if slot == 0 {
            self.used += 1;
            self.last = self.last.max(self.used);
        } else {
            self.used = usize::from(slot);
        }
    }

    /// XTPOPCOLORS with one parameter (0 when it has none): the colors of the top slot, or of
    /// slot `slot`; `None` when that slot was never stored.
    pub fn pop(&mut self, slot: u16) -> Option<ColorOverrides> {
        let index = if slot == 0 {
            self.used.checked_sub(1)?
        } else {
            usize::from(slot) - 1
        };
        let colors = self.slots.get(index)?.clone()?;
        self.used = index;
        Some(colors)
    }

    /// The XTREPORTCOLORS reply.
    pub fn report(&self) -> String {
        format!("\x1b[?{};{}#Q", self.used, self.last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::Rgba;

    fn colors(red: u8) -> ColorOverrides {
        ColorOverrides {
            foreground: Some(Rgba::rgb(red, 0, 0)),
            ..ColorOverrides::default()
        }
    }

    #[test]
    fn pushes_and_pops_in_order() {
        let mut stack = ColorStack::default();
        assert_eq!(stack.report(), "\x1b[?0;0#Q");
        assert_eq!(stack.pop(0), None);
        stack.push(0, &colors(1));
        stack.push(0, &colors(2));
        assert_eq!(stack.report(), "\x1b[?2;2#Q");
        assert_eq!(stack.pop(0), Some(colors(2)));
        assert_eq!(stack.report(), "\x1b[?1;2#Q");
        assert_eq!(stack.pop(0), Some(colors(1)));
        assert_eq!(stack.pop(0), None);
        assert_eq!(stack.report(), "\x1b[?0;2#Q");
    }

    #[test]
    fn holds_ten_entries() {
        let mut stack = ColorStack::default();
        for red in 1..=12 {
            stack.push(0, &colors(red));
        }
        assert_eq!(stack.report(), "\x1b[?10;10#Q");
        // The pushes past the tenth were dropped.
        assert_eq!(stack.pop(0), Some(colors(10)));
        stack.push(11, &colors(11));
        assert_eq!(stack.report(), "\x1b[?9;10#Q");
    }

    #[test]
    fn numbered_slots_set_the_count_as_xterm_does() {
        let mut stack = ColorStack::default();
        // Storing into slot 3 makes the count 3 without padding slots 1 and 2.
        stack.push(3, &colors(3));
        assert_eq!(stack.report(), "\x1b[?3;0#Q");
        assert_eq!(stack.pop(2), None, "slot 2 was never stored");
        assert_eq!(stack.report(), "\x1b[?3;0#Q");
        // Restoring slot 3 leaves the count below it; the slot keeps its colors.
        assert_eq!(stack.pop(3), Some(colors(3)));
        assert_eq!(stack.report(), "\x1b[?2;0#Q");
        assert_eq!(stack.pop(3), Some(colors(3)));
        // An unnumbered push goes past the count, into slot 3 here.
        stack.push(0, &colors(4));
        assert_eq!(stack.report(), "\x1b[?3;3#Q");
        assert_eq!(stack.pop(3), Some(colors(4)));
        // Slots past ten are ignored.
        stack.push(11, &colors(11));
        assert_eq!(stack.pop(11), None);
    }
}
