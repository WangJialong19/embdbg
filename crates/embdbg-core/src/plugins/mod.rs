//! Decoder plugins: a small WebAssembly module per protocol, sandboxed by the host.
//!
//! A plugin is a directory with a `plugin.toml` manifest and a `.wasm` file. The host compiles
//! the module once, gives every connection its own instance, and meters both instructions (fuel)
//! and memory, so a broken or hostile plugin cannot take the program down or spin forever.
//!
//! The guest ABI is deliberately tiny; see `docs/plugins.md`:
//!
//! * `memory` — the guest's linear memory.
//! * `input_ptr() -> i32` and `input_capacity() -> i32` — where the host writes one frame.
//! * `decode(len: i32) -> i32` — decodes the frame in that buffer, returns a pointer to
//!   `[u32 len][u8 kind][payload]`, or 0 to ignore the frame. Kind 0 is f32 samples, 1 a line
//!   of text, 3 an error message.
//! * `alloc(size: i32) -> i32` and `set_params(ptr: i32, len: i32)` — optional; the host writes
//!   the connection's JSON parameters once before decoding.

pub mod decoder;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use crate::pipeline::Framing;

pub use decoder::{PluginDecoder, PluginOutput};

/// The ABI this build speaks. A manifest with another number is refused, not guessed.
pub const API_VERSION: u32 = 1;

/// How much a single decode may run, in wasmi fuel units.
pub const FUEL_PER_FRAME: u64 = 2_000_000;

/// The largest linear memory a plugin may grow to.
pub const MEMORY_LIMIT: usize = 8 * 1024 * 1024;

/// The most one frame may produce, so a plugin cannot make the host allocate without bound.
pub const MAX_OUTPUT: usize = 64 * 1024;

/// The manifest of one plugin, at `plugin.toml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PluginManifest {
    /// Must equal [`API_VERSION`].
    pub api: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    /// The Wasm file, relative to the manifest.
    pub entry: String,
    /// `line`, `datagram` or `tail`; tail needs `tail`.
    pub framing: String,
    /// Hex bytes that end a frame when `framing = "tail"`, such as "00 00 80 7F".
    pub tail: String,
    /// Frames longer than this are dropped instead of buffered forever.
    pub max_frame: usize,
    /// The parameters the host renders as a form and passes to `set_params`.
    pub fields: Vec<PluginField>,
}

impl Default for PluginManifest {
    fn default() -> Self {
        Self {
            api: API_VERSION,
            name: String::new(),
            version: "0.1.0".to_owned(),
            description: String::new(),
            entry: "decoder.wasm".to_owned(),
            framing: "line".to_owned(),
            tail: String::new(),
            max_frame: 64 * 1024,
            fields: Vec::new(),
        }
    }
}

/// One declarative parameter of a plugin.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PluginField {
    /// The JSON key passed to `set_params`.
    pub key: String,
    pub label: String,
    /// `number`, `integer`, `bool`, `text` or `choice`.
    pub kind: String,
    /// The starting value; its JSON type should match `kind`.
    pub default: serde_json::Value,
    /// Bounds for `number` and `integer`.
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// The allowed strings for `choice`.
    pub options: Vec<String>,
}

impl Default for PluginField {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            kind: "text".to_owned(),
            default: serde_json::Value::Null,
            min: None,
            max: None,
            options: Vec::new(),
        }
    }
}

/// What the window and the MCP tools see about one installed plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub slot: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    pub fields: Vec<PluginField>,
    /// Where the plugin was loaded from, for the settings page.
    pub dir: PathBuf,
}

/// A compiled plugin: its manifest, framing and module.
pub struct Plugin {
    pub info: PluginInfo,
    pub framing: Framing,
    module: wasmi::Module,
}

impl std::fmt::Debug for Plugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plugin")
            .field("info", &self.info)
            .field("framing", &self.framing)
            .finish_non_exhaustive()
    }
}

impl Plugin {
    pub fn module(&self) -> &wasmi::Module {
        &self.module
    }
}

struct Registry {
    /// Keyed by slot. A missing key is a plugin that was uninstalled since the last scan.
    plugins: BTreeMap<u32, Arc<Plugin>>,
}

/// Installed plugins, replaced as a whole when the directories are rescanned.
static REGISTRY: RwLock<Option<Arc<Registry>>> = RwLock::new(None);

fn registry() -> &'static RwLock<Option<Arc<Registry>>> {
    &REGISTRY
}

fn read_registry() -> Option<Arc<Registry>> {
    registry()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The plugin directories the window uses: the one next to the program first, then the user's.
pub fn default_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        dirs.push(parent.join("plugins"));
    }
    dirs
}

