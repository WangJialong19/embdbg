//! The hex view: raw bytes, both directions, in the classic offset / hex / ascii layout.

use std::time::{Duration, Instant};

use egui::{Label, RichText, ScrollArea, Ui};
use embdbg_core::pipeline::{Direction, SessionShared};

use crate::theme;
use crate::util::format_clock;

/// Bytes per row of the hex dump.
const BYTES_PER_ROW: usize = 16;
/// How much of the newest traffic the view keeps. Older chunks are left out.
const KEEP_BYTES: usize = 256 * 1024;
/// How often the view is rebuilt at most; streaming data must not rebuild it every frame.
const REBUILD_INTERVAL: Duration = Duration::from_millis(200);
const ROW_HEIGHT: f32 = 16.0;

#[derive(Debug)]
enum Row {
    /// A chunk's time, direction and length.
    Header(String),
    /// One line of hex bytes, with the offset they start at.
    Bytes { offset: u64, bytes: Vec<u8> },
}

#[derive(Debug, Default)]
pub struct HexState {
    follow: bool,
    rows: Vec<Row>,
    /// What the rows were built from: chunk count and byte count.
    built_for: (usize, usize),
    built_at: Option<Instant>,
}

impl HexState {
    /// Rebuilds the rows when there is new traffic, at most every [`REBUILD_INTERVAL`].
    fn refresh(&mut self, shared: &SessionShared, force: bool) {
        let (chunks, bytes) = {
            let log = shared.bytes.read();
            (log.chunks().count(), log.bytes())
        };
        if !force && self.built_for == (chunks, bytes) {
            return;
        }
        if !force
            && self
                .built_at
                .is_some_and(|at| at.elapsed() < REBUILD_INTERVAL)
        {
            return;
        }

        self.rows.clear();
        self.built_for = (chunks, bytes);
        self.built_at = Some(Instant::now());

        let log = shared.bytes.read();
        // Walk backwards to find where the kept window begins, then build forwards.
        let mut kept = 0usize;
        let mut first = log.len();
        for index in (0..log.len()).rev() {
            let Some(chunk) = log.get(index) else { break };
            kept += chunk.bytes.len();
            first = index;
            if kept >= KEEP_BYTES {
                break;
            }
        }
        let mut offset = 0u64;
        for index in 0..log.len() {
            let Some(chunk) = log.get(index) else { break };
            if index < first {
                offset += chunk.bytes.len() as u64;
                continue;
            }
            let direction = match chunk.direction {
                Direction::RemoteToHere => "收",
                Direction::HereToRemote => "发",
            };
            self.rows.push(Row::Header(format!(
                "{}  {direction}  {} 字节",
                format_clock(chunk.t),
                chunk.bytes.len()
            )));
            for (row, bytes) in chunk.bytes.chunks(BYTES_PER_ROW).enumerate() {
                self.rows.push(Row::Bytes {
                    offset: offset + (row * BYTES_PER_ROW) as u64,
                    bytes: bytes.to_vec(),
                });
            }
            offset += chunk.bytes.len() as u64;
        }
    }
}

pub fn ui(ui: &mut Ui, shared: &SessionShared, state: &mut HexState) {
    state.refresh(shared, false);

    ui.horizontal(|ui| {
        if ui.checkbox(&mut state.follow, "跟随最新").changed() {
            state.refresh(shared, true);
        }
        ui.label(
            RichText::new(format!("共 {} 字节", shared.bytes.read().bytes()))
                .small()
                .color(theme::FG_DIM),
        );
        if state.rows.is_empty() {
            ui.label(
                RichText::new("（收到的和发出的原始字节都会出现在这里）")
                    .small()
                    .color(theme::FG_DIM),
            );
        }
    });

    ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(state.follow)
        .show_rows(ui, ROW_HEIGHT, state.rows.len(), |ui, range| {
            for row in &state.rows[range] {
                match row {
                    Row::Header(text) => {
                        ui.label(RichText::new(text).small().color(theme::ACCENT));
                    }
                    Row::Bytes { offset, bytes } => {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{offset:08X}"))
                                    .monospace()
                                    .small()
                                    .color(theme::FG_DIM),
                            );
                            let mut hex = String::new();
                            for index in 0..BYTES_PER_ROW {
                                hex.push_str(
                                    bytes
                                        .get(index)
                                        .map_or("  ", |byte| HEX_DIGITS[*byte as usize]),
                                );
                                hex.push(' ');
                            }
                            ui.label(RichText::new(hex).monospace().small().color(theme::FG));
                            let ascii: String = bytes
                                .iter()
                                .map(|byte| {
                                    if byte.is_ascii_graphic() || *byte == b' ' {
                                        *byte as char
                                    } else {
                                        '.'
                                    }
                                })
                                .collect();
                            ui.add(Label::new(RichText::new(ascii).monospace().small()).truncate());
                        });
                    }
                }
            }
        });
}

/// Every byte as two hex digits, so a row of them can be built without formatting each one.
const HEX_DIGITS: [&str; 256] = [
    "00", "01", "02", "03", "04", "05", "06", "07", "08", "09", "0A", "0B", "0C", "0D", "0E", "0F",
    "10", "11", "12", "13", "14", "15", "16", "17", "18", "19", "1A", "1B", "1C", "1D", "1E", "1F",
    "20", "21", "22", "23", "24", "25", "26", "27", "28", "29", "2A", "2B", "2C", "2D", "2E", "2F",
    "30", "31", "32", "33", "34", "35", "36", "37", "38", "39", "3A", "3B", "3C", "3D", "3E", "3F",
    "40", "41", "42", "43", "44", "45", "46", "47", "48", "49", "4A", "4B", "4C", "4D", "4E", "4F",
    "50", "51", "52", "53", "54", "55", "56", "57", "58", "59", "5A", "5B", "5C", "5D", "5E", "5F",
    "60", "61", "62", "63", "64", "65", "66", "67", "68", "69", "6A", "6B", "6C", "6D", "6E", "6F",
    "70", "71", "72", "73", "74", "75", "76", "77", "78", "79", "7A", "7B", "7C", "7D", "7E", "7F",
    "80", "81", "82", "83", "84", "85", "86", "87", "88", "89", "8A", "8B", "8C", "8D", "8E", "8F",
    "90", "91", "92", "93", "94", "95", "96", "97", "98", "99", "9A", "9B", "9C", "9D", "9E", "9F",
    "A0", "A1", "A2", "A3", "A4", "A5", "A6", "A7", "A8", "A9", "AA", "AB", "AC", "AD", "AE", "AF",
    "B0", "B1", "B2", "B3", "B4", "B5", "B6", "B7", "B8", "B9", "BA", "BB", "BC", "BD", "BE", "BF",
    "C0", "C1", "C2", "C3", "C4", "C5", "C6", "C7", "C8", "C9", "CA", "CB", "CC", "CD", "CE", "CF",
    "D0", "D1", "D2", "D3", "D4", "D5", "D6", "D7", "D8", "D9", "DA", "DB", "DC", "DD", "DE", "DF",
    "E0", "E1", "E2", "E3", "E4", "E5", "E6", "E7", "E8", "E9", "EA", "EB", "EC", "ED", "EE", "EF",
    "F0", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "FA", "FB", "FC", "FD", "FE", "FF",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hex_table_is_right_at_both_ends() {
        assert_eq!(HEX_DIGITS[0x00], "00");
        assert_eq!(HEX_DIGITS[0x7F], "7F");
        assert_eq!(HEX_DIGITS[0xFF], "FF");
        assert_eq!(HEX_DIGITS[0xA5], "A5");
    }
}
