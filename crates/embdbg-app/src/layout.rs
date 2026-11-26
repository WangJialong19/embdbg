//! The window layout that survives a restart: which tabs are open, where they sit, which one is
//! in front, and how the space is divided.
//!
//! It is kept apart from the settings: `layout.json` is written whenever the dock changes, and a
//! file this build cannot use is ignored rather than repaired, so a bad edit never loses tabs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Context;
use egui_dock::DockState;
use serde::{Deserialize, Serialize};

use crate::app::Tab;
use crate::logging;

/// Bumped when the meaning of a saved layout changes; older files then fall back to the default.
const VERSION: u32 = 4;

/// Every main view; the two closable side panels may be missing from a saved layout.
const REQUIRED: [Tab; 4] = [Tab::Waveform, Tab::Xy, Tab::Spectrum, Tab::Histogram];

#[derive(Serialize)]
struct Saved<'a> {
    version: u32,
    dock: &'a DockState<Tab>,
}

#[derive(Deserialize)]
struct Loaded {
    version: u32,
    dock: DockState<Tab>,
}

/// `%LOCALAPPDATA%\embdbg\layout.json` on Windows, the equivalent elsewhere.
pub fn path() -> Option<PathBuf> {
    logging::app_data_dir().map(|dir| dir.join("layout.json"))
}

/// Reads the saved layout, if there is one this build understands.
pub fn load() -> Option<DockState<Tab>> {
    path().and_then(|path| load_from(&path))
}

fn load_from(path: &Path) -> Option<DockState<Tab>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(
                target: "embdbg::layout",
                "读取布局失败（{}）：{error}",
                path.display()
            );
            return None;
        }
    };
    let loaded: Loaded = match serde_json::from_str(&text) {
        Ok(loaded) => loaded,
        Err(error) => {
            tracing::warn!(
                target: "embdbg::layout",
                "忽略无法解析的布局（{}）：{error}",
                path.display()
            );
            return None;
        }
    };
    if loaded.version != VERSION {
        tracing::warn!(
            target: "embdbg::layout",
            "布局版本 {} 与当前 {} 不兼容，使用默认布局",
            loaded.version,
            VERSION
        );
        return None;
    }
    let mut dock = loaded.dock;
    expand(&mut dock);
    if !is_usable(&dock) {
        tracing::warn!(
            target: "embdbg::layout",
            "布局缺少页签或页签重复（{}），使用默认布局",
            path.display()
        );
        return None;
    }
    Some(dock)
}

/// The layout's text, so an unchanged layout is not written again.
///
/// Node rectangles are runtime layout, recomputed on every show, and start out infinite — which
/// JSON cannot represent. They are zeroed in the copy that is saved, so the file depends only on
/// what the user arranged, not on the window size.
pub fn serialize(dock: &DockState<Tab>) -> anyhow::Result<String> {
    let mut dock = dock.clone();
    expand(&mut dock);
    for surface in dock.iter_surfaces_mut() {
        for node in surface.iter_nodes_mut() {
            node.set_rect(egui::Rect::ZERO);
            if let egui_dock::Node::Leaf(leaf) = node {
                leaf.viewport = egui::Rect::ZERO;
            }
        }
    }
    serde_json::to_string_pretty(&Saved {
        version: VERSION,
        dock: &dock,
    })
    .context("序列化布局失败")
}

/// Writes already serialized layout text to its usual place.
pub fn write(text: &str) -> anyhow::Result<()> {
    path().map_or(Ok(()), |path| write_to(&path, text))
}

fn write_to(path: &Path, text: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("创建布局目录失败：{}", parent.display()))?;
    }
    std::fs::write(path, text).with_context(|| format!("写入布局失败：{}", path.display()))
}

/// Whether the layout is one this build can use: every required tab exactly once, and at most
/// one of each side panel. Anything else (a layout from another version, a hand-edited file)
/// falls back to the default instead of starting with tabs missing.
/// Undoes egui_dock's pane folding. A saved layout should never reopen as a blank strip, and a
/// pane folded by an older version comes back with its contents.
fn expand(dock: &mut DockState<Tab>) {
    for surface in dock.iter_surfaces_mut() {
        for node in surface.iter_nodes_mut() {
            if matches!(node, egui_dock::Node::Empty) {
                continue;
            }
            node.set_collapsed(false);
            if node.is_parent() {
                node.set_collapsed_leaf_count(0);
            }
        }
    }
}

