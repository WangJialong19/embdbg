//! The event log at the bottom of the window.

use std::collections::VecDeque;

use egui::{Color32, RichText, ScrollArea, Ui};
use embdbg_core::api::Event;

use crate::theme;

/// Lines kept in the panel. Older ones are dropped.
const MAX_LINES: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn color(self) -> Color32 {
        match self {
            Level::Info => theme::FG,
            Level::Warn => theme::WARNING,
            Level::Error => theme::DANGER,
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Level::Info => "信息",
            Level::Warn => "警告",
            Level::Error => "错误",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Local wall-clock time, `HH:MM:SS.mmm`.
    pub time: String,
    pub level: Level,
    pub text: String,
}

/// A bounded, append-only log.
#[derive(Debug, Default)]
pub struct LogBuffer {
    lines: VecDeque<Line>,
}

impl LogBuffer {
    pub fn push(&mut self, level: Level, text: impl Into<String>) {
        let time = chrono::Local::now().format("%H:%M:%S%.3f").to_string();
        if self.lines.len() == MAX_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(Line {
            time,
            level,
            text: text.into(),
        });
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.push(Level::Info, text);
    }

    pub fn warn(&mut self, text: impl Into<String>) {
        self.push(Level::Warn, text);
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.push(Level::Error, text);
    }

    /// Adds an engine event, if it says anything worth reading.
    pub fn push_event(&mut self, event: &Event) {
        match event {
            Event::Simulator { .. } => self.info(event.to_string()),
            Event::Info(text) => self.info(text.clone()),
            Event::Warn(text) => self.warn(text.clone()),
            Event::Error(text) => self.error(text.clone()),
        }
    }

    pub fn lines(&self) -> impl Iterator<Item = &Line> {
        self.lines.iter()
    }
}

pub fn ui(ui: &mut Ui, log: &LogBuffer) {
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for line in log.lines() {
                ui.horizontal(|ui| {
                    ui.add_space(theme::SPACE_2);
                    ui.label(
                        RichText::new(&line.time)
                            .monospace()
                            .small()
                            .color(theme::FG_DIM),
                    );
                    ui.label(
                        RichText::new(line.level.tag())
                            .small()
                            .color(line.level.color())
                            .strong(),
                    );
                    ui.label(RichText::new(&line.text).small().color(line.level.color()));
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_is_bounded() {
        let mut log = LogBuffer::default();
        for i in 0..(MAX_LINES + 10) {
            log.info(format!("line {i}"));
        }
        let texts: Vec<&str> = log.lines().map(|l| l.text.as_str()).collect();
        assert_eq!(texts.len(), MAX_LINES);
        assert_eq!(texts[0], "line 10");
        let last = format!("line {}", MAX_LINES + 9);
        assert_eq!(texts.last().copied(), Some(last.as_str()));
    }

    #[test]
    fn engine_events_become_lines_of_matching_severity() {
        let mut log = LogBuffer::default();
        log.push_event(&Event::Warn("串口读取超时".into()));
        log.push_event(&Event::Simulator { running: true });
        let levels: Vec<Level> = log.lines().map(|l| l.level).collect();
        assert_eq!(levels, [Level::Warn, Level::Info]);
    }
}