/// Scans `dirs` in order and installs what was found. Returns the number of plugins loaded.
///
/// A directory is one plugin: it must contain `plugin.toml`; its `entry` is the Wasm file.
/// A broken plugin is reported through `tracing` and skipped, never fatal.
pub fn load_from_dirs(dirs: &[PathBuf]) -> usize {
    let previous = read_registry();
    // Keep a plugin's slot across rescans: open connections hold DecoderSpec::Plugin(slot), and
    // installing one plugin must not silently redirect them to another.
    let mut slots: BTreeMap<String, u32> =
        previous.as_ref().map_or_else(BTreeMap::new, |registry| {
            registry
                .plugins
                .values()
                .map(|plugin| (plugin.info.name.clone(), plugin.info.slot))
                .collect()
        });
    let mut next_slot = previous.as_ref().map_or(0, |registry| {
        registry
            .plugins
            .keys()
            .next_back()
            .map_or(0, |slot| slot + 1)
    });
    let mut plugins: BTreeMap<u32, Arc<Plugin>> = BTreeMap::new();
    let mut names: BTreeMap<String, PathBuf> = BTreeMap::new();
    let engine = host_engine();

    for dir in dirs {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                tracing::warn!(target: "embdbg::plugins", "读取插件目录失败（{}）：{error}", dir.display());
                continue;
            }
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .collect();
        paths.sort();
        for path in paths {
            if !path.is_dir() {
                continue;
            }
            match load_one(&path, &engine, 0) {
                Ok(mut plugin) => {
                    if let Some(first) = names.get(&plugin.info.name) {
                        tracing::warn!(
                            target: "embdbg::plugins",
                            "插件「{}」重复（{} 与 {}），用前一个",
                            plugin.info.name,
                            first.display(),
                            path.display()
                        );
                        continue;
                    }
                    let slot = slots.remove(&plugin.info.name).unwrap_or_else(|| {
                        let slot = next_slot;
                        next_slot += 1;
                        slot
                    });
                    plugin.info.slot = slot;
                    tracing::info!(
                        target: "embdbg::plugins",
                        "已加载插件「{}」v{}（{}）",
                        plugin.info.name,
                        plugin.info.version,
                        path.display()
                    );
                    names.insert(plugin.info.name.clone(), path);
                    plugins.insert(slot, Arc::new(plugin));
                }
                Err(error) => {
                    tracing::warn!(
                        target: "embdbg::plugins",
                        "跳过插件目录 {}：{error}",
                        path.display()
                    );
                }
            }
        }
    }

    let count = plugins.len();
    *registry()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(Registry { plugins }));
    count
}

/// The wasmi engine: fuel metering is on for every module the host compiles.
fn host_engine() -> wasmi::Engine {
    let mut config = wasmi::Config::default();
    config.consume_fuel(true);
    wasmi::Engine::new(&config)
}

fn load_one(dir: &Path, engine: &wasmi::Engine, slot: u32) -> Result<Plugin, String> {
    let manifest_path = dir.join("plugin.toml");
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("读取 plugin.toml 失败：{error}"))?;
    let manifest: PluginManifest =
        toml::from_str(&text).map_err(|error| format!("plugin.toml 格式错误：{error}"))?;
    if manifest.api != API_VERSION {
        return Err(format!(
            "插件 API 版本 {}，本程序支持 {API_VERSION}",
            manifest.api
        ));
    }
    if manifest.name.trim().is_empty() {
        return Err("插件缺少 name".to_owned());
    }
    if manifest.entry.contains("..") || Path::new(&manifest.entry).is_absolute() {
        return Err("entry 必须是插件目录内的相对路径".to_owned());
    }
    let wasm_path = dir.join(&manifest.entry);
    let wasm = std::fs::read(&wasm_path)
        .map_err(|error| format!("读取 {} 失败：{error}", wasm_path.display()))?;
    let module =
        wasmi::Module::new(engine, &wasm[..]).map_err(|error| format!("Wasm 模块无效：{error}"))?;
    let framing = framing_of(&manifest)?;
    Ok(Plugin {
        info: PluginInfo {
            slot,
            name: manifest.name,
            version: manifest.version,
            description: manifest.description,
            fields: manifest.fields,
            dir: dir.to_path_buf(),
        },
        framing,
        module,
    })
}

