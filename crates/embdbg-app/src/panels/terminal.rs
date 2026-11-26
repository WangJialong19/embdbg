//! The terminal: a VT100 screen in cells, with colour, a cursor and keyboard input.
//!
//! This is the view a board's console needs: the device's own escape sequences, carriage returns
//! that redraw a line, full-screen programs, and keys that go straight down the port — closer to
//! Electerm than to a list of lines.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use egui::{
    Align, Color32, CursorIcon, Event, EventFilter, FontId, ImeEvent, Key, Layout, Modifiers,
    PointerButton, Pos2, Rangef, Rect, RichText, Sense, Stroke, TextEdit, Ui, Vec2,
    epaint::CornerRadius, output::IMEOutput,
};
use embdbg_core::engine::{Connection, ConnectionId, ConnectionState, Engine, TransportConfig};
use embdbg_core::pipeline::SessionShared;
use embdbg_core::pipeline::Terminal;
use embdbg_core::pipeline::terminal::vt100;
use embdbg_core::transport::ModbusEndpoint;

use crate::theme;

/// Font size of the grid.
const FONT_SIZE: f32 = 13.0;
/// Extra space under each row, so lines do not touch.
const LINE_GAP: f32 = 2.0;
/// Inset between the panel border and the first cell.
const PAD_X: f32 = 6.0;
const PAD_Y: f32 = 4.0;
/// Room for the status line under the grid.
const STATUS_HEIGHT: f32 = 22.0;
/// The cursor blinks at this frequency, in hertz.
const BLINK_HZ: f64 = 1.6;
/// The highlight over selected cells; translucent, so the text underneath stays readable.
const SELECTION: Color32 = Color32::from_rgba_unmultiplied_const(0x3f, 0x6f, 0xc9, 0x88);

/// A mouse selection, in cell coordinates. Both ends are inclusive; `anchor` is where the drag
/// began and `head` follows the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
    anchor: (u16, u16),
    head: (u16, u16),
}

impl Selection {
    /// The two ends in reading order: top-left first.
    fn ordered(self) -> ((u16, u16), (u16, u16)) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

#[derive(Debug, Default)]
pub struct TerminalState {
    /// The grid size the parser was last told about.
    size: (u16, u16),
    /// Pending IME composition, drawn at the cursor until it commits.
    preedit: String,
    /// The cells picked with the mouse, if any.
    selection: Option<Selection>,
    /// How many lines the scrollback held when the selection began. The view scrolls back in
    /// step with that number, so new output does not slide the cells out from under the mouse.
    selection_scrollback: usize,
    /// The connection whose terminal is on screen. It is deliberately independent of the
    /// engine's active connection, so watching a console does not follow the other views.
    selected: Option<ConnectionId>,
    /// Connections seen last frame, to notice a newly opened one.
    known: Vec<ConnectionId>,
    /// The connection last drawn, so switching tabs resets the grid and input state.
    shown: Option<ConnectionId>,
    /// Custom tab names, keyed by [`connection_key`].
    names: BTreeMap<String, String>,
    /// Set when a name was changed, so the app writes the settings.
    names_changed: bool,
    /// The connection being renamed, if the tab is in its editor.
    editing: Option<ConnectionId>,
    rename_draft: String,
}

impl TerminalState {
    /// Restores the custom names saved last time.
    pub fn with_names(names: BTreeMap<String, String>) -> Self {
        Self {
            names,
            ..Self::default()
        }
    }

    pub fn names(&self) -> &BTreeMap<String, String> {
        &self.names
    }