fn is_usable(dock: &DockState<Tab>) -> bool {
    let mut seen = HashSet::new();
    for surface in dock.iter_surfaces() {
        for (_, tab) in surface.iter_all_tabs() {
            if !seen.insert(*tab) {
                return false;
            }
        }
    }
    REQUIRED.iter().all(|tab| seen.contains(tab))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::default_dock;

    fn temp_dir() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("embdbg-layout-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_layout_round_trips_with_the_tool_stack_closed() {
        let mut dock = default_dock();
        crate::app::set_toolstack_panel(&mut dock, false);
        let text = serialize(&dock).unwrap();

        let dir = temp_dir();
        let path = dir.join("layout.json");
        write_to(&path, &text).unwrap();
        let loaded = load_from(&path).expect("a saved layout loads");
        assert_eq!(
            loaded.main_surface().tabs().copied().collect::<Vec<Tab>>(),
            dock.main_surface().tabs().copied().collect::<Vec<Tab>>()
        );
        assert!(!crate::app::toolstack_panel_open(&loaded));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_moved_tab_keeps_its_place() {
        let mut dock = default_dock();
        let xy = dock
            .find_tab(&Tab::Xy)
            .expect("X-Y is in the default layout");
        let center = dock
            .find_tab(&Tab::Waveform)
            .expect("the waveform is in the default layout")
            .node_path();
        dock.move_tab(xy, (center, egui_dock::TabInsert::Insert(0.into())));
        assert_eq!(
            dock.main_surface().find_tab(&Tab::Xy).unwrap().1,
            egui_dock::TabIndex(0)
        );

        let text = serialize(&dock).unwrap();
        let dir = temp_dir();
        let path = dir.join("layout.json");
        write_to(&path, &text).unwrap();
        let loaded = load_from(&path).expect("a saved layout loads");
        assert_eq!(
            loaded.main_surface().find_tab(&Tab::Xy).unwrap().1,
            egui_dock::TabIndex(0)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folded_pane_comes_back_expanded() {
        let mut dock = default_dock();
        let (node, _) = dock
            .main_surface()
            .find_tab(&Tab::Connections)
            .expect("the sidebar is in the default layout");
        dock.main_surface_mut()[node].set_collapsed(true);
        assert!(dock.main_surface()[node].is_collapsed());

        // Saving never keeps the folded state, so a blank strip cannot be restored.
        let text = serialize(&dock).unwrap();
        let parsed: Loaded = serde_json::from_str(&text).unwrap();
        assert!(!parsed.dock.main_surface()[node].is_collapsed());

        let dir = temp_dir();
        let path = dir.join("layout.json");
        write_to(&path, &text).unwrap();
        let loaded = load_from(&path).expect("a saved layout loads");
        assert!(!loaded.main_surface()[node].is_collapsed());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_layout_with_both_side_panels_closed_round_trips() {
        let mut dock = default_dock();
        crate::app::set_toolstack_panel(&mut dock, false);
        crate::app::set_connections_panel(&mut dock, false);
        let text = serialize(&dock).unwrap();
        let dir = temp_dir();
        let path = dir.join("layout.json");
        write_to(&path, &text).unwrap();
        let loaded = load_from(&path).expect("a layout without the side panels loads");
        assert!(!crate::app::toolstack_panel_open(&loaded));
        assert!(!crate::app::connections_panel_open(&loaded));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_layout_missing_a_tab_is_refused() {
        let mut dock = default_dock();
        // Waveform cannot be closed from the UI, but a stale or edited file could still omit it.
        if let Some(path) = dock.find_tab(&Tab::Waveform) {
            dock.remove_tab(path);
        }
        let text = serialize(&dock).unwrap();
        let dir = temp_dir();
        let path = dir.join("layout.json");
        write_to(&path, &text).unwrap();
        assert!(load_from(&path).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_layout_from_another_version_is_refused() {
        let dock = default_dock();
        let text = serialize(&dock).unwrap().replacen(
            &format!("\"version\": {VERSION}"),
            "\"version\": 99",
            1,
        );
        let dir = temp_dir();
        let path = dir.join("layout.json");
        write_to(&path, &text).unwrap();
        assert!(load_from(&path).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let dir = temp_dir();
        assert!(load_from(&dir.join("nothing.json")).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
