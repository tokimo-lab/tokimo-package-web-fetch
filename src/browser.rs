//! 可选的 headless 浏览器抓取抽象。
//!
//! 对付纯 JS 渲染 / 反爬站点（toutiao、zhihu、douyin、reCAPTCHA 等），
//! 上层可以注入一个实现来让上游通过真浏览器拿 HTML。
//!
//! 本 crate 自带两种 headless 浏览器后端：
//! - [`ChromeBrowser`]：系统已安装的 Chrome / Chromium（`--headless=new --dump-dom`），autodetect 默认使用
//! - [`LightpandaBrowser`]：极轻量 CLI 浏览器（<https://lightpanda.io>），仅在显式 opt-in 时启用；
//!   对现代前端（React/Vue SPA、微博、抖音等）兼容性差，autodetect 已**不再**回退到它
//!
//! 通过 [`autodetect_browser`] 只会探测 Chrome；想用 Lightpanda 的话请显式调用
//! [`crate::WebFetcherBuilder::with_lightpanda_autodetect`]。

use crate::error::{FetchError, FetchResult};
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

#[async_trait]
pub trait BrowserFetch: Send + Sync {
    /// 拉一个 URL，返回完整 HTML（应已执行 JS 到 load / networkidle）。
    async fn fetch_html(&self, url: &str) -> FetchResult<String>;

    /// 实现名（用于日志）。默认 "browser"。
    fn name(&self) -> &'static str {
        "browser"
    }
}

/// 用系统 `lightpanda` CLI 作为 headless 浏览器。
///
/// 找不到二进制时 [`LightpandaBrowser::autodetect`] 返回 `None`。
pub struct LightpandaBrowser {
    bin: PathBuf,
    /// 单页抓取超时（外层 tokio timeout 兜底）
    pub timeout: Duration,
    /// `--wait-until` 策略，默认 "networkidle"
    pub wait_until: String,
    /// 固定等待毫秒（用来对付 zse-ck 这种挑战）
    pub wait_ms: Option<u32>,
    /// 可选的等待选择器（优先级高于 wait_ms）
    pub wait_selector: Option<String>,
}

impl LightpandaBrowser {
    pub fn new<P: Into<PathBuf>>(bin: P) -> Self {
        Self {
            bin: bin.into(),
            timeout: Duration::from_secs(25),
            wait_until: "networkidle".to_string(),
            wait_ms: None,
            wait_selector: None,
        }
    }

    /// 查找顺序：
    /// 1. `$LIGHTPANDA_BIN`（显式覆盖）
    /// 2. 工作区 `bin/lightpanda/current/bin/lightpanda`（deps.toml 管理布局，优先）
    /// 3. 工作区 `bin/lightpanda`（旧 flat 布局，兼容期保留）
    /// 4. `$PATH`
    /// 5. `~/.local/bin/lightpanda` / `/usr/local/bin/lightpanda`
    #[must_use]
    pub fn autodetect() -> Option<Self> {
        if let Ok(p) = std::env::var("LIGHTPANDA_BIN") {
            let pb = PathBuf::from(p);
            if pb.is_file() {
                return Some(Self::new(pb));
            }
        }
        if let Some(p) = find_workspace_bin() {
            return Some(Self::new(p));
        }
        if let Ok(p) = which_in_path("lightpanda") {
            return Some(Self::new(p));
        }
        for cand in [
            std::env::var("HOME").ok().map(|h| format!("{h}/.local/bin/lightpanda")),
            Some("/usr/local/bin/lightpanda".to_string()),
        ]
        .into_iter()
        .flatten()
        {
            let p = PathBuf::from(&cand);
            if p.is_file() {
                return Some(Self::new(p));
            }
        }
        None
    }
}

