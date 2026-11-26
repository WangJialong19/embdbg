//! Network tools: DNS, TCP reachability, port scanning and plain HTTP requests.
//!
//! Each tool runs as a task on the engine's runtime and streams text lines back to the panel, so a
//! scan or a ping shows progress while it runs. The tools are read-only probes; nothing here
//! changes the device under test.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{ComboBox, DragValue, RichText, ScrollArea, TextEdit, Ui};
use embdbg_core::engine::Engine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::UnboundedSender;

use crate::theme::{self, FG_DIM, SPACE_1};

/// Which probe the panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tool {
    #[default]
    Dns,
    Ping,
    Scan,
    Http,
}

impl Tool {
    const ALL: [Self; 4] = [Self::Dns, Self::Ping, Self::Scan, Self::Http];

    fn label(self) -> &'static str {
        match self {
            Self::Dns => "DNS 查询",
            Self::Ping => "TCP 连通性",
            Self::Scan => "端口扫描",
            Self::Http => "HTTP 请求",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum HttpMethod {
    #[default]
    Get,
    Post,
    Put,
    Delete,
}

impl HttpMethod {
    const ALL: [Self; 4] = [Self::Get, Self::Post, Self::Put, Self::Delete];

    fn label(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Delete => "DELETE",
        }
    }
}

/// A running probe and the lines it has produced so far.
#[derive(Debug)]
struct Job {
    lines: tokio::sync::mpsc::UnboundedReceiver<String>,
}

#[derive(Debug)]
pub struct NetToolsState {
    tool: Tool,
    dns_host: String,
    ping_host: String,
    ping_port: String,
    ping_count: u32,
    ping_interval_ms: u32,
    ping_timeout_ms: u32,
    scan_host: String,
    scan_start: u16,
    scan_end: u16,
    scan_timeout_ms: u32,
    scan_concurrency: usize,
    http_url: String,
    http_method: HttpMethod,
    http_headers: String,
    http_body: String,
    http_timeout_ms: u32,
    output: Vec<String>,
    job: Option<Job>,
    running: bool,
}

impl Default for NetToolsState {
    fn default() -> Self {
        Self {
            tool: Tool::default(),
            dns_host: "example.com".to_owned(),
            ping_host: "192.168.1.1".to_owned(),
            ping_port: "80".to_owned(),
            ping_count: 4,
            ping_interval_ms: 500,
            ping_timeout_ms: 1000,
            scan_host: "192.168.1.1".to_owned(),
            scan_start: 1,
            scan_end: 1024,
            scan_timeout_ms: 300,
            scan_concurrency: 64,
            http_url: "http://192.168.1.1/".to_owned(),
            http_method: HttpMethod::default(),
            http_headers: String::new(),
            http_body: String::new(),
            http_timeout_ms: 5000,
            output: Vec::new(),
            job: None,
            running: false,
        }
    }
}

impl NetToolsState {
    fn start(&mut self, job: Job) {
        self.output.clear();
        self.job = Some(job);
        self.running = true;
    }

    /// Takes whatever the running job produced; finishes when its sender goes away.
    fn poll(&mut self) {
        let Some(mut job) = self.job.take() else {
            return;
        };
        loop {
            match job.lines.try_recv() {
                Ok(line) => {
                    self.output.push(line);
                    if self.output.len() > 5000 {
                        self.output.remove(0);
                    }
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                    self.job = Some(job);
                    return;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    self.running = false;
                    return;
                }
            }
        }
    }
}

pub fn ui(ui: &mut Ui, state: &mut NetToolsState, engine: &Engine) {
    state.poll();

    ui.horizontal(|ui| {
        ui.label(RichText::new("功能").small().color(FG_DIM));
        ComboBox::from_id_salt("net-tool")
            .selected_text(state.tool.label())
            .show_ui(ui, |ui| {
                for tool in Tool::ALL {
                    ui.selectable_value(&mut state.tool, tool, tool.label());
                }
            });
        if state.running {
            ui.spinner();
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("清空结果").clicked() {
                state.output.clear();
            }
        });
    });
    ui.separator();

    match state.tool {
        Tool::Dns => dns_form(ui, state, engine),
        Tool::Ping => ping_form(ui, state, engine),
        Tool::Scan => scan_form(ui, state, engine),
        Tool::Http => http_form(ui, state, engine),
    }

