//! Talking in writing: the assistant's words shown on the device (`text.display`), and what a person
//! types sent with [`Observer::input_text`](crate::Observer::input_text) (`text.input`).

/// Where the assistant's words are shown (a screen, a terminal): the device declares `text.display`.
///
/// An answer may come in several messages (the assistant says it is looking something up, then gives
/// the result), in order; `last` marks the last one of a turn. An unprompted message (a reminder) is
/// one message, `last` set. Text is plain: no Markdown. Calls come from the network task: show the
/// text and return at once. A closure `Fn(&str, bool)` is a text display.
pub trait TextDisplay: Send + Sync + 'static {
    /// Show `text`; `last` when it ends the assistant's turn.
    fn show_text(&self, text: &str, last: bool);
}

impl<F: Fn(&str, bool) + Send + Sync + 'static> TextDisplay for F {
    fn show_text(&self, text: &str, last: bool) {
        self(text, last);
    }
}