/// Turns the manifest's framing description into a framer configuration.
fn framing_of(manifest: &PluginManifest) -> Result<Framing, String> {
    let max_len = manifest.max_frame.clamp(256, 4 * 1024 * 1024);
    match manifest.framing.as_str() {
        "line" | "" => Ok(Framing::Lines { max_len }),
        "datagram" => Ok(Framing::Datagram),
        "tail" => {
            let tail = parse_hex(&manifest.tail).map_err(|error| format!("tail 无效：{error}"))?;
            if tail.is_empty() {
                return Err("framing = \"tail\" 需要 tail，例如 \"00 00 80 7F\"".to_owned());
            }
            Ok(Framing::Tail { tail, max_len })
        }
        other => Err(format!("未知 framing {other}：用 line、datagram 或 tail")),
    }
}

fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let digits: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if !digits.len().is_multiple_of(2) {
        return Err("十六进制位数必须是偶数".to_owned());
    }
    (0..digits.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&digits[index..index + 2], 16)
                .map_err(|_| format!("不是十六进制：{}", &digits[index..index + 2]))
        })
        .collect()
}

/// The installed plugins, in slot order.
pub fn list() -> Vec<PluginInfo> {
    read_registry().map_or_else(Vec::new, |registry| {
        registry
            .plugins
            .values()
            .map(|plugin| plugin.info.clone())
            .collect()
    })
}

pub fn get(slot: u32) -> Option<Arc<Plugin>> {
    read_registry()?.plugins.get(&slot).cloned()
}

/// The slot a plugin was installed at, by name.
pub fn slot_of(name: &str) -> Option<u32> {
    list()
        .into_iter()
        .find(|info| info.name == name)
        .map(|info| info.slot)
}

/// The framing a plugin asked for; `None` when the slot no longer exists.
pub fn framing(slot: u32) -> Option<Framing> {
    get(slot).map(|plugin| plugin.framing.clone())
}

/// The JSON parameters a plugin declares, so a new connection starts from its defaults.
pub fn default_params(slot: u32) -> String {
    let Some(plugin) = get(slot) else {
        return String::new();
    };
    let mut values = serde_json::Map::new();
    for field in &plugin.info.fields {
        values.insert(field.key.clone(), field.default.clone());
    }
    serde_json::Value::Object(values).to_string()
}

/// Installs a set of already compiled plugins. Tests use this; the program uses
/// [`load_from_dirs`] or [`install_from`].
pub fn install(plugins: Vec<Plugin>) {
    let mut installed = BTreeMap::new();
    for plugin in plugins {
        installed.insert(plugin.info.slot, Arc::new(plugin));
    }
    *registry()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some(Arc::new(Registry { plugins: installed }));
}

/// Checks a directory as a plugin without installing it.
pub fn probe(dir: &Path) -> Result<PluginInfo, String> {
    load_one(dir, &host_engine(), 0).map(|plugin| plugin.info)
}

/// Copies a plugin directory into `root` and rescans `dirs`. A plugin that fails to compile is
/// refused before anything is written.
pub fn install_from(source: &Path, root: &Path, dirs: &[PathBuf]) -> Result<PluginInfo, String> {
    let folder = source
        .file_name()
        .ok_or_else(|| "选择的是磁盘根目录".to_owned())?;
    let target = root.join(folder);
    if target.exists() {
        return Err(format!("{} 已存在，请先卸载旧版本", target.display()));
    }
    let info = probe(source)?;
    std::fs::create_dir_all(root).map_err(|error| format!("创建插件目录失败：{error}"))?;
    if let Err(error) = copy_plugin_dir(source, &target) {
        let _ = std::fs::remove_dir_all(&target);
        return Err(error);
    }
    load_from_dirs(dirs);
    // The slot is assigned by the rescan, so return the installed copy rather than the probe.
    Ok(list()
        .into_iter()
        .find(|installed| installed.name == info.name)
        .unwrap_or(info))
}

fn copy_plugin_dir(source: &Path, target: &Path) -> Result<(), String> {
    std::fs::create_dir_all(target).map_err(|error| format!("创建目录失败：{error}"))?;
    let entries =
        std::fs::read_dir(source).map_err(|error| format!("读取插件目录失败：{error}"))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("读取插件目录失败：{error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("读取文件类型失败：{error}"))?;
        if file_type.is_dir() {
            return Err("插件目录里不能有子目录".to_owned());
        }
        std::fs::copy(entry.path(), target.join(entry.file_name())).map_err(|error| {
            format!("复制 {} 失败：{error}", entry.file_name().to_string_lossy())
        })?;
    }
    Ok(())
}