/// 从 `CARGO_MANIFEST_DIR` 或当前目录向上递归查找 lightpanda 二进制。
/// 优先 deps.toml 管理的 `bin/lightpanda/current/bin/lightpanda[.exe]`，
/// 兼容 fallback 到旧 flat 布局 `bin/lightpanda[.exe]`。
fn find_workspace_bin() -> Option<PathBuf> {
    let start = std::env::var("CARGO_MANIFEST_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    let exe_suffix = if cfg!(windows) { ".exe" } else { "" };
    let candidates = [
        format!("bin/lightpanda/current/bin/lightpanda{exe_suffix}"),
        format!("bin/lightpanda{exe_suffix}"),
    ];
    let mut dir = start.as_path();
    loop {
        for rel in &candidates {
            let cand = dir.join(rel);
            if cand.is_file() {
                return Some(cand);
            }
        }
        dir = dir.parent()?;
    }
}

fn which_in_path(name: &str) -> Result<PathBuf, ()> {
    let path = std::env::var_os("PATH").ok_or(())?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(())
}

#[async_trait]
impl BrowserFetch for LightpandaBrowser {
    fn name(&self) -> &'static str {
        "lightpanda"
    }

    async fn fetch_html(&self, url: &str) -> FetchResult<String> {
        let mut cmd = tokio::process::Command::new(&self.bin);
        cmd.arg("fetch")
            .arg("--dump")
            .arg("html")
            .arg("--wait-until")
            .arg(&self.wait_until);
        if let Some(sel) = &self.wait_selector {
            cmd.arg("--wait-selector").arg(sel);
        } else if let Some(ms) = self.wait_ms {
            cmd.arg("--wait-ms").arg(ms.to_string());
        }
        cmd.arg(url);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let fut = async {
            let output = cmd
                .output()
                .await
                .map_err(|e| FetchError::Browser(format!("spawn failed: {e}")))?;
            if !output.status.success() {
                let err = String::from_utf8_lossy(&output.stderr);
                return Err(FetchError::Browser(format!("exit {:?}: {err}", output.status.code())));
            }
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        };

        tokio::time::timeout(self.timeout, fut)
            .await
            .map_err(|_| FetchError::Timeout)?
    }
}

/// 用系统安装的 Chrome / Chromium 作为 headless 浏览器。
///
/// 通过 `--headless=new --dump-dom` 渲染 JS 后输出最终 DOM，
/// 不需要 CDP / websocket 依赖，每次 fetch 启动新进程、用完即退出。
pub struct ChromeBrowser {
    bin: PathBuf,
    /// 单页抓取超时（外层 tokio timeout 兜底）
    pub timeout: Duration,
    /// `--virtual-time-budget` 毫秒数；Chrome 会在虚拟时间推进这么多毫秒后再 dump DOM，
    /// 用来让 SPA 页面的异步 JS（天气 API、列表渲染等）有机会执行完毕。
    /// 设为 `None` 则不传该 flag，Chrome 使用默认行为（等 load 事件即 dump）。
    ///
    /// 推荐值（Chrome/Puppeteer 社区惯例）：
    /// - 静态页：不需要
    /// - 轻 SPA（React/Vue 基本渲染）：1000–3000
    /// - 带 API 调用的 SPA（天气、新闻列表等）：5000–10000
    /// - 重型 dashboard：10000–20000
    ///
    /// 详见 [`VIRTUAL_TIME_BUDGET_DEFAULT`]。
    pub virtual_time_budget_ms: Option<u32>,
}

/// 带 API 调用的 SPA 页面推荐的 `--virtual-time-budget` 默认值（10 秒）。
///
/// Chrome/Puppeteer 社区惯例：SPA 页面需要 5–10s 让异步 JS + 网络请求完成。
/// 该值适用于需要远程 API 调用的页面（天气、新闻、列表等），
/// 对纯前端渲染的轻量 SPA 可适当降低至 3000–5000。
pub const VIRTUAL_TIME_BUDGET_DEFAULT: u32 = 8_000;