    ui.add_space(SPACE_1);
    ui.separator();
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            if state.output.is_empty() {
                ui.label(RichText::new("结果会显示在这里。").small().color(FG_DIM));
            }
            for line in &state.output {
                ui.label(RichText::new(line).monospace().color(theme::FG));
            }
        });
}

fn dns_form(ui: &mut Ui, state: &mut NetToolsState, engine: &Engine) {
    ui.horizontal(|ui| {
        ui.label("主机名");
        ui.add(
            TextEdit::singleline(&mut state.dns_host)
                .desired_width(240.0)
                .hint_text("example.com 或 192.168.1.1"),
        );
        if ui
            .add_enabled(!state.running, egui::Button::new("查询"))
            .clicked()
        {
            let host = state.dns_host.trim().to_owned();
            if host.is_empty() {
                state.output.push("请输入主机名".to_owned());
                return;
            }
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            engine
                .runtime()
                .spawn(async move { resolve(host, tx).await });
            state.start(Job { lines: rx });
        }
    });
}

async fn resolve(host: String, out: UnboundedSender<String>) {
    match tokio::net::lookup_host((host.as_str(), 0)).await {
        Ok(addresses) => {
            let mut v4: Vec<IpAddr> = Vec::new();
            let mut v6: Vec<IpAddr> = Vec::new();
            for address in addresses {
                let ip = address.ip();
                let target = if ip.is_ipv4() { &mut v4 } else { &mut v6 };
                if !target.contains(&ip) {
                    target.push(ip);
                }
            }
            let _ = out.send(format!("{host} 解析到 {} 个地址：", v4.len() + v6.len()));
            for ip in v4.into_iter().chain(v6) {
                let _ = out.send(format!("  {ip}"));
            }
        }
        Err(error) => {
            let _ = out.send(format!("解析 {host} 失败：{error}"));
        }
    }
}

fn ping_form(ui: &mut Ui, state: &mut NetToolsState, engine: &Engine) {
    ui.horizontal(|ui| {
        ui.label("主机");
        ui.add(
            TextEdit::singleline(&mut state.ping_host)
                .desired_width(160.0)
                .hint_text("192.168.1.1"),
        );
        ui.label("端口");
        ui.add(TextEdit::singleline(&mut state.ping_port).desired_width(56.0));
        ui.label("次数");
        ui.add(DragValue::new(&mut state.ping_count).range(1..=20));
        ui.label("间隔");
        ui.add(
            DragValue::new(&mut state.ping_interval_ms)
                .range(100..=5000)
                .suffix(" ms"),
        );
        ui.label("超时");
        ui.add(
            DragValue::new(&mut state.ping_timeout_ms)
                .range(100..=10_000)
                .suffix(" ms"),
        );
        if ui
            .add_enabled(!state.running, egui::Button::new("开始"))
            .clicked()
        {
            let host = state.ping_host.trim().to_owned();
            let port = match parse_port(&state.ping_port) {
                Ok(port) => port,
                Err(error) => {
                    state.output.push(error);
                    return;
                }
            };
            if host.is_empty() {
                state.output.push("请输入主机".to_owned());
                return;
            }
            let count = state.ping_count;
            let interval = state.ping_interval_ms;
            let timeout = state.ping_timeout_ms;
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            engine
                .runtime()
                .spawn(async move { tcp_ping(host, port, count, interval, timeout, tx).await });
            state.start(Job { lines: rx });
        }
    });
}

async fn tcp_ping(
    host: String,
    port: u16,
    count: u32,
    interval_ms: u32,
    timeout_ms: u32,
    out: UnboundedSender<String>,
) {
    let _ = out.send(format!("连接 {host}:{port} 共 {count} 次"));
    let mut times = Vec::new();
    for attempt in 1..=count {
        let started = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_millis(u64::from(timeout_ms)),
            TcpStream::connect((host.as_str(), port)),
        )
        .await;
        let line = match result {
            Ok(Ok(stream)) => {
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                times.push(ms);
                drop(stream);
                format!("[{attempt}/{count}] {ms:.1} ms")
            }
            Ok(Err(error)) => format!("[{attempt}/{count}] 失败：{error}"),
            Err(_) => format!("[{attempt}/{count}] 超时（{timeout_ms} ms）"),
        };
        let _ = out.send(line);
        if attempt < count {
            tokio::time::sleep(Duration::from_millis(u64::from(interval_ms))).await;
        }
    }
    if times.is_empty() {
        let _ = out.send("全部失败".to_owned());
    } else {
        let min = times.iter().copied().fold(f64::INFINITY, f64::min);
        let max = times.iter().copied().fold(0.0, f64::max);
        let average = times.iter().sum::<f64>() / times.len() as f64;
        let _ = out.send(format!(
            "完成：成功 {}/{}，最小 {min:.1} ms，平均 {average:.1} ms，最大 {max:.1} ms",
            times.len(),
            count
        ));
    }
}

