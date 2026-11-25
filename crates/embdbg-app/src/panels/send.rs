//! The send panel: text or hex, with an optional line ending and repetition.

use std::time::{Duration, Instant};

use egui::{Button, ComboBox, DragValue, RichText, ScrollArea, TextEdit, Ui};
use embdbg_core::engine::{Connection, Engine};

use crate::theme::{self, FG_DIM, SPACE_1, SPACE_2};

/// What is appended to text sends. Hex sends exactly the bytes typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineEnding {
    None,
    Cr,
    Lf,
    CrLf,
}

impl LineEnding {
    const ALL: [Self; 4] = [Self::None, Self::Cr, Self::Lf, Self::CrLf];

    fn label(self) -> &'static str {
        match self {
            Self::None => "无",
            Self::Cr => "CR",
            Self::Lf => "LF",
            Self::CrLf => "CRLF",
        }
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            Self::None => b"",
            Self::Cr => b"\r",
            Self::Lf => b"\n",
            Self::CrLf => b"\r\n",
        }
    }
}

#[derive(Debug)]
pub struct SendState {
    pub text: String,
    pub hex: bool,
    ending: LineEnding,
    repeat: bool,
    interval_ms: u32,
    last_sent: Option<Instant>,
    /// The last few payloads, newest last.
    history: Vec<String>,
}

impl Default for SendState {
    fn default() -> Self {
        Self {
            text: String::new(),
            hex: false,
            // Most serial protocols want a line break; AT commands are one example.
            ending: LineEnding::CrLf,
            repeat: false,
            interval_ms: 1000,
            last_sent: None,
            history: Vec::new(),
        }
    }
}

/// Draws the send panel. Returns a message for the log if sending failed.
pub fn ui(
    ui: &mut Ui,
    engine: &Engine,
    connection: Option<&Connection>,
    state: &mut SendState,
) -> Option<String> {
    let Some(connection) = connection else {
        ui.label(
            RichText::new("先打开一个连接，并在「连接」里选中它，再从这里发送。").color(FG_DIM),
        );
        return None;
    };

    let mut send = false;
    ui.horizontal(|ui| {
        ui.selectable_value(&mut state.hex, false, "文本");
        ui.selectable_value(&mut state.hex, true, "HEX");
        ComboBox::from_id_salt("send-ending")
            .selected_text(format!("换行：{}", state.ending.label()))
            .show_ui(ui, |ui| {
                for ending in LineEnding::ALL {
                    ui.selectable_value(
                        &mut state.ending,
                        ending,
                        format!("换行：{}", ending.label()),
                    );
                }
            });
        if ui
            .add(Button::new(RichText::new("发送").strong()))
            .clicked()
        {
            send = true;
        }
        ui.checkbox(&mut state.repeat, "定时发送");
        ui.add_enabled(
            state.repeat,
            DragValue::new(&mut state.interval_ms)
                .range(10..=60_000)
                .suffix(" ms"),
        );
        if !state.history.is_empty() {
            ComboBox::from_id_salt("send-history")
                .selected_text("历史")
                .show_ui(ui, |ui| {
                    for entry in state.history.iter().rev().take(20) {
                        let single_line = entry.replace(['\r', '\n'], " ");
                        if ui.selectable_label(false, single_line).clicked() {
                            state.text = entry.clone();
                        }
                    }
                });
        }
    });

    ui.add_space(SPACE_1);
    ScrollArea::vertical()
        .max_height(120.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.add(
                TextEdit::multiline(&mut state.text)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .desired_rows(3)
                    .hint_text(if state.hex { "01 02 0A FF" } else { "AT" }),
            );
        });

    // The timer and the button share one path, so the recorded history and timing stay honest.
    let due = state.repeat
        && state
            .last_sent
            .is_none_or(|at| at.elapsed() >= Duration::from_millis(u64::from(state.interval_ms)));
    if send || due {
        let result = payload(state).and_then(|bytes| engine.send(connection.id, &bytes));
        match result {
            Ok(()) => {
                state.last_sent = Some(Instant::now());
                remember(&mut state.history, &state.text);
                return None;
            }
            Err(error) => return Some(format!("发送失败：{error}")),
        }
    }

    ui.add_space(SPACE_2);
    ui.label(
        RichText::new(format!(
            "发送到：{} {}",
            connection.config.kind(),
            connection.config.describe()
        ))
        .small()
        .color(theme::FG_DIM),
    );
    None
}

/// The bytes the current input stands for.
fn payload(state: &SendState) -> Result<Vec<u8>, String> {
    if state.hex {
        parse_hex(&state.text)
    } else {
        let mut bytes = state.text.as_bytes().to_vec();
        bytes.extend_from_slice(state.ending.bytes());
        Ok(bytes)
    }
}

/// Parses `01 02 0A`, `0x01,0x02` or `01020A` alike.
fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = text
        .replace("0x", "")
        .replace("0X", "")
        .chars()
        .filter(|character| !character.is_whitespace() && *character != ',' && *character != ':')
        .collect();
    if cleaned.is_empty() {
        return Ok(Vec::new());
    }
    if !cleaned.len().is_multiple_of(2) {
        return Err(format!("HEX 输入有 {} 个字符，凑不成整字节", cleaned.len()));
    }
    (0..cleaned.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&cleaned[index..index + 2], 16)
                .map_err(|_| format!("不是有效的 HEX：{}", &cleaned[index..index + 2]))
        })
        .collect()
}

/// Adds a payload to the history, newest last, without repeats.
fn remember(history: &mut Vec<String>, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    history.retain(|entry| entry != text);
    history.push(text.to_owned());
    if history.len() > 50 {
        history.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_input_accepts_the_usual_spellings() {
        assert_eq!(parse_hex("01 02 0A"), Ok(vec![0x01, 0x02, 0x0A]));
        assert_eq!(parse_hex("0x01,0x02"), Ok(vec![0x01, 0x02]));
        assert_eq!(parse_hex("01020a"), Ok(vec![0x01, 0x02, 0x0A]));
        assert_eq!(parse_hex("01:02"), Ok(vec![0x01, 0x02]));
        assert_eq!(parse_hex("  "), Ok(Vec::new()));
    }

    #[test]
    fn bad_hex_is_refused_with_a_reason() {
        let error = parse_hex("012").unwrap_err();
        assert!(error.contains("凑不成整字节"), "{error}");
        let error = parse_hex("01 ZZ").unwrap_err();
        assert!(error.contains("ZZ"), "{error}");
    }

    #[test]
    fn text_sends_carry_the_chosen_line_ending() {
        let mut state = SendState {
            text: "AT".to_owned(),
            ..SendState::default()
        };
        assert_eq!(payload(&state), Ok(b"AT\r\n".to_vec()));
        state.ending = LineEnding::None;
        assert_eq!(payload(&state), Ok(b"AT".to_vec()));
        state.hex = true;
        state.text = "41 54".to_owned();
        // Hex sends exactly what was typed, without an added line break.
        assert_eq!(payload(&state), Ok(b"AT".to_vec()));
    }

    #[test]
    fn the_history_keeps_recent_entries_without_duplicates() {
        let mut history = Vec::new();
        remember(&mut history, "A");
        remember(&mut history, "B");
        remember(&mut history, "A");
        assert_eq!(history, ["B", "A"]);
        remember(&mut history, "  ");
        assert_eq!(history, ["B", "A"]);
    }
}