/// Chrome 路径缓存，整个进程生命周期只搜索一次。
static CHROME_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// 已知的 Chrome / Chromium 可执行文件名（不含 .exe，跨平台通用）。
const CHROME_NAMES: &[&str] = &["google-chrome-stable", "google-chrome", "chromium-browser", "chromium"];

impl ChromeBrowser {
    pub fn new<P: Into<PathBuf>>(bin: P) -> Self {
        Self {
            bin: bin.into(),
            timeout: Duration::from_secs(12),
            virtual_time_budget_ms: Some(VIRTUAL_TIME_BUDGET_DEFAULT),
        }
    }

    /// 自动探测系统中的 Chrome / Chromium，结果缓存在 [`CHROME_PATH`]。
    ///
    /// 查找顺序：
    /// 1. `$CHROME_BIN`（显式覆盖）
    /// 2. `$PATH` 中搜索已知名字
    /// 3. 平台特定已知路径（Linux / macOS / Windows 多盘符）
    #[must_use]
    pub fn autodetect() -> Option<Self> {
        let path = CHROME_PATH.get_or_init(find_chrome);
        path.as_ref().map(|p| Self::new(p.clone()))
    }
}

/// 实际的 Chrome 搜索逻辑，只在首次调用时执行。
fn find_chrome() -> Option<PathBuf> {
    // 1. 环境变量显式指定
    if let Ok(p) = std::env::var("CHROME_BIN") {
        let pb = PathBuf::from(&p);
        if pb.is_file() {
            return Some(pb);
        }
    }

    // 2. PATH 搜索
    for name in CHROME_NAMES {
        if let Ok(p) = which_in_path(name) {
            return Some(p);
        }
    }
    // Windows: try `where chrome`
    if cfg!(windows)
        && let Some(p) = where_command("chrome")
    {
        return Some(p);
    }

    // 3. 平台特定已知路径
    platform_chrome_paths().into_iter().find(|p| p.is_file())
}

/// Windows 上运行 `where <name>` 查找可执行文件。
#[cfg(windows)]
fn where_command(name: &str) -> Option<PathBuf> {
    let output = std::process::Command::new("where").arg(name).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let first_line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()?
        .trim()
        .to_string();
    let p = PathBuf::from(first_line);
    if p.is_file() { Some(p) } else { None }
}

#[cfg(not(windows))]
fn where_command(_name: &str) -> Option<PathBuf> {
    None
}

/// 平台特定的 Chrome / Chromium 已知安装路径。
fn platform_chrome_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if cfg!(target_os = "linux") {
        paths.extend([
            PathBuf::from("/usr/bin/google-chrome-stable"),
            PathBuf::from("/usr/bin/google-chrome"),
            PathBuf::from("/usr/bin/chromium-browser"),
            PathBuf::from("/usr/bin/chromium"),
            PathBuf::from("/snap/bin/chromium"),
        ]);
    } else if cfg!(target_os = "macos") {
        if let Ok(home) = std::env::var("HOME") {
            paths.push(PathBuf::from(format!(
                "{home}/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
            )));
        }
        paths.push(PathBuf::from(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ));
        paths.push(PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"));
    } else if cfg!(target_os = "windows") {
        let exe = "Google\\Chrome\\Application\\chrome.exe";
        // 从环境变量推导盘符，支持非 C: 安装
        for env_key in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
            if let Ok(base) = std::env::var(env_key) {
                paths.push(PathBuf::from(format!("{base}\\{exe}")));
            }
        }
        // 常见固定路径兜底
        paths.extend([
            PathBuf::from("C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"),
            PathBuf::from("C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe"),
        ]);
    }

    paths
}