fn scan_form(ui: &mut Ui, state: &mut NetToolsState, engine: &Engine) {
    ui.horizontal(|ui| {
        ui.label("主机");
        ui.add(
            TextEdit::singleline(&mut state.scan_host)
                .desired_width(150.0)
                .hint_text("192.168.1.1"),
        );
        ui.label("端口");
        ui.add(DragValue::new(&mut state.scan_start).range(1..=65535));
        ui.label("到");
        ui.add(DragValue::new(&mut state.scan_end).range(1..=65535));
        ui.label("超时");
        ui.add(
            DragValue::new(&mut state.scan_timeout_ms)
                .range(50..=5000)
                .suffix(" ms"),
        );
        ui.label("并发");
        ui.add(DragValue::new(&mut state.scan_concurrency).range(1..=256));
        if ui
            .add_enabled(!state.running, egui::Button::new("扫描"))
            .clicked()
        {
            let host = state.scan_host.trim().to_owned();
            if host.is_empty() {
                state.output.push("请输入主机".to_owned());
                return;
            }
            if state.scan_start > state.scan_end {
                state.output.push("起始端口不能大于结束端口".to_owned());
                return;
            }
            if u32::from(state.scan_end) - u32::from(state.scan_start) > 10_000 {
                state
                    .output
                    .push("一次最多扫描 10001 个端口；请缩小范围".to_owned());
                return;
            }
            let (start, end) = (state.scan_start, state.scan_end);
            let timeout = state.scan_timeout_ms;
            let concurrency = state.scan_concurrency;
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            engine
                .runtime()
                .spawn(async move { scan_ports(host, start, end, timeout, concurrency, tx).await });
            state.start(Job { lines: rx });
        }
    });
}

async fn scan_ports(
    host: String,
    start: u16,
    end: u16,
    timeout_ms: u32,
    concurrency: usize,
    out: UnboundedSender<String>,
) {
    let _ = out.send(format!(
        "扫描 {host}:{start}..{end}，并发 {concurrency}，超时 {timeout_ms} ms"
    ));
    let started = Instant::now();
    let semaphore = Arc::new(Semaphore::new(concurrency.clamp(1, 256)));
    let mut tasks = Vec::new();
    for port in start..=end {
        let Ok(permit) = Arc::clone(&semaphore).acquire_owned().await else {
            break;
        };
        let host = host.clone();
        let out = out.clone();
        tasks.push(tokio::spawn(async move {
            let _permit = permit;
            let result = tokio::time::timeout(
                Duration::from_millis(u64::from(timeout_ms)),
                TcpStream::connect((host.as_str(), port)),
            )
            .await;
            if let Ok(Ok(stream)) = result {
                drop(stream);
                let _ = out.send(format!("开放 {port}"));
                true
            } else {
                false
            }
        }));
    }
    let mut open = 0u32;
    for task in tasks {
        if task.await.unwrap_or(false) {
            open += 1;
        }
    }
    let _ = out.send(format!(
        "扫描完成：{open} 个开放端口，用时 {:.2} s",
        started.elapsed().as_secs_f64()
    ));
}

fn http_form(ui: &mut Ui, state: &mut NetToolsState, engine: &Engine) {
    ui.horizontal(|ui| {
        ComboBox::from_id_salt("http-method")
            .selected_text(state.http_method.label())
            .show_ui(ui, |ui| {
                for method in HttpMethod::ALL {
                    ui.selectable_value(&mut state.http_method, method, method.label());
                }
            });
        ui.add(
            TextEdit::singleline(&mut state.http_url)
                .desired_width(360.0)
                .hint_text("http://192.168.1.1/api"),
        );
        ui.label("超时");
        ui.add(
            DragValue::new(&mut state.http_timeout_ms)
                .range(200..=60_000)
                .suffix(" ms"),
        );
        if ui
            .add_enabled(!state.running, egui::Button::new("发送"))
            .clicked()
        {
            let url = state.http_url.trim().to_owned();
            if url.is_empty() {
                state.output.push("请输入 URL".to_owned());
                return;
            }
            let method = state.http_method.label().to_owned();
            let headers = state.http_headers.clone();
            let body = state.http_body.clone();
            let timeout = state.http_timeout_ms;
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            engine
                .runtime()
                .spawn(async move { http_request(method, url, headers, body, timeout, tx).await });
            state.start(Job { lines: rx });
        }
    });
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(
                RichText::new("请求头（每行 Key: Value）")
                    .small()
                    .color(FG_DIM),
            );
            ui.add(
                TextEdit::multiline(&mut state.http_headers)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(3)
                    .desired_width(360.0)
                    .hint_text("Content-Type: application/json"),
            );
        });
        ui.vertical(|ui| {
            ui.label(RichText::new("请求体").small().color(FG_DIM));
            ui.add(
                TextEdit::multiline(&mut state.http_body)
                    .font(egui::TextStyle::Monospace)
                    .desired_rows(3)
                    .desired_width(360.0)
                    .hint_text("{\"cmd\":\"status\"}"),
            );
        });
    });
}