    /// Whether a tab was renamed since the last save; the flag is cleared.
    pub fn take_names_changed(&mut self) -> bool {
        std::mem::take(&mut self.names_changed)
    }
}

/// The tab label when the user has not renamed the connection.
pub fn default_name(config: &TransportConfig) -> String {
    match config {
        TransportConfig::Simulator(_) => "模拟源".to_owned(),
        TransportConfig::Serial(serial) => serial.port.clone(),
        TransportConfig::TcpClient(client) => format!("{}:{}", client.host, client.port),
        TransportConfig::TcpServer(server) => format!("监听 {}:{}", server.bind, server.port),
        TransportConfig::Udp(udp) => format!("UDP {}", udp.port),
        TransportConfig::WebSocket(ws) => ws.url.clone(),
        TransportConfig::Mqtt(mqtt) => format!("MQTT {}:{}", mqtt.host, mqtt.port),
        TransportConfig::Modbus(modbus) => match &modbus.endpoint {
            ModbusEndpoint::Tcp { host, port } => format!("Modbus {host}:{port}"),
            ModbusEndpoint::Rtu { port, .. } => format!("Modbus {port}"),
        },
    }
}

/// The key a custom tab name is stored under. It stays the same when the port's baud rate or a
/// protocol setting changes, so renaming COM5 survives a reopen.
fn connection_key(config: &TransportConfig) -> String {
    format!("{}|{}", config.kind(), default_name(config))
}

fn state_color(state: &ConnectionState) -> Color32 {
    match state {
        ConnectionState::Connecting => theme::WARNING,
        ConnectionState::Open => theme::SUCCESS,
        ConnectionState::Closed => theme::FG_DIM,
        ConnectionState::Failed(_) => theme::DANGER,
    }
}

/// Draws one tab per connection and the terminal of the selected one.
pub fn ui(ui: &mut Ui, engine: &Engine, state: &mut TerminalState) {
    let connections = engine.connections();
    if connections.is_empty() {
        ui.label(
            RichText::new("先打开一个连接，每个连接都会有自己的一页终端。").color(theme::FG_DIM),
        );
        return;
    }

    // A newly opened connection comes to the front; a connection that disappeared falls back to
    // the engine's active one, then to the first.
    let ids: Vec<ConnectionId> = connections.iter().map(|connection| connection.id).collect();
    if let Some(new) = ids.iter().find(|id| !state.known.contains(id)).copied() {
        state.selected = Some(new);
    }
    state.known = ids.clone();
    if !state.selected.is_some_and(|id| ids.contains(&id)) {
        state.selected = engine
            .active_id()
            .filter(|id| ids.contains(id))
            .or_else(|| ids.first().copied());
    }
    terminal_tabs(ui, &connections, state);

    let Some(connection) = state
        .selected
        .and_then(|id| connections.iter().find(|connection| connection.id == id))
        .cloned()
    else {
        return;
    };
    if state.shown != Some(connection.id) {
        state.shown = Some(connection.id);
        state.size = (0, 0);
        state.selection = None;
        state.preedit.clear();
    }
    body(ui, engine, &connection, &connection.shared, state);
}

/// The row of connection tabs. Click switches the terminal, double-click renames the tab.
fn terminal_tabs(ui: &mut Ui, connections: &[Arc<Connection>], state: &mut TerminalState) {
    let mut start_edit: Option<(ConnectionId, String)> = None;
    let mut commit: Option<(ConnectionId, String)> = None;

    ui.horizontal_wrapped(|ui| {
        for connection in connections {
            let key = connection_key(&connection.config);
            let name = state
                .names
                .get(&key)
                .cloned()
                .unwrap_or_else(|| default_name(&connection.config));

            if state.editing == Some(connection.id) {
                let editor = ui.add_sized(
                    [120.0, 20.0],
                    TextEdit::singleline(&mut state.rename_draft).hint_text("名称"),
                );
                editor.request_focus();
                let enter = ui.input(|input| input.key_pressed(Key::Enter));
                if enter || editor.lost_focus() {
                    commit = Some((connection.id, state.rename_draft.clone()));
                }
                continue;
            }

            let selected = state.selected == Some(connection.id);
            let color = state_color(&connection.state());
            let response = ui
                .selectable_label(selected, RichText::new(format!("● {name}")).color(color))
                .on_hover_text("单击切换终端，双击重命名");
            if response.double_clicked() {
                start_edit = Some((connection.id, name));
            } else if response.clicked() {
                state.selected = Some(connection.id);
            }
        }
    });

    if let Some((id, draft)) = start_edit {
        state.editing = Some(id);
        state.rename_draft = draft;
    }
    if let Some((id, draft)) = commit {
        if let Some(connection) = connections.iter().find(|connection| connection.id == id) {
            let key = connection_key(&connection.config);
            let name = draft.trim();
            if name.is_empty() {
                state.names.remove(&key);
            } else {
                state.names.insert(key, name.to_owned());
            }
            state.names_changed = true;
        }
        state.editing = None;
        state.rename_draft.clear();
    }
}

/// Draws the terminal of one connection and sends whatever is typed into it.
fn body(
    ui: &mut Ui,
    engine: &Engine,
    connection: &Connection,
    shared: &SessionShared,
    state: &mut TerminalState,
) {
    // One monospace cell, measured from a frame's own font.
    let font = FontId::monospace(FONT_SIZE);
    let (cell_width, line_height) = {
        let galley = ui
            .painter()
            .layout_no_wrap("W".to_owned(), font.clone(), theme::FG);
        (
            galley.size().x.max(1.0),
            (galley.size().y + LINE_GAP).max(1.0),
        )
    };

    let grid_height = (ui.available_height() - STATUS_HEIGHT - ui.spacing().item_spacing.y)
        .max(line_height * 2.0);
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), grid_height),
        Sense::click_and_drag(),
    );
    let response = response.on_hover_cursor(CursorIcon::Text);
    if response.clicked() || response.drag_started() {
        response.request_focus();
    }
    let focused = response.has_focus();
    if focused {
        // While typing, arrows, Tab and Escape belong to the device, not to egui's focus system.
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            );
        });
    }

    // The cells sit inside a small inset, so the first line is not glued to the border.
    let grid = rect.shrink2(Vec2::new(PAD_X, PAD_Y));
    let cols = ((grid.width() / cell_width).floor() as u16).max(2);
    let rows = ((grid.height() / line_height).floor() as u16).max(2);
    if (rows, cols) != state.size {
        let mut terminal = shared.terminal.write();
        terminal.resize(rows, cols);
        // A selection is in cells, and a resize reflows those cells.
        if state.selection.take().is_some() {
            terminal.scroll_to_live();
        }
        state.size = (rows, cols);
    }

    // The wheel walks through history, the way a terminal does. It ends a selection: the mouse
    // is moving the view now, not picking cells.
    let wheel = ui.input(|input| input.smooth_scroll_delta.y);
    if response.hovered() && wheel != 0.0 {
        let lines = ((wheel.abs() / line_height).ceil() as usize).max(1);
        let mut terminal = shared.terminal.write();
        state.selection = None;
        if wheel > 0.0 {
            terminal.scroll_back(lines);
        } else {
            terminal.scroll_forward(lines);
        }
    }

    // While a selection exists the cells under it must stay put. Each line that output pushes
    // into the scrollback moves the view one line further back, which keeps the same lines on
    // screen without copying them. When the scrollback is full it can no longer compensate; that
    // is a selection frozen for thousands of lines, long after it was copied.
    if state.selection.is_some() {
        let mut terminal = shared.terminal.write();
        let len = terminal.scrollback_len();
        if len > state.selection_scrollback {
            let growth = len - state.selection_scrollback;
            terminal.scroll_back(growth);
            state.selection_scrollback = len;
        } else if len < state.selection_scrollback {
            // The history was cleared underneath the selection.
            state.selection = None;
        }
    }

    // Where a point lands in the grid, clamped to it so a drag past the edge keeps selecting.
    let cell_at = |pos: Pos2| -> (u16, u16) {
        let col = ((pos.x - grid.left()) / cell_width)
            .floor()
            .clamp(0.0, f32::from(cols - 1)) as u16;
        let row = ((pos.y - grid.top()) / line_height)
            .floor()
            .clamp(0.0, f32::from(rows - 1)) as u16;
        (row, col)
    };

    if response.double_clicked_by(PointerButton::Primary) {
        if let Some(pos) = response.interact_pointer_pos() {
            let (row, col) = cell_at(pos);
            let terminal = shared.terminal.read();
            state.selection = word_at(&terminal, row, col);
            state.selection_scrollback = terminal.scrollback_len();
        }
    } else if response.drag_started_by(PointerButton::Primary) {
        if let Some(origin) = ui.input(|input| input.pointer.press_origin()) {
            let anchor = cell_at(origin);
            let head = response.interact_pointer_pos().map_or(anchor, cell_at);
            state.selection = Some(Selection { anchor, head });
            state.selection_scrollback = shared.terminal.read().scrollback_len();
        }
    } else if response.dragged_by(PointerButton::Primary) {
        if let (Some(pos), Some(selection)) =
            (response.interact_pointer_pos(), state.selection.as_mut())
        {
            selection.head = cell_at(pos);
        }
    } else if response.clicked() && state.selection.take().is_some() {
        // Clicking without dragging ends the selection and returns to live output.
        shared.terminal.write().scroll_to_live();
    }

    // Ctrl+C copies when there is a selection and interrupts when there is not, as terminals do.
    // Ctrl+Shift+C always means copy, and is swallowed either way.
    let copy_key =
        focused && ui.input(|input| input.modifiers.command && input.key_pressed(Key::C));
    if copy_key && let Some(selection) = state.selection {
        let terminal = shared.terminal.read();
        let text = selection_text(&terminal, selection);
        drop(terminal);
        if !text.is_empty() {
            ui.ctx().copy_text(text);
        }
    }
    let swallow_ctrl_c =
        copy_key && (state.selection.is_some() || ui.input(|input| input.modifiers.shift));

    // Keys go down the port as they are; the device echoes them back.
    if focused {
        let bytes = typed_bytes(ui, &mut state.preedit, swallow_ctrl_c);
        if !bytes.is_empty() {
            if connection.state().is_open()
                && let Err(error) = engine.send(connection.id, &bytes)
            {
                tracing::warn!(target: "embdbg::terminal", "发送失败：{error}");
            }
            // Typing ends the selection and returns to the live screen, the way a shell does.
            state.selection = None;
            shared.terminal.write().scroll_to_live();
        }
    } else {
        state.preedit.clear();
    }

    let terminal = shared.terminal.read();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::ZERO, theme::BG_WINDOW);

    for row in 0..rows {
        let mut col = 0;
        while col < cols {
            let Some(cell) = terminal.cell(row, col) else {
                break;
            };
            if cell.is_wide_continuation() {
                col += 1;
                continue;
            }
            let style = CellStyle::of(cell);
            let start = col;
            let mut text = String::new();
            while col < cols {
                let Some(cell) = terminal.cell(row, col) else {
                    break;
                };
                if cell.is_wide_continuation() {
                    col += 1;
                    continue;
                }
                if CellStyle::of(cell) != style {
                    break;
                }
                match cell.contents() {
                    "" => text.push(' '),
                    contents => text.push_str(contents),
                }
                col += 1;
            }
            paint_run(
                &painter,
                grid,
                row,
                start,
                col,
                &text,
                style,
                cell_width,
                line_height,
                &font,
            );
        }
    }

    // The selection sits above the text but below the cursor; cells it covers stay readable.
    if let Some(selection) = state.selection {
        let ((start_row, start_col), (end_row, end_col)) = selection.ordered();
        for row in start_row..=end_row.min(rows - 1) {
            let from = if row == start_row { start_col } else { 0 };
            let to = if row == end_row {
                end_col.min(cols - 1)
            } else {
                cols - 1
            };
            if to < from {
                continue;
            }
            let highlight = Rect::from_min_size(
                Pos2::new(
                    grid.left() + f32::from(from) * cell_width,
                    grid.top() + f32::from(row) * line_height,
                ),
                Vec2::new(f32::from(to - from + 1) * cell_width, line_height),
            );
            painter.rect_filled(highlight, CornerRadius::ZERO, SELECTION);
        }
    }

    // The cursor: a blinking block while typing, an outline otherwise. A pending IME
    // composition is drawn at its place, underlined, with the caret after it.
    let (cursor_row, cursor_col) = terminal.cursor();
    let live = terminal.is_live();
    let mut cursor_rect = cell_rect(grid, cursor_row, cursor_col, cell_width, line_height);
    let preedit = focused
        .then(|| state.preedit.clone())
        .filter(|text| !text.is_empty());
    if let Some(text) = &preedit {
        let origin = cursor_rect.min;
        let galley = painter.layout_no_wrap(text.clone(), font.clone(), theme::FG);
        let width = galley.size().x.max(cell_width);
        painter.rect_filled(
            Rect::from_min_size(origin, Vec2::new(width, line_height)),
            CornerRadius::ZERO,
            theme::BG_WIDGET,
        );
        painter.galley(origin, galley, theme::FG);
        painter.hline(
            Rangef::new(origin.x, origin.x + width),
            origin.y + line_height - 2.0,
            Stroke::new(1.0, theme::FG),
        );
        cursor_rect = Rect::from_min_size(
            Pos2::new(origin.x + width, origin.y),
            Vec2::new(1.5, line_height),
        );
    } else if live && !terminal.hide_cursor() {
        let cell = terminal.cell(cursor_row, cursor_col);
        if focused {
            ui.ctx().request_repaint_after(Duration::from_millis(300));
            let blink_on = (ui.input(|input| input.time) * BLINK_HZ).fract() < 0.5;
            if blink_on {
                let wide = cell.is_some_and(vt100::Cell::is_wide);
                let block = Rect::from_min_size(
                    cursor_rect.min,
                    Vec2::new(
                        if wide { cell_width * 2.0 } else { cell_width },
                        line_height,
                    ),
                );
                painter.rect_filled(block, CornerRadius::ZERO, theme::FG);
                if let Some(cell) = cell
                    && !cell.contents().is_empty()
                {
                    let galley = painter.layout_no_wrap(
                        cell.contents().to_owned(),
                        font.clone(),
                        theme::BG_WINDOW,
                    );
                    painter.galley(block.min, galley, theme::BG_WINDOW);
                }
            }
        } else {
            painter.rect_stroke(
                cursor_rect,
                CornerRadius::ZERO,
                Stroke::new(1.0, theme::FG_DIM),
                egui::StrokeKind::Inside,
            );
        }
    }

    // The IME candidate window follows the caret.
    if focused {
        let to_global = ui
            .ctx()
            .layer_transform_to_global(ui.layer_id())
            .unwrap_or_default();
        ui.ctx().output_mut(|output| {
            output.ime = Some(IMEOutput {
                purpose: egui::IMEPurpose::Normal,
                rect: to_global * rect,
                cursor_rect: to_global * cursor_rect,
                should_interrupt_composition: false,
            });
        });
    }

    let (rows_now, cols_now) = (terminal.rows(), terminal.cols());
    let offset = terminal.offset();
    let alternate = terminal.alternate_screen();
    drop(terminal);

    // Buttons on the status line do not take the keyboard away from the screen.
    let mut keep_focus = false;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{cols_now}×{rows_now}"))
                .monospace()
                .small()
                .color(theme::FG_DIM),
        );
        if alternate {
            ui.label(RichText::new("全屏程序").small().color(theme::ACCENT));
        }
        if !live {
            ui.label(
                RichText::new(format!("回看 {offset} 行"))
                    .small()
                    .color(theme::WARNING),
            );
        }
        if state.selection.is_some() {
            ui.label(
                RichText::new("已暂停刷新，点击恢复")
                    .small()
                    .color(theme::ACCENT),
            );
        }
        if focused {
            if ui.small_button("释放键盘").clicked() {
                response.surrender_focus();
            }
        } else {
            ui.label(
                RichText::new("点击后键盘输入，回车发送；拖选复制（Ctrl+Shift+C），滚轮回看历史")
                    .small()
                    .color(theme::FG_DIM),
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("复制屏幕").clicked() {
                ui.ctx().copy_text(shared.terminal.read().contents());
                keep_focus = true;
            }
            if let Some(selection) = state.selection
                && ui.small_button("复制选中").clicked()
            {
                ui.ctx()
                    .copy_text(selection_text(&shared.terminal.read(), selection));
                keep_focus = true;
            }
            if ui.small_button("清屏").clicked() {
                // Clears this view only; the device's screen is untouched.
                state.selection = None;
                let mut terminal = shared.terminal.write();
                terminal.scroll_to_live();
                terminal.feed(b"\x1b[2J\x1b[H");
                keep_focus = true;
            }
            if !live && ui.small_button("回到底部").clicked() {
                state.selection = None;
                shared.terminal.write().scroll_to_live();
                keep_focus = true;
            }
        });
    });
    if keep_focus && focused {
        response.request_focus();
    }
}

