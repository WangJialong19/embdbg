# embdbg · 嵌入式调试助手

一款用 Rust 写的桌面调试工具，目标是把嵌入式工程师分散在好几种软件里的活收拢到一处：

- **串口与网络统一接入**：串口、TCP/UDP、WebSocket、MQTT、Modbus TCP/RTU。
- **数据看得见**：实时波形、X-Y 相图、实时 FFT、直方图统计。
- **网络调试内置**：常用协议收发、Ping、端口扫描、DNS、局域网发现。
- **给 AI 用**：内置 MCP 服务，让 AI 能直接开关串口、收发数据、读取样本与 FFT 结果，用来验证固件。
- **可装插件**：用户自己写的 WASM 解码器，装进程序就能用。

当前进度：**M0 已完成**（见下方“进度”）。详细方案见会话记录中的实施方案。

## 构建与运行

需要 Rust stable（1.95 以上，本机实测 1.98.1）与 MSVC 生成工具（VS 2022）。字体已随仓库提交，不需要额外下载。

```sh
cargo build --release          # 产物：target/release/embdbg.exe
cargo test --workspace         # 全部测试
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

直接在开发机上运行：

```sh
target/release/embdbg.exe --sim          # 打开内置模拟信号源（8 通道，1 MS/s）
target/release/embdbg.exe                # 空界面
```

命令参数：

| 参数 | 含义 |
|---|---|
| `--renderer auto\|glow\|wgpu\|dx12\|vulkan` | 本次运行的渲染后端，覆盖设置 |
| `--max-fps N` | 数据流入时的帧率上限，0 为跟随显示器 |
| `--sim` | 启动时打开模拟信号源 |
| `--open-settings` | 启动时打开设置窗口 |
| `--screenshot FILE` | 启动约两秒后把窗口存成 PNG 并退出 |
| `--perf-test SECS --perf-report FILE` | 帧率自检，结果写成 `key = value` 文本 |
| `--coarse-timers` | 不申请 1 ms 系统定时精度，更省电 |
| `--serial PORT` | 界面启动时直接打开串口，如 `--serial COM5` |
| `--serial-baud N` | 配合 `--serial` 的波特率（默认 115200） |
| `--serial-decoder NAME` | 配合 `--serial` 的协议：`line`/`justfloat`/`justint` |
| `embdbg ports` | 列出本机串口并退出 |
| `embdbg serial PORT [--baud N] [--decoder NAME] [--listen S] [--send TEXT] [--hex-send HEX]` | 无界面串口自检：收发并打印解码结果与统计 |
| `embdbg mcp` | 以 MCP 服务运行：有窗口时接管当前会话，否则无界面运行 |

设置、布局与日志：`%LOCALAPPDATA%\embdbg\config.toml`、`layout.json`、`logs\`。连接表单、发送宏与窗口布局都会记住。

## MCP：让 AI 操作这台调试助手

在 AI 客户端里把 MCP 命令配置成 `embdbg mcp`（stdio 传输）：

- 有窗口在运行时，`mcp` 进程通过本机命名管道连到窗口，AI 的每一步都发生在你眼前的会话里，并在「AI」页签留下调用记录；
- 没有窗口时，同一个命令在进程内无界面启动引擎，工具集不变；
- 端点只在本机（Windows 命名管道 / Unix 本地套接字），不监听网络端口。

| 类别 | 工具 |
|---|---|
| 连接 | `list_serial_ports`、`open_connection`（serial / tcp-client / tcp-server / simulator）、`close_connection`、`list_connections` |
| 收发 | `send`、`read_lines`、`read_terminal`、`wait_for`、`send_and_wait` |
| 分析 | `list_channels`、`read_samples`、`get_fft`、`get_histogram`、`get_stats` |
| 会话 | `screenshot`（仅窗口模式：把 PNG 存到临时目录并返回路径） |

典型用法：让 AI 打开模拟源、读通道、算 FFT 峰值并截图；或打开串口发一条 AT 指令，`wait_for` 到 `OK` 后再读终端。

客户端配置示例（路径换成你的 release 可执行文件）：

```json
{
  "mcpServers": {
    "embdbg": {
      "command": "E:\\home\\hugo\\调试助手\\target\\release\\embdbg.exe",
      "args": ["mcp"]
    }
  }
}
```

## 界面与渲染后端

默认 **自动**：Windows/Linux 上优先 OpenGL，系统建不出 OpenGL 3.3 上下文时自动改用 wgpu（wgpu 内优先 DX12）；macOS 优先 Metal。设置窗口（标题栏齿轮）里可以手动锁定 OpenGL / wgpu / DirectX 12 / Vulkan，换后端需要重启。

选择依据是开发机（Windows 11、Intel Arc 140T 核显、150% 缩放、120 Hz 屏）上的实测，同一界面、8 通道 × 1 MS/s 流入：

| 后端 | 空闲内存（工作集 / 专用） | 流入时帧率 | 流入时 CPU（单核 100%） | 每帧渲染 CPU |
|---|---|---|---|---|
| OpenGL（默认） | 108 / 54 MB | 120 FPS | 30% | 约 1.2 ms |
| wgpu / DirectX 12 | 265 / 222 MB | 120 FPS | 42% | 约 3 ms |
| wgpu / Vulkan | 344 / 334 MB* | 120 FPS | 111%* | 约 8 ms* |

\* Vulkan 在这块 Intel 核显上：空闲空转 27% CPU（因为程序申请了 1 ms 定时精度），每帧多耗约 5 ms。所以 Windows 上默认不再让 wgpu 自己挑 Vulkan。

另外说明：`wgpu` 这套图形栈的固定开销就在 300 MB 上下（最小 eframe 空窗口同样如此），与数据缓冲无关；程序自身的采样缓冲按 4 字节/样本计，8 通道各 100 万样本约占 37 MB。

帧率上限默认“跟随显示器”：定时唤醒有 1–2 ms 抖动，设固定上限在高刷新率屏幕上反而会错过垂直同步点、造成节奏不匀。

## 目录结构

```
crates/
  embdbg-core/   引擎：不依赖任何界面库，可无界面运行（MCP、测试都用它）
    src/pipeline/  通道环、min/max 金字塔抽稀、帧↔时间映射
    src/transport/ 数据源（串口、TCP、UDP、内置模拟源）
    src/platform.rs Windows 定时精度
  embdbg-app/    eframe/egui 界面
    src/plots/     自绘波形图与坐标轴
    src/panels/    连接、通道表、日志、设置
