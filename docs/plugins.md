# 解码插件（M4）

插件是一个 WebAssembly 解码器：把一帧原始字节变成通道样本、一行文本，或者直接忽略。它运行在
`embdbg-core` 的 wasmi 沙箱里，只能看见宿主写进它内存的那一帧，没有任何文件、网络或系统调用，
所以一个坏插件最多让当前连接报错，不会影响程序或设备。

## 安装位置

程序启动时按顺序扫描，同名插件以先找到的为准：

1. `%LOCALAPPDATA%\embdbg\plugins\`（Windows）或平台等价目录
2. 可执行文件旁的 `plugins\`

每个插件占一个目录：

```text
%LOCALAPPDATA%\embdbg\plugins\csv-channels\
  plugin.toml
  decoder.wasm
```

`embdbg plugin list` 可以确认是否加载成功；`embdbg plugin dirs` 打印扫描路径。插件在启动时加载，
安装或更新后重启程序。

程序内也可以管理：**设置 → 解码插件**：

- **安装插件…**：选择一个包含 `plugin.toml` 的文件夹，程序先校验清单与 Wasm，再复制到用户插件目录并重新扫描；
- **卸载**：只删除用户插件目录里的插件，需要再点一次确认；程序旁的插件只能手动删；
- **重新扫描**：手工改了插件目录后不用重启即可刷新；
- **打开插件目录**：直接打开文件管理器。

## plugin.toml

```toml
api = 1                       # 必须等于程序支持的版本
name = "CSV 三通道"
version = "0.1.0"
description = "一行逗号分隔的数字变成通道"
entry = "decoder.wasm"        # 相对路径，不能跳出插件目录
framing = "line"              # line | tail | datagram
# tail = "00 00 80 7F"        # framing = "tail" 时必填
max_frame = 4096              # 超长帧丢弃，默认 64 KiB

[[fields]]                    # 声明式参数，界面直接渲染成表单
key = "scale"
label = "缩放倍数"
kind = "number"               # number | integer | bool | text | choice
default = 1.0
min = 0.0
max = 1000.0
# options = ["A", "B"]        # kind = "choice" 时使用
```

参数以 JSON 对象的形式一次性交给插件，例如 `{"scale":2.0}`；连接里改参数会重新实例化插件。

## ABI v1

插件导出以下内容，不能有任何 import（有 import 会实例化失败）：

| 导出 | 作用 |
|---|---|
| `memory` | 线性内存 |
| `input_ptr() -> i32` | 宿主写帧的位置 |
| `input_capacity() -> i32` | 最多写入多少字节 |
| `decode(len: i32) -> i32` | 解码 `input[0..len]`，返回输出块指针，返回 0 表示忽略这一帧 |
| `alloc(size: i32) -> i32` | 可选；有参数时必须提供，返回参数缓冲区 |
| `set_params(ptr: i32, len: i32)` | 可选；有参数时必须提供，读取 UTF-8 JSON |

输出块布局：

```text
[u32 长度（小端）][u8 类型][payload]
类型 0：payload 是 f32 小端样本
类型 1：payload 是 UTF-8 文本行
类型 3：payload 是错误信息（UTF-8），连接面板显示为红字
```

限制：单帧最多写 64 KiB 输出；线性内存上限 8 MiB；每次 `decode` 最多 200 万条 wasmi 指令，
超出会以“燃料耗尽”报错；所有失败都只影响这一帧。

## 写一个插件

Rust 示例见 `examples/plugins/csv-channels`：

```bash
rustup target add wasm32-unknown-unknown
cd examples/plugins/csv-channels
cargo build --release --target wasm32-unknown-unknown
mkdir -p "$LOCALAPPDATA/embdbg/plugins/csv-channels"
cp plugin.toml "$LOCALAPPDATA/embdbg/plugins/csv-channels/"
cp target/wasm32-unknown-unknown/release/csv_channels.wasm \
   "$LOCALAPPDATA/embdbg/plugins/csv-channels/decoder.wasm"
```

C 示例见 `examples/plugins/c-csv`，构建命令写在源文件顶部（`clang --target=wasm32 -nostdlib`）。

装好后重启程序，在左侧「当前连接的协议」下拉框里选「插件：…」，下方会按 `[[fields]]` 生成参数表单；
插件报错会以红字显示在同一处。