/// The rectangle of one cell in screen coordinates.
fn cell_rect(rect: Rect, row: u16, col: u16, cell_width: f32, line_height: f32) -> Rect {
    Rect::from_min_size(
        Pos2::new(
            rect.left() + f32::from(col) * cell_width,
            rect.top() + f32::from(row) * line_height,
        ),
        Vec2::new(cell_width, line_height),
    )
}

/// A cell's colours and decoration, so runs of equal style can be drawn in one go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CellStyle {
    fg: Color32,
    bg: Color32,
    underline: bool,
}

impl CellStyle {
    fn of(cell: &vt100::Cell) -> Self {
        let bold = cell.bold();
        let mut fg = ansi_color(cell.fgcolor(), bold, theme::FG);
        let mut bg = ansi_color(cell.bgcolor(), false, theme::BG_WINDOW);
        if cell.dim() {
            fg = fg.gamma_multiply(0.6);
        }
        if cell.inverse() {
            std::mem::swap(&mut fg, &mut bg);
        }
        Self {
            fg,
            bg,
            underline: cell.underline(),
        }
    }
}

/// Maps a terminal colour to the screen. `bold` brightens the eight base colours, as most
/// terminals do; indices past the sixteen follow the xterm 6×6×6 cube and grey ramp.
fn ansi_color(color: vt100::Color, bold: bool, default: Color32) -> Color32 {
    match color {
        vt100::Color::Default => default,
        vt100::Color::Idx(index) => {
            let index = if bold && index < 8 { index + 8 } else { index };
            palette_color(index)
        }
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

fn palette_color(index: u8) -> Color32 {
    if let Some(color) = theme::ANSI.get(usize::from(index)) {
        return *color;
    }
    if index < 232 {
        const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
        let cube = index - 16;
        return Color32::from_rgb(
            LEVELS[usize::from(cube / 36)],
            LEVELS[usize::from((cube / 6) % 6)],
            LEVELS[usize::from(cube % 6)],
        );
    }
    let grey = 8 + (index - 232) * 10;
    Color32::from_gray(grey)
}

#[allow(clippy::too_many_arguments)]
fn paint_run(
    painter: &egui::Painter,
    rect: Rect,
    row: u16,
    start_col: u16,
    end_col: u16,
    text: &str,
    style: CellStyle,
    cell_width: f32,
    line_height: f32,
    font: &FontId,
) {
    // Blank cells with no colour of their own need no work; most of a screen is this.
    if style.bg == theme::BG_WINDOW && !style.underline && text.trim().is_empty() {
        return;
    }
    let origin = Pos2::new(
        rect.left() + f32::from(start_col) * cell_width,
        rect.top() + f32::from(row) * line_height,
    );
    let width = f32::from(end_col - start_col) * cell_width;
    if style.bg != theme::BG_WINDOW {
        painter.rect_filled(
            Rect::from_min_size(origin, Vec2::new(width, line_height)),
            CornerRadius::ZERO,
            style.bg,
        );
    }
    let galley = painter.layout_no_wrap(text.to_owned(), font.clone(), style.fg);
    painter.galley(origin, galley, style.fg);
    if style.underline {
        painter.hline(
            Rangef::new(origin.x, origin.x + width),
            origin.y + line_height - 2.0,
            Stroke::new(1.0, style.fg),
        );
    }
}

/// Everything typed since the last frame, as bytes for the port.
///
/// `swallow_ctrl_c` keeps Ctrl+C from also sending an interrupt when it is being used to copy.
fn typed_bytes(ui: &Ui, preedit: &mut String, swallow_ctrl_c: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut replaced_preedit = false;
    ui.input(|input| {
        for event in &input.events {
            match event {
                Event::Ime(ImeEvent::Preedit { text, .. }) => {
                    preedit.clone_from(text);
                    continue;
                }
                Event::Ime(ImeEvent::Commit(text)) => {
                    bytes.extend_from_slice(text.as_bytes());
                    replaced_preedit = true;
                    continue;
                }
                // A control chord is a control code, not the letter it produced.
                Event::Text(_) if input.modifiers.command => continue,
                Event::Key {
                    key: Key::C,
                    pressed: true,
                    modifiers,
                    ..
                } if swallow_ctrl_c && modifiers.command => continue,
                Event::Key { pressed: false, .. } => continue,
                _ => {}
            }
            if let Some(more) = event_bytes(event) {
                bytes.extend_from_slice(&more);
                replaced_preedit = true;
            }
        }
    });
    if replaced_preedit {
        preedit.clear();
    }
    bytes
}

/// The word of cells around the given one, for double-click selection. Cells with only spaces
/// separate words; a double-click on one selects nothing.
fn word_at(terminal: &Terminal, row: u16, col: u16) -> Option<Selection> {
    let cell = terminal.cell(row, col)?;
    // The second half of a wide character belongs to the character before it.
    let col = if cell.is_wide_continuation() {
        col.saturating_sub(1)
    } else {
        col
    };
    let is_word = |col: u16| {
        terminal
            .cell(row, col)
            .is_some_and(|cell| !cell.is_wide_continuation() && !cell.contents().trim().is_empty())
    };
    if !is_word(col) {
        return None;
    }
    let mut start = col;
    while start > 0 && is_word(start - 1) {
        start -= 1;
    }
    let mut end = col;
    while end + 1 < terminal.cols() && is_word(end + 1) {
        end += 1;
    }
    Some(Selection {
        anchor: (row, start),
        head: (row, end),
    })
}

/// The text under a selection, with each line's padding trimmed and wrapped lines rejoined.
fn selection_text(terminal: &Terminal, selection: Selection) -> String {
    let ((start_row, start_col), (end_row, end_col)) = selection.ordered();
    let text = terminal.screen().contents_between(
        start_row,
        start_col,
        end_row,
        end_col.saturating_add(1),
    );
    text.split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

/// One event as bytes, if it is input the terminal should forward.
fn event_bytes(event: &Event) -> Option<Vec<u8>> {
    match event {
        Event::Text(text) => Some(text.as_bytes().to_vec()),
        Event::Paste(text) => Some(text.as_bytes().to_vec()),
        Event::Ime(ImeEvent::Commit(text)) => Some(text.as_bytes().to_vec()),
        Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => key_bytes(*key, modifiers),
        _ => None,
    }
}

/// One key press as the bytes a terminal sends.
fn key_bytes(key: Key, modifiers: &Modifiers) -> Option<Vec<u8>> {
    if modifiers.command {
        // Ctrl+V is the integration's paste: it arrives as Event::Paste with the text.
        if key == Key::V {
            return None;
        }
        return ctrl_byte(key).map(|byte| vec![byte]);
    }
    let bytes: &[u8] = match key {
        Key::Enter => b"\r",
        // DEL, which most embedded consoles expect for backspace.
        Key::Backspace => b"\x7f",
        Key::Tab => b"\t",
        Key::Escape => b"\x1b",
        Key::ArrowUp => b"\x1b[A",
        Key::ArrowDown => b"\x1b[B",
        Key::ArrowRight => b"\x1b[C",
        Key::ArrowLeft => b"\x1b[D",
        Key::Home => b"\x1b[H",
        Key::End => b"\x1b[F",
        Key::Delete => b"\x1b[3~",
        Key::PageUp => b"\x1b[5~",
        Key::PageDown => b"\x1b[6~",
        Key::Insert => b"\x1b[2~",
        _ => return None,
    };
    Some(bytes.to_vec())
}

/// The control byte for Ctrl + a letter, as every terminal sends it.
fn ctrl_byte(key: Key) -> Option<u8> {
    Some(match key {
        Key::Space => 0x00,
        Key::A => 0x01,
        Key::B => 0x02,
        Key::C => 0x03,
        Key::D => 0x04,
        Key::E => 0x05,
        Key::F => 0x06,
        Key::G => 0x07,
        Key::H => 0x08,
        Key::I => 0x09,
        Key::J => 0x0a,
        Key::K => 0x0b,
        Key::L => 0x0c,
        Key::M => 0x0d,
        Key::N => 0x0e,
        Key::O => 0x0f,
        Key::P => 0x10,
        Key::Q => 0x11,
        Key::R => 0x12,
        Key::S => 0x13,
        Key::T => 0x14,
        Key::U => 0x15,
        Key::V => 0x16,
        Key::W => 0x17,
        Key::X => 0x18,
        Key::Y => 0x19,
        Key::Z => 0x1a,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key, ctrl: bool) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers {
                ctrl,
                command: ctrl,
                ..Modifiers::default()
            },
        }
    }

    #[test]
    fn text_and_control_keys_map_to_bytes() {
        assert_eq!(
            event_bytes(&Event::Text("AT".to_owned())),
            Some(b"AT".to_vec())
        );
        assert_eq!(event_bytes(&key(Key::Enter, false)), Some(b"\r".to_vec()));
        assert_eq!(event_bytes(&key(Key::Backspace, false)), Some(vec![0x7f]));
        assert_eq!(
            event_bytes(&key(Key::ArrowUp, false)),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(event_bytes(&key(Key::F5, false)), None);
    }

    #[test]
    fn control_combinations_are_control_codes() {
        assert_eq!(event_bytes(&key(Key::C, true)), Some(vec![0x03]));
        assert_eq!(event_bytes(&key(Key::D, true)), Some(vec![0x04]));
        assert_eq!(event_bytes(&key(Key::Z, true)), Some(vec![0x1a]));
        // Ctrl with a key that has no control code sends nothing.
        assert_eq!(event_bytes(&key(Key::ArrowUp, true)), None);
        // Paste is left to the integration, which sends the clipboard as Event::Paste.
        assert_eq!(event_bytes(&key(Key::V, true)), None);
    }

    #[test]
    fn paste_arrives_as_its_own_bytes() {
        assert_eq!(
            event_bytes(&Event::Paste("ls\r".to_owned())),
            Some(b"ls\r".to_vec())
        );
    }

    #[test]
    fn tabs_are_named_after_their_connection() {
        let serial = TransportConfig::Serial(embdbg_core::transport::SerialConfig {
            port: "COM5".to_owned(),
            baud: 921_600,
            ..embdbg_core::transport::SerialConfig::default()
        });
        assert_eq!(default_name(&serial), "COM5");
        assert_eq!(connection_key(&serial), "串口|COM5");
        // The key does not depend on the baud rate, so a rename survives a reopen.
        let slower = TransportConfig::Serial(embdbg_core::transport::SerialConfig {
            port: "COM5".to_owned(),
            ..embdbg_core::transport::SerialConfig::default()
        });
        assert_eq!(connection_key(&slower), connection_key(&serial));
    }

    #[test]
    fn a_selection_copies_without_the_line_padding() {
        let mut terminal = Terminal::new(4, 20);
        terminal.feed(b"abc   \r\ndef\r\n");

        let selection = Selection {
            anchor: (0, 0),
            head: (1, 1),
        };
        assert_eq!(selection_text(&terminal, selection), "abc\nde");

        // A selection dragged backwards means the same cells.
        let selection = Selection {
            anchor: (0, 2),
            head: (0, 1),
        };
        assert_eq!(selection_text(&terminal, selection), "bc");
    }

    #[test]
    fn a_double_click_selects_the_word_under_the_pointer() {
        let mut terminal = Terminal::new(4, 20);
        terminal.feed(b"hello world\r\n");
        assert_eq!(
            word_at(&terminal, 0, 8),
            Some(Selection {
                anchor: (0, 6),
                head: (0, 10),
            })
        );
        // A space has no word.
        assert_eq!(word_at(&terminal, 0, 5), None);
    }

    #[test]
    fn the_palette_covers_256_colours() {
        assert_eq!(palette_color(1), theme::ANSI[1]);
        assert_eq!(palette_color(196), Color32::from_rgb(255, 0, 0));
        assert_eq!(palette_color(232), Color32::from_gray(8));
        assert_eq!(palette_color(255), Color32::from_gray(238));
    }

    #[test]
    fn bold_brightens_only_the_base_colours() {
        assert_eq!(
            ansi_color(vt100::Color::Idx(1), true, theme::FG),
            theme::ANSI[9]
        );
        assert_eq!(
            ansi_color(vt100::Color::Idx(9), true, theme::FG),
            theme::ANSI[9]
        );
        assert_eq!(
            ansi_color(vt100::Color::Default, true, theme::FG),
            theme::FG
        );
    }
}
