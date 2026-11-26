//! A terminal screen: ANSI/VT100 sequences parsed into a grid of cells.
//!
//! This is what makes a serial console usable: colours, cursor movement, carriage returns that
//! overwrite a line, and full-screen programs — rather than a list of lines.

pub use vt100;

use std::num::NonZeroU16;

use vt100::Parser;

/// How many lines scroll back above the visible screen.
const SCROLLBACK_LINES: usize = 5_000;

/// How text bytes become characters on screen.
///
/// UTF-8 goes straight to the parser, which already understands it. The others are decoded first,
/// so a GBK console shows readable Chinese instead of mojibake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextEncoding {
    #[default]
    Utf8,
    Gbk,
    Gb18030,
    Latin1,
}

impl TextEncoding {
    pub const ALL: [Self; 4] = [Self::Utf8, Self::Gbk, Self::Gb18030, Self::Latin1];

    pub fn id(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Gbk => "gbk",
            Self::Gb18030 => "gb18030",
            Self::Latin1 => "latin1",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "UTF-8",
            Self::Gbk => "GBK",
            Self::Gb18030 => "GB18030",
            Self::Latin1 => "Latin-1",
        }
    }

    /// Reads back an id written by [`Self::id`], for the saved settings.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|encoding| encoding.id() == id)
    }

    /// The codec behind a non-UTF-8 encoding; UTF-8 needs none.
    fn codec(self) -> Option<&'static encoding_rs::Encoding> {
        match self {
            Self::Utf8 => None,
            Self::Gbk => Some(encoding_rs::GBK),
            Self::Gb18030 => Some(encoding_rs::GB18030),
            Self::Latin1 => Some(encoding_rs::WINDOWS_1252),
        }
    }
}

/// A terminal emulator sized in character cells.
pub struct Terminal {
    parser: Parser,
    rows: u16,
    cols: u16,
    /// How far back into the scrollback the view is; 0 is the live screen.
    offset: usize,
    /// How received bytes become text.
    encoding: TextEncoding,
    /// The incremental decoder of a non-UTF-8 encoding.
    decoder: Option<encoding_rs::Decoder>,
    /// Bytes of a character the last read ended in the middle of.
    pending: Vec<u8>,
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal")
            .field("rows", &self.rows)
            .field("cols", &self.cols)
            .field("offset", &self.offset)
            .finish_non_exhaustive()
    }
}

impl Terminal {
    pub fn new(rows: u16, cols: u16) -> Self {
        let (rows, cols) = (rows.max(2), cols.max(2));
        Self {
            parser: Parser::new(nonzero(rows), nonzero(cols), SCROLLBACK_LINES),
            rows,
            cols,
            offset: 0,
            encoding: TextEncoding::default(),
            decoder: None,
            pending: Vec::new(),
        }
    }

