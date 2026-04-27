//! 可选的 headless 浏览器抓取抽象。
//!
//! 对付纯 JS 渲染 / 反爬站点（toutiao、zhihu、douyin、reCAPTCHA 等），
//! 上层可以注入一个实现来让上游通过真浏览器拿 HTML。
//!
//! 本 crate 自带 [`LightpandaBrowser`]：调用系统里已经安装的 `lightpanda` CLI
//! （<https://lightpanda.io>），不需要 GPU / display 的极轻量 headless 浏览器。

use crate::error::{FetchError, FetchResult};
use async_trait::async_trait;
use std::path::PathBuf;
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
    /// 2. 工作区 `bin/lightpanda`（`make lightpanda` 的产物，向上找到第一个 `bin/lightpanda`）
    /// 3. `$PATH`
    /// 4. `~/.local/bin/lightpanda` / `/usr/local/bin/lightpanda`
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

/// 从 `CARGO_MANIFEST_DIR` 或当前目录向上递归查找 `bin/lightpanda`。
fn find_workspace_bin() -> Option<PathBuf> {
    let start = std::env::var("CARGO_MANIFEST_DIR")
        .ok()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    let mut dir = start.as_path();
    loop {
        let cand = dir.join("bin").join("lightpanda");
        if cand.is_file() {
            return Some(cand);
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