#[async_trait]
impl BrowserFetch for ChromeBrowser {
    fn name(&self) -> &'static str {
        "chrome"
    }

    async fn fetch_html(&self, url: &str) -> FetchResult<String> {
        let mut cmd = tokio::process::Command::new(&self.bin);
        cmd.arg("--headless=new")
            .arg("--no-sandbox")
            .arg("--disable-gpu")
            .arg("--disable-dev-shm-usage")
            .arg("--hide-scrollbars")
            .arg(format!("--user-agent={}", crate::DEFAULT_USER_AGENT));
        // `--virtual-time-budget` 让 Chrome 把 N 毫秒的 setTimeout / 跳转
        // 折叠到极短的真实时间内执行完，避免 --dump-dom 在第一个 load
        // 事件就 dump 走（典型坑：微博 passport.visitor 中转页、各种
        // meta-refresh 中转，不加这个 flag 只能抓到中转壳子）。
        if let Some(ms) = self.virtual_time_budget_ms {
            cmd.arg(format!("--virtual-time-budget={ms}"));
            // `--timeout` 给 Chrome 内部一个硬上限，超过就强制 dump 然后
            // 退出；防止 tieba / 百度系页面一直长轮询 XHR 让 Chrome 永远
            // 等不到"网络空闲"。配合 virtual-time-budget 使用，给一些 buffer。
            cmd.arg(format!("--timeout={}", ms.saturating_add(5_000)));
        }
        cmd.arg("--dump-dom").arg(url);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| FetchError::Browser(format!("spawn failed: {e}")))?;

        // 手动接管 stdout，把 buffer 提到外层：超时时也能保留已读到的 DOM
        // 片段，避免外层 timeout 触发后直接丢弃所有数据。
        let mut stdout = child.stdout.take().expect("piped stdout");
        let buf = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::<u8>::new()));
        let buf_reader = std::sync::Arc::clone(&buf);

        let read_fut = async {
            use tokio::io::AsyncReadExt;
            let mut chunk = [0u8; 8192];
            loop {
                match stdout.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut shared = buf_reader.lock().await;
                        shared.extend_from_slice(&chunk[..n]);
                    }
                    Err(e) => return Err(FetchError::Browser(format!("stdout read failed: {e}"))),
                }
            }
            let status = child
                .wait()
                .await
                .map_err(|e| FetchError::Browser(format!("wait failed: {e}")))?;
            if !status.success() {
                return Err(FetchError::Browser(format!("exit {:?}", status.code())));
            }
            let bytes = buf_reader.lock().await.clone();
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        };

        match tokio::time::timeout(self.timeout, read_fut).await {
            Err(_elapsed) => {
                // 超时：先杀进程防泄漏，再把已 dump 出来的部分尽量利用起来。
                let _ = child.kill().await;
                let partial = buf.lock().await.clone();
                if partial.len() > 1024 {
                    tracing::warn!("chrome timeout but recovered {} bytes of partial DOM", partial.len());
                    Ok(String::from_utf8_lossy(&partial).into_owned())
                } else {
                    Err(FetchError::Timeout)
                }
            }
            Ok(result) => result,
        }
    }
}

/// 自动探测可用的 headless 浏览器：只检测 Chrome / Chromium。
///
/// 历史上曾经把 Lightpanda 作为默认首选（更轻量、启动更快），但实测下来它对
/// 现代前端（微博的 React bundle、抖音、知乎等）经常在执行 JS 时直接抛
/// `caught.exception=Unknown` 而水合失败，并且**仍以 exit 0 返回一张空壳 DOM**，
/// 导致上层无从识别失败、也不会再回退到 Chrome。为了避免这种"假装成功"，
/// 现在 autodetect 链路里彻底移除 Lightpanda：调用方如果仍想用，可显式调用
/// [`crate::WebFetcherBuilder::with_lightpanda_autodetect`] 自负盈亏。
pub fn autodetect_browser() -> Option<Arc<dyn BrowserFetch>> {
    if let Some(ch) = ChromeBrowser::autodetect() {
        tracing::info!("autodetected headless browser: chrome");
        return Some(Arc::new(ch));
    }
    tracing::info!("no headless browser detected (chrome / chromium)");
    None
}