async fn http_request(
    method: String,
    url: String,
    headers: String,
    body: String,
    timeout_ms: u32,
    out: UnboundedSender<String>,
) {
    let (host, port, path) = match parse_http_url(&url) {
        Ok(parts) => parts,
        Err(error) => {
            let _ = out.send(error);
            return;
        }
    };
    let started = Instant::now();
    let connect = tokio::time::timeout(
        Duration::from_millis(u64::from(timeout_ms)),
        TcpStream::connect((host.as_str(), port)),
    )
    .await;
    let mut stream = match connect {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            let _ = out.send(format!("连接 {host}:{port} 失败：{error}"));
            return;
        }
        Err(_) => {
            let _ = out.send(format!("连接 {host}:{port} 超时"));
            return;
        }
    };

    let mut request = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for line in headers.lines().filter(|line| !line.trim().is_empty()) {
        request.push_str(line.trim());
        request.push_str("\r\n");
    }
    if !body.is_empty() && !headers.to_ascii_lowercase().contains("content-length:") {
        request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    request.push_str("\r\n");
    request.push_str(&body);
    if let Err(error) = stream.write_all(request.as_bytes()).await {
        let _ = out.send(format!("发送请求失败：{error}"));
        return;
    }

    let deadline = started + Duration::from_millis(u64::from(timeout_ms));
    let mut response = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let _ = out.send("读取响应超时".to_owned());
            break;
        }
        match tokio::time::timeout(remaining, stream.read(&mut buffer)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(read)) => {
                response.extend_from_slice(&buffer[..read]);
                if response.len() > 512 * 1024 {
                    let _ = out.send("响应超过 512 KB，已截断".to_owned());
                    break;
                }
            }
            Ok(Err(error)) => {
                let _ = out.send(format!("读取响应失败：{error}"));
                break;
            }
            Err(_) => {
                let _ = out.send("读取响应超时".to_owned());
                break;
            }
        }
    }

    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    let (head, body) = split_response(&response);
    let head_text = String::from_utf8_lossy(head);
    let _ = out.send(format!("{method} {url} · {elapsed:.0} ms"));
    for line in head_text.lines() {
        if !line.is_empty() {
            let _ = out.send(line.to_owned());
        }
    }
    let chunked = head_text
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked");
    let body = if chunked {
        decode_chunked(body)
    } else {
        body.to_vec()
    };
    let _ = out.send(format!("响应体 {} 字节：", body.len()));
    let text = String::from_utf8_lossy(&body);
    for line in text.lines().take(200) {
        let _ = out.send(line.to_owned());
    }
}

/// Splits an HTTP response into head and body bytes.
fn split_response(response: &[u8]) -> (&[u8], &[u8]) {
    match response.windows(4).position(|window| window == b"\r\n\r\n") {
        Some(index) => (&response[..index], &response[index + 4..]),
        None => (response, &[]),
    }
}

/// Decodes a chunked body; a malformed chunk ends the decode rather than failing the request.
fn decode_chunked(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(end) = rest.windows(2).position(|window| window == b"\r\n") {
        let size_text = std::str::from_utf8(&rest[..end]).unwrap_or("");
        let size_text = size_text.split(';').next().unwrap_or("").trim();
        let Ok(size) = usize::from_str_radix(size_text, 16) else {
            break;
        };
        rest = &rest[end + 2..];
        if size == 0 || rest.len() < size {
            break;
        }
        out.extend_from_slice(&rest[..size]);
        rest = &rest[(size + 2).min(rest.len())..];
    }
    out
}