/// Removes a plugin that lives under `root`. Plugins next to the program or in an extra
/// directory stay: the user did not put them in the user plugins folder.
pub fn uninstall(slot: u32, root: &Path, dirs: &[PathBuf]) -> Result<(), String> {
    let plugin = get(slot).ok_or_else(|| "插件已经不存在".to_owned())?;
    let root = root
        .canonicalize()
        .map_err(|error| format!("插件目录无效：{error}"))?;
    let dir = plugin
        .info
        .dir
        .canonicalize()
        .map_err(|error| format!("插件目录无效：{error}"))?;
    if !dir.starts_with(&root) {
        return Err("只能卸载用户插件目录里的插件".to_owned());
    }
    std::fs::remove_dir_all(&dir).map_err(|error| format!("删除失败：{error}"))?;
    load_from_dirs(dirs);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_manifest_round_trips() {
        let text = r#"
api = 1
name = "三通道"
version = "0.2.0"
framing = "tail"
tail = "00 00 80 7F"

[[fields]]
key = "scale"
label = "缩放"
kind = "number"
default = 2.5
min = 0.0
max = 10.0
"#;
        let manifest: PluginManifest = toml::from_str(text).unwrap();
        assert_eq!(manifest.name, "三通道");
        assert_eq!(manifest.fields.len(), 1);
        assert_eq!(manifest.fields[0].key, "scale");
        assert_eq!(manifest.fields[0].default, serde_json::json!(2.5));
        let framing = framing_of(&manifest).unwrap();
        match framing {
            Framing::Tail { tail, .. } => assert_eq!(tail, vec![0x00, 0x00, 0x80, 0x7F]),
            other => panic!("unexpected framing {other:?}"),
        }
    }

    #[test]
    fn a_tail_needs_bytes() {
        let manifest = PluginManifest {
            framing: "tail".to_owned(),
            ..PluginManifest::default()
        };
        assert!(framing_of(&manifest).is_err());
    }

    #[test]
    fn a_plugin_from_a_directory_decodes_frames() {
        use crate::pipeline::{DataSession, DecoderSpec, SessionConfig, SessionShared};

        let dir = std::env::temp_dir().join(format!("embdbg-plugin-test-{}", std::process::id()));
        let plugin_dir = dir.join("e2e");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.toml"),
            "api = 1\nname = \"e2e\"\nframing = \"line\"\n",
        )
        .unwrap();
        // A line plugin in WAT: wasmi parses text modules, so the test needs no toolchain.
        std::fs::write(
            plugin_dir.join("decoder.wasm"),
            r#"
            (module
              (memory (export "memory") 1 2)
              (func (export "input_ptr") (result i32) (i32.const 1024))
              (func (export "input_capacity") (result i32) (i32.const 4096))
              (func (export "decode") (param $len i32) (result i32)
                (local $out i32)
                (local.set $out (i32.const 8192))
                (i32.store (local.get $out) (local.get $len))
                (i32.store8 (i32.add (local.get $out) (i32.const 4)) (i32.const 1))
                (memory.copy
                  (i32.add (local.get $out) (i32.const 5))
                  (i32.const 1024)
                  (local.get $len))
                (local.get $out)))
            "#,
        )
        .unwrap();

        assert_eq!(load_from_dirs(std::slice::from_ref(&dir)), 1);
        let slot = slot_of("e2e").expect("the test plugin is installed");
        assert!(matches!(framing(slot), Some(Framing::Lines { .. })));

        let shared = SessionShared::new();
        let mut session = DataSession::new(SessionConfig::stream(DecoderSpec::Plugin(slot)));
        session.on_bytes(1.0, b"hello\n", &shared);
        let lines = shared.lines.read();
        assert_eq!(lines.lines().next().unwrap().text, "hello");
        drop(lines);

        // Installing copies a folder into the user root and keeps the existing slot numbers.
        let source = dir.join("source").join("installed");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("plugin.toml"),
            "api = 1\nname = \"installed\"\nframing = \"line\"\n",
        )
        .unwrap();
        std::fs::copy(plugin_dir.join("decoder.wasm"), source.join("decoder.wasm")).unwrap();
        let user_root = dir.join("user");
        let dirs = vec![dir.clone(), user_root.clone()];

        let before = slot_of("e2e").unwrap();
        let info = install_from(&source, &user_root, &dirs).unwrap();
        assert_eq!(info.name, "installed");
        assert!(user_root.join("installed").join("plugin.toml").is_file());
        assert_eq!(slot_of("e2e"), Some(before), "a rescan keeps old slots");

        let installed_slot = info.slot;
        load_from_dirs(&dirs);
        assert_eq!(get(installed_slot).unwrap().info.name, "installed");

        // Plugins outside the user root are not the app's to delete.
        assert!(uninstall(before, &user_root, &dirs).is_err());
        uninstall(installed_slot, &user_root, &dirs).unwrap();
        assert!(!user_root.join("installed").exists());
        assert!(slot_of("installed").is_none());
        assert!(slot_of("e2e").is_some());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
