//! tokimo-web-fetch — 统一 web 页面抓取
//!
//! 提供三种通道：
//! - **HTTP**：普通 `reqwest::Client::get`
//! - **Browser**：调用本地 `lightpanda` headless 浏览器执行 JS 后取 HTML
//! - **CloudflareBypass**：通过 [FlareSolverr] 绕 CF 挑战
//!
//! 以及可选的 [Readability] 降噪，把 HTML 抽成主文 + 元数据。
//!
//! 入口：[`WebFetcher`]，用 [`WebFetcher::builder`] 构造，[`WebFetcher::fetch_with`] 调用。
//!
//! 行为约定：当请求方指定 `FetchMode::Browser` 但当前 `WebFetcher`
//! 没有可用的浏览器实例（`autodetect` 没找到 lightpanda、也没手动注入），
//! 会打 `WARN` 日志后降级到普通 HTTP 请求。`FetchMode::CloudflareBypass`
//! 在没配 FlareSolverr 时同理。
//!
//! [FlareSolverr]: https://github.com/FlareSolverr/FlareSolverr
//! [Readability]: https://github.com/mozilla/readability

#![allow(clippy::module_name_repetitions)]

pub mod browser;
pub mod cloudflare;
pub mod error;
pub mod fetcher;
pub mod readability;
pub mod ssrf;

pub use browser::{BrowserFetch, LightpandaBrowser};
pub use cloudflare::{CfFetchResult, CloudflareBypassClient, is_under_challenge};
pub use error::{FetchError, FetchResult};
pub use fetcher::{
    Denoise, FetchMode, FetchOptions, FetchResponse, UsedChannel, WebFetcher,
    WebFetcherBuilder,
};
pub use readability::{DenoisedArticle, denoise_html};

/// 默认 UA 使用较新的桌面 Chrome。
///
/// 历史上曾使用 Firefox UA，但阿里 Tengine CDN 的若干默认黑名单规则
/// 会对 `Firefox/135` 这种较新 Firefox UA 直接返回 403 `denied by UA ACL = blacklist`
/// （例如 tianqi.eastday.com / tianqi.com），导致 Readability 抓到的只是
/// 一个 311 字节的 403 提示页。Chrome UA 实测能穿过这些默认规则。
pub const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