/// Parses `http://host[:port][/path]`. HTTPS needs a TLS stack and is not offered yet.
fn parse_http_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| "目前只支持 http:// 地址（HTTPS 暂未开放）".to_owned())?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err("URL 缺少主机名".to_owned());
    }
    let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
        // IPv6 literal: [::1] or [::1]:8080.
        match stripped.split_once(']') {
            Some((host, rest)) => {
                let port = rest
                    .strip_prefix(':')
                    .and_then(|port| port.parse::<u16>().ok())
                    .unwrap_or(80);
                (host.to_owned(), port)
            }
            None => return Err("IPv6 地址缺少 ]".to_owned()),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) if port.parse::<u16>().is_ok() => {
                (host.to_owned(), port.parse().unwrap_or(80))
            }
            _ => (authority.to_owned(), 80),
        }
    };
    if host.is_empty() {
        return Err("URL 缺少主机名".to_owned());
    }
    Ok((host, port, path.to_owned()))
}

fn parse_port(text: &str) -> Result<u16, String> {
    text.trim()
        .parse::<u16>()
        .map_err(|_| format!("端口号无效：{text}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_urls_split_into_host_port_and_path() {
        assert_eq!(
            parse_http_url("http://192.168.1.1").unwrap(),
            ("192.168.1.1".to_owned(), 80, "/".to_owned())
        );
        assert_eq!(
            parse_http_url("http://example.com:8080/api?x=1").unwrap(),
            ("example.com".to_owned(), 8080, "/api?x=1".to_owned())
        );
        assert_eq!(
            parse_http_url("http://[::1]:9000/status").unwrap(),
            ("::1".to_owned(), 9000, "/status".to_owned())
        );
        assert!(parse_http_url("https://example.com/").is_err());
        assert!(parse_http_url("example.com").is_err());
    }

    #[test]
    fn chunked_bodies_are_joined() {
        let body = b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        assert_eq!(decode_chunked(body), b"Wikipedia");
        // A malformed chunk stops the decode without panicking.
        assert_eq!(decode_chunked(b"zz\r\nabc"), Vec::<u8>::new());
    }

    #[tokio::test]
    async fn tcp_ping_and_scan_find_a_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                drop(stream);
            }
        });

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        tcp_ping("127.0.0.1".to_owned(), port, 2, 10, 500, tx).await;
        let mut lines = Vec::new();
        while let Some(line) = rx.recv().await {
            lines.push(line);
        }
        assert!(lines.iter().any(|line| line.contains("2/2")), "{lines:?}");
        assert!(
            lines.iter().any(|line| line.contains("成功 2/2")),
            "{lines:?}"
        );

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        scan_ports("127.0.0.1".to_owned(), port, port, 500, 4, tx).await;
        let mut lines = Vec::new();
        while let Some(line) = rx.recv().await {
            lines.push(line);
        }
        assert!(
            lines
                .iter()
                .any(|line| line.contains(&format!("开放 {port}"))),
            "{lines:?}"
        );
    }

    #[tokio::test]
    async fn an_http_request_reads_the_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 2048];
            let _ = stream.read(&mut buffer).await;
            let _ = stream
                .write_all(
                    b"HTTP/1.1 200 OK
Content-Length: 5

hello",
                )
                .await;
        });
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        http_request(
            "GET".to_owned(),
            format!("http://{addr}/status"),
            String::new(),
            String::new(),
            2000,
            tx,
        )
        .await;
        let mut lines = Vec::new();
        while let Some(line) = rx.recv().await {
            lines.push(line);
        }
        assert!(
            lines.iter().any(|line| line.contains("HTTP/1.1 200 OK")),
            "{lines:?}"
        );
        assert!(lines.iter().any(|line| line == "hello"), "{lines:?}");
    }

    #[tokio::test]
    async fn dns_resolves_localhost() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        resolve("localhost".to_owned(), tx).await;
        let mut lines = Vec::new();
        while let Some(line) = rx.recv().await {
            lines.push(line);
        }
        assert!(
            lines
                .iter()
                .any(|line| line.contains("127.0.0.1") || line.contains("::1")),
            "{lines:?}"
        );
    }

    #[test]
    fn a_response_splits_at_the_first_blank_line() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi";
        let (head, body) = split_response(response);
        assert_eq!(body, b"hi");
        assert!(String::from_utf8_lossy(head).starts_with("HTTP/1.1 200"));
    }
}
