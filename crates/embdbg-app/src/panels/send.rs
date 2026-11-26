//! The send panel: text or hex, with an optional line ending and repetition.

use std::time::{Duration, Instant};

use egui::{Button, ComboBox, DragValue, RichText, ScrollArea, TextEdit, Ui};
use embdbg_core::engine::{Connection, ConnectionState, Engine};

use crate::settings::SendMacro;
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

/// The outcome of the last attempt, shown under the editor for a few seconds.
#[derive(Debug)]
struct LastSend {
    at: Instant,
    ok: bool,
    text: String,
}

#[derive(Debug)]
pub struct SendState {
    pub text: String,
    pub hex: bool,
    ending: LineEnding,
    repeat: bool,
    interval_ms: u32,
    last_sent: Option<Instant>,
    last: Option<LastSend>,
    /// The last few payloads, newest last.
    history: Vec<String>,
    /// Saved messages, one click away.
    pub macros: Vec<SendMacro>,
    /// The name typed for the next macro.
    macro_name: String,
    /// Set when the macro list changed, so the app can write it to the settings.
    macros_changed: bool,
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
            last: None,
            history: Vec::new(),
            macros: Vec::new(),
            macro_name: String::new(),
            macros_changed: false,
        }
    }
}

impl SendState {
    /// The panel with the macros that were saved last time.
    pub fn with_macros(macros: Vec<SendMacro>) -> Self {
        Self {
            macros,
            ..SendState::default()
        }
    }

    /// Whether the macro list changed since the last save; the flag is cleared.
    pub fn take_macros_changed(&mut self) -> bool {
        std::mem::take(&mut self.macros_changed)
    }
}

/// Draws the send panel. Returns a message for the log if sending failed.
pub fn ui(ui: &mut Ui, engine: &Engine, state: &mut SendState) -> Option<String> {
    let connections = engine.connections();
    // The active connection follows the terminal tab the person picked; a failed one must not
    // swallow sends, so fall back to the first connection that can actually send.
    let usable = |connection: &&std::sync::Arc<Connection>| {
        matches!(
            connection.state(),
            ConnectionState::Open | ConnectionState::Connecting
        )
    };
    let target_id = engine
        .active_id()
        .filter(|id| connections.iter().any(|c| c.id == *id && usable(&c)))
        .or_else(|| connections.iter().find(usable).map(|c| c.id))
        .or_else(|| engine.active_id())
        .or_else(|| connections.first().map(|c| c.id));
    let Some(connection) = target_id
        .and_then(|id| connections.iter().find(|connection| connection.id == id))
        .cloned()
    else {
        ui.label(
            RichText::new("先打开一个连接，并在「连接」里选中它，再从这里发送。").color(FG_DIM),
        );
        return None;
    };

    if matches!(
        connection.config,
        embdbg_core::engine::TransportConfig::Modbus(_)
    ) {
        ui.label(
            RichText::new("所选连接是 Modbus，只读；无需发送，轮询结果直接进通道。")
                .small()
                .color(FG_DIM),
        );
        return None;
    }

    let mut log_error = None;
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
        .max_height(70.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.add(
                TextEdit::multiline(&mut state.text)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .desired_rows(2)
                    .hint_text(if state.hex { "01 02 0A FF" } else { "AT" }),
            );
        });

    // Macros: save what is in the editor under a name, then send it with one click.
    let mut send_macro: Option<SendMacro> = None;
    let mut remove_macro: Option<usize> = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new("宏").strong());
        ui.add(
            TextEdit::singleline(&mut state.macro_name)
                .desired_width(70.0)
                .hint_text("名称"),
        );
        if ui.small_button("存为宏").clicked() {
            save_macro(state);
        }
    });
    if !state.macros.is_empty() {
        ui.horizontal_wrapped(|ui| {
            for index in 0..state.macros.len() {
                let entry = state.macros[index].clone();
                if ui
                    .add(Button::new(RichText::new(&entry.name).small()))
                    .on_hover_text(&entry.text)
                    .clicked()
                {
                    send_macro = Some(entry);
                }
                if ui
                    .add(Button::new("×").small())
                    .on_hover_text("删除这个宏")
                    .clicked()
                {
                    remove_macro = Some(index);
                }
            }
        });
    }
    if let Some(index) = remove_macro {
        state.macros.remove(index);
        state.macros_changed = true;
    }

    // The timer and the button share one path, so the recorded history and timing stay honest.
    let due = state.repeat
        && state
            .last_sent
            .is_none_or(|at| at.elapsed() >= Duration::from_millis(u64::from(state.interval_ms)));

    if let Some(entry) = send_macro {
        let label = format!("发送宏「{}」", entry.name);
        match macro_payload(&entry).and_then(|bytes| {
            let len = bytes.len();
            engine.send(connection.id, &bytes).map(|()| len)
        }) {
            Ok(bytes) => {
                state.last = Some(LastSend {
                    at: Instant::now(),
                    ok: true,
                    text: format!("{label}：{bytes} 字节 → {}", target_label(&connection)),
                });
            }
            Err(error) => {
                let message = format!("{label}失败：{error}");
                state.last = Some(LastSend {
                    at: Instant::now(),
                    ok: false,
                    text: format!("{message} → {}", target_label(&connection)),
                });
                log_error = Some(message);
            }
        }
    } else if send || due {
        match payload(state).and_then(|bytes| {
            let len = bytes.len();
            engine.send(connection.id, &bytes).map(|()| len)
        }) {
            Ok(bytes) => {
                state.last_sent = Some(Instant::now());
                remember(&mut state.history, &state.text);
                state.last = Some(LastSend {
                    at: Instant::now(),
                    ok: true,
                    text: format!("已发送 {bytes} 字节 → {}", target_label(&connection)),
                });
            }
            Err(error) => {
                let message = format!("发送失败：{error}");
                state.last = Some(LastSend {
                    at: Instant::now(),
                    ok: false,
                    text: format!("{message} → {}", target_label(&connection)),
                });
                log_error = Some(message);
            }
        }
    }

    if let Some(last) = &state.last
        && last.at.elapsed() < Duration::from_secs(8)
    {
        ui.label(RichText::new(&last.text).small().color(if last.ok {
            theme::SUCCESS
        } else {
            theme::DANGER
        }));
    }
    ui.add_space(SPACE_2);
    ui.label(
        RichText::new(format!("发送到：{}", target_label(&connection)))
            .small()
            .color(FG_DIM),
    );
    log_error
}