    /// Feeds received bytes. Escape sequences, line breaks and carriage returns take effect.
    ///
    /// Output does not disturb a view scrolled back into history: reading older lines while the
    /// device keeps logging is the point of having history.
    pub fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let Some(codec) = self.encoding.codec() else {
            self.parser.process(bytes);
            return;
        };
        // A read can end in the middle of a multi-byte character; keep the tail for next time.
        self.pending.extend_from_slice(bytes);
        let decoder = self
            .decoder
            .get_or_insert_with(|| codec.new_decoder_without_bom_handling());
        let mut offset = 0;
        while offset < self.pending.len() {
            let mut text = String::with_capacity((self.pending.len() - offset) * 2 + 8);
            let (result, read, _) =
                decoder.decode_to_string(&self.pending[offset..], &mut text, false);
            if read == 0 {
                break;
            }
            offset += read;
            self.parser.process(text.as_bytes());
            if matches!(result, encoding_rs::CoderResult::InputEmpty) {
                break;
            }
        }
        self.pending.drain(..offset);
    }

    /// Switches how received bytes become text. The screen starts over: what was shown was
    /// decoded the old way.
    pub fn set_encoding(&mut self, encoding: TextEncoding) {
        if encoding == self.encoding {
            return;
        }
        self.encoding = encoding;
        self.decoder = None;
        self.pending.clear();
        self.clear();
    }

    pub fn encoding(&self) -> TextEncoding {
        self.encoding
    }

    /// Changes the screen size; the widget calls this when its own size changes.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(2), cols.max(2));
        if (rows, cols) != (self.rows, self.cols) {
            self.rows = rows;
            self.cols = cols;
            self.parser
                .screen_mut()
                .set_size(nonzero(rows), nonzero(cols));
        }
    }

    /// Empties the screen and the scrollback, keeping the size. The view stays live.
    pub fn clear(&mut self) {
        self.parser = Parser::new(nonzero(self.rows), nonzero(self.cols), SCROLLBACK_LINES);
        self.offset = 0;
        self.pending.clear();
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn cols(&self) -> u16 {
        self.cols
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    pub fn cell(&self, row: u16, col: u16) -> Option<&vt100::Cell> {
        self.parser.screen().cell(row, col)
    }

    /// Row and column of the cursor.
    pub fn cursor(&self) -> (u16, u16) {
        self.parser.screen().cursor_position()
    }

    pub fn hide_cursor(&self) -> bool {
        self.parser.screen().hide_cursor()
    }

    pub fn alternate_screen(&self) -> bool {
        self.parser.screen().alternate_screen()
    }

    /// Whether the view shows the live screen rather than history.
    pub fn is_live(&self) -> bool {
        self.offset == 0
    }

    /// How far back the view is, in lines.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// How many lines the scrollback holds. It grows as output scrolls the screen; a view can
    /// stay on the same lines by scrolling back as much as this grew.
    pub fn scrollback_len(&self) -> usize {
        self.parser.screen().scrollback_len()
    }

    /// Scrolls back through history by at most `lines`; returns the new offset.
    pub fn scroll_back(&mut self, lines: usize) -> usize {
        self.offset = self.offset.saturating_add(lines);
        self.parser.screen_mut().set_scrollback(self.offset);
        // The parser clamps to what it actually kept.
        self.offset = self.parser.screen().scrollback();
        self.offset
    }

    /// Scrolls forward towards the live screen; returns the new offset.
    pub fn scroll_forward(&mut self, lines: usize) -> usize {
        self.offset = self.offset.saturating_sub(lines);
        self.parser.screen_mut().set_scrollback(self.offset);
        self.offset = self.parser.screen().scrollback();
        self.offset
    }

    pub fn scroll_to_live(&mut self) {
        self.offset = 0;
        self.parser.screen_mut().set_scrollback(0);
    }

    /// The visible text, one line per row: for MCP and for copying.
    pub fn contents(&self) -> String {
        self.parser.screen().contents()
    }
}

/// The parser wants non-zero sizes; a terminal is never empty.
fn nonzero(value: u16) -> NonZeroU16 {
    NonZeroU16::new(value.max(1)).expect("clamped to at least one")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(terminal: &Terminal) -> Vec<String> {
        terminal
            .contents()
            .lines()
            .map(|line| line.trim_end().to_owned())
            .collect()
    }

    #[test]
    fn plain_text_lands_on_consecutive_lines() {
        let mut terminal = Terminal::new(4, 20);
        terminal.feed(b"hello\r\nworld\r\n");
        let lines = text_of(&terminal);
        assert_eq!(lines[0], "hello");
        assert_eq!(lines[1], "world");
        assert_eq!(terminal.cursor(), (2, 0));
    }

    #[test]
    fn a_carriage_return_overwrites_the_line() {
        let mut terminal = Terminal::new(4, 20);
        // The pattern of a progress bar: print, return, print again.
        terminal.feed(b"progress 10%\rprogress 90%");
        let lines = text_of(&terminal);
        assert_eq!(lines[0], "progress 90%");
    }

    #[test]
    fn colours_and_attributes_reach_the_cells() {
        let mut terminal = Terminal::new(4, 20);
        terminal.feed(b"\x1b[31mR\x1b[0m\x1b[1mB\x1b[0m");
        let red = terminal.cell(0, 0).unwrap();
        assert_eq!(red.fgcolor(), vt100::Color::Idx(1));
        let bold = terminal.cell(0, 1).unwrap();
        assert!(bold.bold());
    }

    #[test]
    fn the_cursor_can_be_moved_and_the_screen_cleared() {
        let mut terminal = Terminal::new(4, 20);
        terminal.feed(b"abc\x1b[2;5HX");
        assert_eq!(terminal.cursor(), (1, 5));
        let lines = text_of(&terminal);
        assert_eq!(lines[1].trim_end(), "    X");
        terminal.feed(b"\x1b[2J");
        assert!(text_of(&terminal).iter().all(String::is_empty));
    }

    #[test]
    fn scrolling_and_resizing_work() {
        let mut terminal = Terminal::new(3, 10);
        terminal.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        // The screen shows the last three lines, the cursor sitting at the end of the last one.
        let lines = text_of(&terminal);
        assert_eq!(lines, ["three", "four", "five"]);
        assert_eq!(terminal.cursor(), (2, 4));
        // Scrolling back shows history and takes the view off the live screen.
        assert_eq!(terminal.scroll_back(2), 2);
        assert!(!terminal.is_live());
        assert_eq!(text_of(&terminal)[2], "three");
        terminal.scroll_to_live();
        assert!(terminal.is_live());
        // New output leaves a view scrolled back where it is.
        terminal.scroll_back(5);
        terminal.feed(b"\r\nsix\r\n");
        assert!(!terminal.is_live());
        // A resize keeps the content.
        terminal.scroll_to_live();
        terminal.resize(5, 20);
        assert_eq!((terminal.rows(), terminal.cols()), (5, 20));
        assert!(text_of(&terminal).iter().any(|line| line == "six"));
    }

    #[test]
    fn unicode_and_wide_characters_survive() {
        let mut terminal = Terminal::new(4, 20);
        terminal.feed("中文 ok\r\n".as_bytes());
        let lines = text_of(&terminal);
        assert!(lines[0].starts_with("中文 ok"), "{lines:?}");
    }

    #[test]
    fn gbk_text_is_decoded_before_it_reaches_the_cells() {
        let mut terminal = Terminal::new(4, 20);
        terminal.set_encoding(TextEncoding::Gbk);
        // "中文" in GBK.
        terminal.feed(&[0xd6, 0xd0, 0xce, 0xc4]);
        assert!(
            text_of(&terminal)[0].starts_with("中文"),
            "{:?}",
            text_of(&terminal)
        );
    }

    #[test]
    fn a_split_character_waits_for_its_other_half() {
        let mut terminal = Terminal::new(4, 20);
        terminal.set_encoding(TextEncoding::Gbk);
        terminal.feed(&[0xd6]);
        assert!(terminal.contents().trim().is_empty());
        terminal.feed(&[0xd0]);
        assert_eq!(text_of(&terminal)[0], "中");
    }

    #[test]
    fn switching_the_encoding_starts_the_screen_over() {
        let mut terminal = Terminal::new(4, 20);
        terminal.feed(b"old");
        assert_eq!(text_of(&terminal)[0], "old");
        terminal.set_encoding(TextEncoding::Gbk);
        assert!(terminal.contents().trim().is_empty());
        assert_eq!(terminal.encoding(), TextEncoding::Gbk);
    }
}