assets/fonts/    内嵌的思源黑体子集与 Tabler 图标子集（含许可证）
tools/fonts/     字体子集构建脚本与说明
tools/measure.ps1 内存/CPU 采样脚本
```

## 进度

- [x] **M0 骨架**：workspace、tokio 与 eframe 集成、主题与中文字体、停靠布局、自绘波形图、模拟信号源、渲染后端选择与回退、设置窗口、截图与性能自检。
- [x] **M1 数据主链路**：串口与 TCP 客户端/服务器、JustFloat/JustInt/行协议、波形/X-Y/频谱/直方图、终端与十六进制视图、发送面板（文本/HEX/换行/定时/历史）。
  - 串口已实机验证：CH340（COM5，115200）的 Linux 控制台 + 1 Hz GPS/CAN 日志，终端与十六进制视图、无界面收发与统计均正常。
  - 连接面板按 VOFA 的方式组织：顶部「数据接口」选择串口 / TCP 客户端 / TCP 服务器 / UDP / 模拟信号源，只显示所选接口的参数，再往下的连接列表与协议不变。
  - 终端是 VT100 仿真：ANSI 颜色、光标、回车覆盖整行、滚轮回看历史、按键直通（方向键、Ctrl 组合、粘贴），输入法候选窗跟随光标；鼠标拖选或双击选词后 Ctrl+Shift+C 复制，选中期间画面暂停刷新，点击恢复。
  - 窗口布局会记住：页签位置、活动页签、分栏比例、通道面板的开关都存在 `%LOCALAPPDATA%\embdbg\layout.json`，下次启动原样恢复。
  - 连接表单（接口、端口、波特率、数据位/校验/停止位/流控、TCP 地址）与发送宏存入 `config.toml`，重启后还在。
- [x] **M2 MCP**：rmcp 3.11 服务、15 个工具、stdio 桥、命名管道接管窗口、无界面模式、AI 活动页签与截图工具。
  - 已用脚本端到端验证两条路径：无窗口时进程内服务；有窗口时 `embdbg mcp` 接管会话，工具调用会出现在界面的「AI」页签里。
  - 还需用真实 AI 客户端（ZCode）走一遍工具调用，并把"AI 写操作"的权限开关放到 M3 的网络工具里一起做。
- [ ] **M3 网络**（进行中）：UDP 已接入（界面、MCP 工具、按数据报分帧）；WebSocket、MQTT、Modbus 与网络小工具待做。
- [ ] **M4 WASM 插件**。
- [ ] **M5 打磨发布**。

## 字体

界面内嵌 Noto Sans SC 子集（OFL 1.1，7532 字，2.24 MB）与 Tabler Icons 子集（MIT），换机器显示一致。重新生成见 [tools/fonts/README.md](tools/fonts/README.md)。
