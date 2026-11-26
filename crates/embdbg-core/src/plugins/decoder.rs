//! The host side of the plugin ABI: one sandboxed instance per connection.

use wasmi::{Instance, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc};

use super::{FUEL_PER_FRAME, MAX_OUTPUT, MEMORY_LIMIT, Plugin};

/// How many parameters the host accepts: enough for a form, far from a memory attack.
const MAX_PARAMS: usize = 16 * 1024;

/// One frame's decoding.
#[derive(Debug, Clone, PartialEq)]
pub enum PluginOutput {
    /// Channel samples, in order.
    Samples(Vec<f32>),
    /// One line of text.
    Line(Vec<u8>),
    /// The plugin does not recognise this frame.
    Ignored,
}

/// Everything a worker needs from a store: just the resource limits.
struct Sandbox {
    limits: StoreLimits,
}

/// A live plugin instance with its own memory and fuel counter.
pub struct PluginDecoder {
    store: Store<Sandbox>,
    memory: Memory,
    decode: TypedFunc<i32, i32>,
    input_ptr: usize,
    input_capacity: usize,
    /// Set after a trap or a protocol violation; the connection reports it instead of retrying
    /// every frame.
    error: Option<String>,
}

impl std::fmt::Debug for PluginDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginDecoder")
            .field("input_ptr", &self.input_ptr)
            .field("input_capacity", &self.input_capacity)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl PluginDecoder {
    /// Instantiates a plugin and hands it the connection's JSON parameters.
    pub fn new(plugin: &Plugin, params: &str) -> Result<Self, String> {
        let module: &Module = plugin.module();
        let limits = StoreLimitsBuilder::new()
            .memory_size(MEMORY_LIMIT)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(module.engine(), Sandbox { limits });
        store.limiter(|sandbox| &mut sandbox.limits);
        store
            .set_fuel(FUEL_PER_FRAME)
            .map_err(|error| format!("插件沙箱没有启用燃料计量：{error}"))?;

        // No host functions are linked: anything the module imports makes instantiation fail,
        // which is the point.
        let linker = wasmi::Linker::new(module.engine());
        let instance: Instance = linker
            .instantiate_and_start(&mut store, module)
            .map_err(|error| format!("实例化插件失败：{error}"))?;

        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| "插件没有导出 memory".to_owned())?;
        let input_ptr = instance
            .get_typed_func::<(), i32>(&store, "input_ptr")
            .map_err(|_| "插件缺少 input_ptr()".to_owned())?;
        let input_capacity = instance
            .get_typed_func::<(), i32>(&store, "input_capacity")
            .map_err(|_| "插件缺少 input_capacity()".to_owned())?;
        let decode = instance
            .get_typed_func::<i32, i32>(&store, "decode")
            .map_err(|_| "插件缺少 decode(len)".to_owned())?;

        let input_ptr = input_ptr
            .call(&mut store, ())
            .map_err(|error| format!("读取 input_ptr 失败：{error}"))?;
        let input_capacity = input_capacity
            .call(&mut store, ())
            .map_err(|error| format!("读取 input_capacity 失败：{error}"))?;
        if input_ptr < 0 || input_capacity <= 0 {
            return Err("插件的输入缓冲区无效".to_owned());
        }
        let input_ptr = input_ptr as usize;
        let input_capacity = input_capacity as usize;

        let mut decoder = Self {
            store,
            memory,
            decode,
            input_ptr,
            input_capacity,
            error: None,
        };
        if !params.is_empty() && params != "{}" {
            decoder.pass_params(&instance, params)?;
        }
        Ok(decoder)
    }

    fn pass_params(&mut self, instance: &Instance, params: &str) -> Result<(), String> {
        if params.len() > MAX_PARAMS {
            return Err("插件参数过大".to_owned());
        }
        let alloc = instance
            .get_typed_func::<i32, i32>(&self.store, "alloc")
            .map_err(|_| "插件有参数但没有导出 alloc(size)".to_owned())?;
        let set_params = instance
            .get_typed_func::<(i32, i32), ()>(&self.store, "set_params")
            .map_err(|_| "插件有参数但没有导出 set_params(ptr, len)".to_owned())?;
        let ptr = alloc
            .call(&mut self.store, params.len() as i32)
            .map_err(|error| format!("分配参数缓冲区失败：{error}"))?;
        if ptr <= 0 {
            return Err("插件拒绝分配参数缓冲区".to_owned());
        }
        self.memory
            .write(&mut self.store, ptr as usize, params.as_bytes())
            .map_err(|error| format!("写入插件参数失败：{error}"))?;
        set_params
            .call(&mut self.store, (ptr, params.len() as i32))
            .map_err(|error| format!("set_params 失败：{error}"))
    }

    pub fn input_capacity(&self) -> usize {
        self.input_capacity
    }

    /// The last failure, for the connection panel. Successful frames do not clear it; the plugin
    /// stays suspect until the connection is reopened.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Decodes one frame in the sandbox.
    pub fn decode(&mut self, frame: &[u8]) -> Result<PluginOutput, String> {
        if frame.len() > self.input_capacity {
            return Err(format!(
                "帧有 {} 字节，超过插件的 {} 字节输入缓冲区",
                frame.len(),
                self.input_capacity
            ));
        }
        self.memory
            .write(&mut self.store, self.input_ptr, frame)
            .map_err(|error| format!("写入插件输入失败：{error}"))?;
        self.store
            .set_fuel(FUEL_PER_FRAME)
            .map_err(|error| format!("设置燃料失败：{error}"))?;
        let ptr = match self.decode.call(&mut self.store, frame.len() as i32) {
            Ok(ptr) => ptr,
            Err(error) => {
                let message = format!("插件解码失败（超出燃料或陷阱）：{error}");
                self.error = Some(message.clone());
                return Err(message);
            }
        };
        if ptr == 0 {
            return Ok(PluginOutput::Ignored);
        }
        if ptr < 0 {
            return Err("插件返回了无效的输出指针".to_owned());
        }
        let ptr = ptr as usize;

        let mut header = [0u8; 5];
        self.memory
            .read(&self.store, ptr, &mut header)
            .map_err(|error| format!("读取插件输出头失败：{error}"))?;
        let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        if len > MAX_OUTPUT {
            return Err(format!("插件输出 {len} 字节，超过上限 {MAX_OUTPUT}"));
        }
        let mut payload = vec![0u8; len];
        self.memory
            .read(&self.store, ptr + 5, &mut payload)
            .map_err(|error| format!("读取插件输出失败：{error}"))?;
        match header[4] {
            0 => {
                if !payload.len().is_multiple_of(4) {
                    return Err("插件返回的样本长度不是 4 的倍数".to_owned());
                }
                let samples = payload
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|bytes| f32::from_le_bytes(*bytes))
                    .collect();
                Ok(PluginOutput::Samples(samples))
            }
            1 => Ok(PluginOutput::Line(payload)),
            3 => Err(String::from_utf8_lossy(&payload).into_owned()),
            other => Err(format!("插件返回了未知的输出类型 {other}")),
        }
    }
}