/// The send target as one line: protocol, then the port or address.
fn target_label(connection: &Connection) -> String {
    format!(
        "{} {}",
        connection.config.kind(),
        connection.config.describe()
    )
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

/// Saves the editor's contents as a macro, replacing one with the same name. A macro with no
/// name takes the first characters of its text.
fn save_macro(state: &mut SendState) {
    if state.text.trim().is_empty() {
        return;
    }
    let name = if state.macro_name.trim().is_empty() {
        state.text.chars().take(12).collect()
    } else {
        state.macro_name.trim().to_owned()
    };
    let ending = match state.ending {
        LineEnding::None => "none",
        LineEnding::Cr => "cr",
        LineEnding::Lf => "lf",
        LineEnding::CrLf => "crlf",
    };
    let saved = SendMacro {
        name,
        text: state.text.clone(),
        hex: state.hex,
        ending: ending.to_owned(),
    };
    if let Some(existing) = state
        .macros
        .iter_mut()
        .find(|entry| entry.name == saved.name)
    {
        *existing = saved;
    } else {
        state.macros.push(saved);
    }
    state.macro_name.clear();
    state.macros_changed = true;
}

/// The bytes a saved macro stands for.
fn macro_payload(entry: &SendMacro) -> Result<Vec<u8>, String> {
    if entry.hex {
        return parse_hex(&entry.text);
    }
    let ending: &[u8] = match entry.ending.as_str() {
        "cr" => b"\r",
        "lf" => b"\n",
        "crlf" => b"\r\n",
        _ => b"",
    };
    let mut bytes = entry.text.as_bytes().to_vec();
    bytes.extend_from_slice(ending);
    Ok(bytes)
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

    #[test]
    fn a_macro_keeps_its_own_payload_and_ending() {
        let mut state = SendState {
            text: "AT".to_owned(),
            ..SendState::default()
        };
        state.ending = LineEnding::Lf;
        save_macro(&mut state);
        assert_eq!(state.macros.len(), 1);
        assert_eq!(state.macros[0].name, "AT");
        assert_eq!(macro_payload(&state.macros[0]), Ok(b"AT\n".to_vec()));
        assert!(state.take_macros_changed());
        assert!(!state.take_macros_changed());

        // A hex macro sends exactly the bytes typed, whatever the ending says.
        state.hex = true;
        state.text = "41 54".to_owned();
        save_macro(&mut state);
        assert_eq!(state.macros.len(), 2);
        assert_eq!(macro_payload(&state.macros[1]), Ok(b"AT".to_vec()));
    }

    #[test]
    fn saving_under_an_existing_name_replaces_it() {
        let mut state = SendState {
            text: "AT".to_owned(),
            ..SendState::default()
        };
        save_macro(&mut state);
        state.text = "AT+VER?".to_owned();
        state.macro_name = "AT".to_owned();
        save_macro(&mut state);
        assert_eq!(state.macros.len(), 1);
        assert_eq!(state.macros[0].text, "AT+VER?");
        assert_eq!(macro_payload(&state.macros[0]), Ok(b"AT+VER?\r\n".to_vec()));
    }
}
