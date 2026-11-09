//! The text an input method is composing, for views that take typed text through GPUI's
//! input handler but keep no editable text of their own (the terminal, the remote desktop).
//!
//! GPUI hands such a view everything the platform's text input produces: plain typing, a
//! dead key's accent while it waits for the next key (`´` before `e` makes `é`), an IME's
//! reading while it is being converted. The input method sees only the composition, with
//! the caret at its end; committed text goes on to the view's session and leaves nothing
//! behind. Ranges are in UTF-16 code units, as GPUI's input handler counts them.

use std::ops::Range;

use gpui_kit::UTF16Selection;

/// The composition in progress, if any.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Composition {
    text: Option<String>,
}

impl Composition {
    /// The text being composed.
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// Compose `text` (`replace_and_mark_text_in_range`); empty text ends the composition.
    pub fn set(&mut self, text: &str) {
        self.text = (!text.is_empty()).then(|| text.to_owned());
    }

    /// End the composition (`unmark_text`, or text was committed).
    pub fn clear(&mut self) {
        self.text = None;
    }

    /// `text_for_range`: part of the composition, with the range clamped to it.
    pub fn text_for_range(
        &self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
    ) -> Option<String> {
        let units: Vec<u16> = self.text.as_ref()?.encode_utf16().collect();
        let start = range.start.min(units.len());
        let end = range.end.clamp(start, units.len());
        *adjusted = Some(start..end);
        Some(String::from_utf16_lossy(&units[start..end]))
    }

    /// `selected_text_range`: the caret, at the end of the composition.
    pub fn selection(&self) -> UTF16Selection {
        let end = self
            .text
            .as_ref()
            .map_or(0, |text| text.encode_utf16().count());
        UTF16Selection {
            range: end..end,
            reversed: false,
        }
    }

    /// `marked_text_range`: all of the composition.
    pub fn marked_range(&self) -> Option<Range<usize>> {
        self.text
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_the_composition_to_the_input_method() {
        let mut composition = Composition::default();
        assert_eq!(composition.marked_range(), None);
        assert_eq!(composition.selection().range, 0..0);
        let mut adjusted = None;
        assert_eq!(composition.text_for_range(0..1, &mut adjusted), None);

        composition.set("´");
        assert_eq!(composition.text(), Some("´"));
        assert_eq!(composition.marked_range(), Some(0..1));
        // UTF-16 units: the emoji counts two.
        composition.set("に😀");
        assert_eq!(composition.marked_range(), Some(0..3));
        assert_eq!(composition.selection().range, 3..3);
        assert_eq!(
            composition.text_for_range(1..9, &mut adjusted).as_deref(),
            Some("😀")
        );
        assert_eq!(adjusted, Some(1..3));

        composition.set("");
        assert_eq!(composition.text(), None);
        composition.set("ほ");
        composition.clear();
        assert_eq!(composition.marked_range(), None);
    }
}