/// A plugin used by the tests: its module is written as WAT, so no toolchain is needed.
#[cfg(test)]
fn wat_plugin(slot: u32, wat: &str) -> std::sync::Arc<Plugin> {
    let mut config = wasmi::Config::default();
    config.consume_fuel(true);
    let engine = wasmi::Engine::new(&config);
    let module = wasmi::Module::new(&engine, wat.as_bytes()).expect("the test module is valid");
    std::sync::Arc::new(Plugin {
        info: super::PluginInfo {
            slot,
            name: "test".to_owned(),
            version: "0".to_owned(),
            description: String::new(),
            fields: Vec::new(),
            dir: std::path::PathBuf::new(),
        },
        framing: crate::pipeline::Framing::default(),
        module,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE_PLUGIN: &str = r#"
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
    "#;

    #[test]
    fn a_line_plugin_sees_its_frame() {
        let plugin = wat_plugin(0, LINE_PLUGIN);
        let mut decoder = PluginDecoder::new(&plugin, "").unwrap();
        let output = decoder.decode(b"hello").unwrap();
        assert_eq!(output, PluginOutput::Line(b"hello".to_vec()));
    }

    #[test]
    fn a_frame_larger_than_the_input_buffer_is_refused() {
        let plugin = wat_plugin(0, LINE_PLUGIN);
        let mut decoder = PluginDecoder::new(&plugin, "").unwrap();
        let frame = vec![0u8; 4097];
        assert!(decoder.decode(&frame).is_err());
    }

    #[test]
    fn a_plugin_without_a_frame_bound_is_ignored() {
        let wat = r#"
            (module
              (memory (export "memory") 1)
              (func (export "input_ptr") (result i32) (i32.const 0))
              (func (export "input_capacity") (result i32) (i32.const 16))
              (func (export "decode") (param i32) (result i32) (i32.const 0)))
        "#;
        let plugin = wat_plugin(0, wat);
        let mut decoder = PluginDecoder::new(&plugin, "").unwrap();
        assert_eq!(decoder.decode(b"x").unwrap(), PluginOutput::Ignored);
    }

    #[test]
    fn an_endless_loop_runs_out_of_fuel() {
        let wat = r#"
            (module
              (memory (export "memory") 1)
              (func (export "input_ptr") (result i32) (i32.const 0))
              (func (export "input_capacity") (result i32) (i32.const 16))
              (func (export "decode") (param i32) (result i32)
                (loop $spin (br $spin))
                (i32.const 0)))
        "#;
        let plugin = wat_plugin(0, wat);
        let mut decoder = PluginDecoder::new(&plugin, "").unwrap();
        let error = decoder.decode(b"x").unwrap_err();
        assert!(error.contains("燃料"), "{error}");
        assert!(decoder.error().is_some());
    }

    #[test]
    fn growing_past_the_memory_limit_traps() {
        let wat = r#"
            (module
              (memory (export "memory") 1)
              (func (export "input_ptr") (result i32) (i32.const 0))
              (func (export "input_capacity") (result i32) (i32.const 16))
              (func (export "decode") (param i32) (result i32)
                (drop (memory.grow (i32.const 2000)))
                (i32.const 0)))
        "#;
        let plugin = wat_plugin(0, wat);
        let mut decoder = PluginDecoder::new(&plugin, "").unwrap();
        // 2000 pages is 128 MiB; the 8 MiB limit has to stop it.
        assert!(decoder.decode(b"x").is_err());
    }

    #[test]
    fn parameters_reach_the_plugin() {
        let wat = r#"
            (module
              (memory (export "memory") 1)
              (global $len (mut i32) (i32.const 0))
              (func (export "input_ptr") (result i32) (i32.const 1024))
              (func (export "input_capacity") (result i32) (i32.const 4096))
              (func (export "alloc") (param i32) (result i32) (i32.const 16384))
              (func (export "set_params") (param $ptr i32) (param $len i32)
                (global.set $len (local.get $len)))
              (func (export "decode") (param $len i32) (result i32)
                (local $out i32)
                (local.set $out (i32.const 8192))
                (i32.store (local.get $out) (i32.const 4))
                (i32.store8 (i32.add (local.get $out) (i32.const 4)) (i32.const 0))
                (f32.store
                  (i32.add (local.get $out) (i32.const 5))
                  (f32.convert_i32_u (global.get $len)))
                (local.get $out)))
        "#;
        let plugin = wat_plugin(0, wat);
        let params = r#"{"scale":2}"#;
        let mut decoder = PluginDecoder::new(&plugin, params).unwrap();
        let output = decoder.decode(b"x").unwrap();
        let PluginOutput::Samples(samples) = output else {
            panic!("expected samples");
        };
        assert_eq!(samples, vec![params.len() as f32]);
    }

    #[test]
    fn parameters_without_an_entry_point_are_an_error() {
        let plugin = wat_plugin(0, LINE_PLUGIN);
        assert!(PluginDecoder::new(&plugin, r#"{"a":1}"#).is_err());
    }
}
