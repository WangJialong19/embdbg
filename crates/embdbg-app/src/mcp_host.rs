//! Serving MCP over a local socket, and the `embdbg mcp` command that bridges stdio to it.
//!
//! An AI client launches `embdbg mcp` and talks JSON-RPC over stdio. If a window is running,
//! the bridge forwards to the window's endpoint, so the AI drives the session a person sees.
//! With no window, the same tools are served by an engine in the bridge process.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use embdbg_core::engine::Engine;
use embdbg_core::mcp::{Activity, McpService, ScreenshotPort};
use embdbg_core::pipeline::DEFAULT_CAPACITY;
use interprocess::local_socket::{
    GenericNamespaced, ListenerOptions,
    tokio::{Listener, prelude::*},
};
use tokio::sync::broadcast;

/// The local socket both sides look for. Namespaced, so no file is left behind.
pub const ENDPOINT: &str = "embdbg-mcp";

/// Starts the window's MCP endpoint. Returns the feed the AI panel shows.
pub fn spawn_server(
    engine: Arc<Engine>,
    screenshot: Option<ScreenshotPort>,
) -> broadcast::Receiver<Activity> {
    let (activity, feed) = embdbg_core::mcp::activity_channel();
    engine.runtime().spawn(async move {
        let listener = match listen() {
            Ok(listener) => listener,
            Err(error) => {
                tracing::warn!(target: "embdbg::mcp", "MCP 端点启动失败：{error}");
                return;
            }
        };
        tracing::info!(target: "embdbg::mcp", "MCP 端点已就绪：{ENDPOINT}");
        loop {
            match listener.accept().await {
                Ok(stream) => {
                    let engine = Arc::clone(&engine);
                    let activity = activity.clone();
                    let screenshot = screenshot.clone();
                    tokio::spawn(async move {
                        let mut service = McpService::with_activity(engine, activity);
                        if let Some(port) = &screenshot {
                            service = service.with_screenshot(Arc::clone(port));
                        }
                        let (read, write) = tokio::io::split(stream);
                        match rmcp::serve_server(service, (read, write)).await {
                            Ok(running) => {
                                let _ = running.waiting().await;
                            }
                            Err(error) => {
                                tracing::warn!(target: "embdbg::mcp", "MCP 会话结束：{error}");
                            }
                        }
                    });
                }
                Err(error) => {
                    tracing::warn!(target: "embdbg::mcp", "MCP 接受连接失败：{error}");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    });
    feed
}

fn listen() -> std::io::Result<Listener> {
    let name = ENDPOINT.to_ns_name::<GenericNamespaced>()?;
    ListenerOptions::new().name(name).create_tokio()
}

/// `embdbg mcp`: the AI client's stdio, forwarded to a window when one is running, or served by
/// an engine in this process when none is.
pub fn run_stdio() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("创建 Tokio 运行时失败")?;
    runtime.block_on(async {
        match connect().await {
            Some(stream) => {
                tracing::info!(target: "embdbg::mcp", "接管运行中的窗口");
                bridge(stream).await.context("转发到窗口失败")
            }
            None => {
                tracing::info!(target: "embdbg::mcp", "没有运行中的窗口，无界面模式");
                headless().await.context("无界面 MCP 服务失败")
            }
        }
    })
}

async fn connect() -> Option<LocalSocketStream> {
    let name = ENDPOINT.to_ns_name::<GenericNamespaced>().ok()?;
    LocalSocketStream::connect(name).await.ok()
}

async fn bridge(stream: LocalSocketStream) -> anyhow::Result<()> {
    let (mut from_window, mut to_window) = tokio::io::split(stream);
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    tokio::select! {
        result = tokio::io::copy(&mut stdin, &mut to_window) => { result?; }
        result = tokio::io::copy(&mut from_window, &mut stdout) => { result?; }
    }
    Ok(())
}

async fn headless() -> anyhow::Result<()> {
    let engine = Arc::new(Engine::new(
        tokio::runtime::Handle::current(),
        DEFAULT_CAPACITY,
    ));
    let service = McpService::new(engine);
    let running = rmcp::serve_server(service, rmcp::transport::io::stdio()).await?;
    running.waiting().await?;
    Ok(())
}
