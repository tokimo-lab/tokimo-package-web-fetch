//! 统一 web 抓取入口：HTTP / 无头浏览器 / Cloudflare bypass + 可选 Readability 降噪。

use std::sync::Arc;
use std::time::Duration;

use crate::browser::BrowserFetch;
use crate::cloudflare::{CloudflareBypassClient, has_anti_bot_wall, is_under_challenge, looks_like_spa_or_blank};
use crate::error::{FetchError, FetchResult};
use crate::readability::{DenoisedArticle, denoise_html};

/// 经过 HTTP 通道抓到的内容，如果 Readability 能抽出的正文字符数
/// 少于这个阈值，就视为 SPA / 反爬 / 无实质内容，触发无头浏览器重试。
const BROWSER_ESCALATION_MIN_READABLE_CHARS: usize = 200;

/// 在做 "粗略 HTML → 纯文本" 统计时，少于这个可见字符数认为
/// 页面基本是 SPA 壳子或空白 —— 此阈值给 auto 通道在 pre-denoise 时用。
const SPA_BLANK_MIN_CHARS: usize = 120;

/// 上面这些阈值对 "很短的反爬 403/验证页" 单独再加一道阈值：
/// body 小于这个长度时，几乎不可能承载有用内容，直接升级浏览器。
const TINY_BODY_BYTES: usize = 512;

/// 选择哪种通道抓页面。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FetchMode {
    /// 优先 HTTP，碰到 CF 挑战自动升级到 bypass，再降级到浏览器（如果可用）。
    #[default]
    Auto,
    /// 强制走普通 reqwest GET。
    Http,
    /// 强制走无头浏览器；浏览器不可用则降级到 HTTP 并打 warn 日志。
    Browser,
    /// 强制走 FlareSolverr Cloudflare bypass；FlareSolverr 未配置则降级到 HTTP。
    CloudflareBypass,
}

/// 是否做降噪。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Denoise {
    /// 原始 HTML，不处理。
    #[default]
    None,
    /// Readability 抽主文，返回结构化 [`DenoisedArticle`]。
    Readability,
}

#[derive(Debug, Clone)]
pub struct FetchOptions {
    pub mode: FetchMode,
    pub denoise: Denoise,
    pub timeout: Duration,
    /// 自定义 Cookie 头（仅对 HTTP / CF 通道生效）。
    pub cookie: Option<String>,
    /// 额外请求头（仅 HTTP 通道）。
    pub extra_headers: Vec<(String, String)>,
    /// 是否启用 SSRF 防护（检查目标 IP 是否为私有/内网地址）。默认关闭。
    pub ssrf_enabled: bool,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            mode: FetchMode::default(),
            denoise: Denoise::default(),
            timeout: Duration::from_secs(30),
            cookie: None,
            extra_headers: Vec::new(),
            ssrf_enabled: false,
        }
    }
}

/// 实际执行抓取走的通道（用于日志 / 调试）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsedChannel {
    Http,
    Browser,
    CloudflareBypass,
}

#[derive(Debug, Clone)]
pub struct FetchResponse {
    pub status: u16,
    pub final_url: String,
    pub body: String,
    pub used: UsedChannel,
    /// `denoise = Readability` 时为 `Some`；否则为 `None`。
    pub denoised: Option<DenoisedArticle>,
}

/// 统一的 web 抓取客户端。
///
/// 用 [`WebFetcherBuilder`] 构造。
pub struct WebFetcher {
    http: reqwest::Client,
    browser: Option<Arc<dyn BrowserFetch>>,
    cf: Option<CloudflareBypassClient>,
    default_options: FetchOptions,
}

#[derive(Default)]
pub struct WebFetcherBuilder {
    http: Option<reqwest::Client>,
    browser: Option<Arc<dyn BrowserFetch>>,
    flaresolverr_url: Option<String>,
    user_agent: Option<String>,
    default_options: FetchOptions,
}

impl WebFetcher {
    #[must_use]
    pub fn builder() -> WebFetcherBuilder {
        WebFetcherBuilder::default()
    }

    /// 默认配置：reqwest + headless 浏览器 autodetect（Lightpanda → Chrome）+ 无 FlareSolverr。
    #[must_use]
    pub fn with_defaults() -> Self {
        WebFetcherBuilder::default().with_autodetect().build()
    }

    pub fn http_client(&self) -> &reqwest::Client {
        &self.http
    }

    pub fn browser(&self) -> Option<&Arc<dyn BrowserFetch>> {
        self.browser.as_ref()
    }

    /// 抓一个 URL，使用默认 options。
    pub async fn fetch(&self, url: &str) -> FetchResult<FetchResponse> {
        self.fetch_with(url, &self.default_options).await
    }

    /// 抓一个 URL，覆盖 options。
    pub async fn fetch_with(&self, url: &str, opts: &FetchOptions) -> FetchResult<FetchResponse> {
        // SSRF 防护：在发起任何网络请求前验证目标 IP 不属于私有/内网地址。
        // 已知局限：DNS rebinding (TOCTOU) 和 redirect-based bypass 无法在此层完全阻止；
        // 详见 ssrf.rs 文档注释。
        if opts.ssrf_enabled {
            crate::ssrf::check_ssrf(url).await?;
        }

        let raw = tokio::time::timeout(opts.timeout, self.fetch_raw(url, opts))
            .await
            .map_err(|_| FetchError::Timeout)??;

        let denoised = match opts.denoise {
            Denoise::None => None,
            // Readability 失败时先不直接报错，留给下面的浏览器升级兜底；
            // 如果最后仍然没有可用降噪结果，再返回错误。
            Denoise::Readability => denoise_html(&raw.body, url, &raw.final_url).ok(),
        };

        // 后降噪升级：HTTP 通道拿到 200 但 Readability 只抽出很短正文
        // （典型：JS 渲染的 SPA / 列表页只有导航文字），或 Readability 直接
        // 失败（例如 doubao.com 这种纯 SPA 静态 HTML 根本没有正文），如果
        // 配置了浏览器通道，再用浏览器重抓一次。
        let (raw, denoised) = self.maybe_escalate_to_browser(url, opts, raw, denoised).await;

        // 如果调用方明确要求 Readability 但最终仍然没有结果，返回错误。
        if opts.denoise == Denoise::Readability && denoised.is_none() {
            return Err(FetchError::Readability("failed to extract readable content".into()));
        }

        Ok(FetchResponse {
            status: raw.status,
            final_url: raw.final_url,
            body: raw.body,
            used: raw.used,
            denoised,
        })
    }

    async fn maybe_escalate_to_browser(
        &self,
        url: &str,
        opts: &FetchOptions,
        raw: RawFetch,
        denoised: Option<DenoisedArticle>,
    ) -> (RawFetch, Option<DenoisedArticle>) {
        // 仅在 Auto 模式 + HTTP 通道 + 有浏览器 + 需要降噪时考虑升级。
        if opts.mode != FetchMode::Auto
            || raw.used != UsedChannel::Http
            || self.browser.is_none()
            || opts.denoise == Denoise::None
        {
            return (raw, denoised);
        }
        // denoise 失败（None）也要升级；否则按正文字符数判断。
        let readable_chars: usize = denoised
            .as_ref()
            .map_or(0, |a| a.content_text.chars().filter(|c| !c.is_whitespace()).count());
        if denoised.is_some() && readable_chars >= BROWSER_ESCALATION_MIN_READABLE_CHARS {
            return (raw, denoised);
        }

        tracing::info!(
            url,
            readable_chars,
            body_bytes = raw.body.len(),
            denoise_failed = denoised.is_none(),
            "Auto: HTTP 通道 Readability 无/过短正文，升级无头浏览器重抓"
        );
        match self.browser_or_fallback(url, opts).await {
            Some(Ok(new_raw)) => {
                let new_denoised = denoise_html(&new_raw.body, url, &new_raw.final_url).ok();
                let new_chars: usize = new_denoised
                    .as_ref()
                    .map_or(0, |d| d.content_text.chars().filter(|c| !c.is_whitespace()).count());
                if new_chars > readable_chars {
                    (new_raw, new_denoised)
                } else {
                    // 浏览器也没拿到更多正文，维持 HTTP 结果避免倒退。
                    (raw, denoised)
                }
            }
            Some(Err(e)) => {
                tracing::warn!(url, error = %e, "无头浏览器升级失败，保留 HTTP 结果");
                (raw, denoised)
            }
            None => (raw, denoised),
        }
    }

    async fn fetch_raw(&self, url: &str, opts: &FetchOptions) -> FetchResult<RawFetch> {
        match opts.mode {
            FetchMode::Http => self.fetch_http(url, opts).await,
            FetchMode::Browser => match self.browser_or_fallback(url, opts).await {
                Some(res) => res,
                None => self.fetch_http(url, opts).await,
            },
            FetchMode::CloudflareBypass => self.fetch_cf_or_fallback(url, opts).await,
            FetchMode::Auto => self.fetch_auto(url, opts).await,
        }
    }

    async fn fetch_auto(&self, url: &str, opts: &FetchOptions) -> FetchResult<RawFetch> {
        let http_res = self.fetch_http(url, opts).await?;
        if !needs_pre_denoise_escalation(http_res.status, &http_res.body) {
            return Ok(http_res);
        }
        tracing::info!(
            url,
            status = http_res.status,
            body_bytes = http_res.body.len(),
            "Auto: HTTP 返回命中反爬/空壳/失败特征，尝试升级通道"
        );
        // 优先 CF bypass，失败再尝试浏览器。
        if self.cf.is_some() {
            match self.fetch_cf(url, opts).await {
                Ok(r) if !needs_pre_denoise_escalation(r.status, &r.body) => return Ok(r),
                Ok(_) => tracing::warn!(url, "FlareSolverr 通过但仍是挑战页/空壳"),
                Err(e) => tracing::warn!(url, error = %e, "FlareSolverr 失败"),
            }
        }
        if let Some(res) = self.browser_or_fallback(url, opts).await {
            return res;
        }
        // 都不行，把原始 HTTP 结果返回，让上游决定如何处理。
        Ok(http_res)
    }

    /// 试图调用浏览器；浏览器不可用时返回 None（调用方负责降级 + 打日志已在此函数内完成）。
    async fn browser_or_fallback(&self, url: &str, _opts: &FetchOptions) -> Option<FetchResult<RawFetch>> {
        let Some(browser) = &self.browser else {
            tracing::warn!(url, "请求使用无头浏览器但未配置可用浏览器，降级到普通 HTTP 请求");
            return None;
        };
        let name = browser.name();
        Some(match browser.fetch_html(url).await {
            Ok(body) => Ok(RawFetch {
                status: 200,
                final_url: url.to_string(),
                body,
                used: UsedChannel::Browser,
            }),
            Err(e) => {
                tracing::warn!(url, browser = name, error = %e, "无头浏览器抓取失败");
                Err(e)
            }
        })
    }

    async fn fetch_cf_or_fallback(&self, url: &str, opts: &FetchOptions) -> FetchResult<RawFetch> {
        if self.cf.is_none() {
            tracing::warn!(url, "请求 CloudflareBypass 但未配置 FlareSolverr，降级到普通 HTTP 请求");
            return self.fetch_http(url, opts).await;
        }
        self.fetch_cf(url, opts).await
    }

    async fn fetch_cf(&self, url: &str, opts: &FetchOptions) -> FetchResult<RawFetch> {
        let cf = self
            .cf
            .as_ref()
            .ok_or_else(|| FetchError::CloudflareBypass("not configured".into()))?;
        let r = cf.fetch_html(url, opts.cookie.as_deref()).await?;
        Ok(RawFetch {
            status: r.status,
            final_url: r.final_url,
            body: r.body,
            used: UsedChannel::CloudflareBypass,
        })
    }

    async fn fetch_http(&self, url: &str, opts: &FetchOptions) -> FetchResult<RawFetch> {
        let mut req = self.http.get(url);
        if let Some(c) = &opts.cookie {
            req = req.header("Cookie", c.as_str());
        }
        for (k, v) in &opts.extra_headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req.send().await?;
        let status = resp.status().as_u16();
        let final_url = resp.url().to_string();
        let body = resp.text().await?;
        Ok(RawFetch {
            status,
            final_url,
            body,
            used: UsedChannel::Http,
        })
    }
}

struct RawFetch {
    status: u16,
    final_url: String,
    body: String,
    used: UsedChannel,
}

/// `fetch_auto` 阶段的"要不要升级通道"判断。
///
/// 这一层只用 **原始 HTTP 状态码 + HTML 字符串** 做粗筛，不依赖 Readability
/// 输出（`fetch_with` 会在降噪后再做一次更精确的判断）。命中任一条就升级：
///   - HTTP 非 2xx（典型：阿里 Tengine 对黑名单 UA 的 403）
///   - body 极短（<512B，几乎只能承载错误页 / 空 SPA 壳）
///   - 命中 Cloudflare / DDoS-Guard 挑战 ([`is_under_challenge`])
///   - 命中反爬墙 / 人机验证 / UA 黑名单 ([`has_anti_bot_wall`])
///   - 粗略去掉 script/style 后的可见文本 < 120 字符 ([`looks_like_spa_or_blank`])
fn needs_pre_denoise_escalation(status: u16, body: &str) -> bool {
    if !(200..300).contains(&status) {
        return true;
    }
    if body.len() < TINY_BODY_BYTES {
        return true;
    }
    if is_under_challenge(body) {
        return true;
    }
    if has_anti_bot_wall(body) {
        return true;
    }
    if looks_like_spa_or_blank(body, SPA_BLANK_MIN_CHARS) {
        return true;
    }
    false
}

impl WebFetcherBuilder {
    #[must_use]
    pub fn http_client(mut self, client: reqwest::Client) -> Self {
        self.http = Some(client);
        self
    }

    #[must_use]
    pub fn user_agent(mut self, ua: impl Into<String>) -> Self {
        self.user_agent = Some(ua.into());
        self
    }

    #[must_use]
    pub fn browser(mut self, browser: Arc<dyn BrowserFetch>) -> Self {
        self.browser = Some(browser);
        self
    }

    /// 自动探测系统里的 lightpanda；探测不到就保持 None。
    #[must_use]
    pub fn with_lightpanda_autodetect(mut self) -> Self {
        if let Some(lp) = crate::browser::LightpandaBrowser::autodetect() {
            self.browser = Some(Arc::new(lp));
        }
        self
    }

    /// 自动探测可用的 headless 浏览器：优先 Lightpanda，回退到 Chrome/Chromium。
    #[must_use]
    pub fn with_autodetect(mut self) -> Self {
        if let Some(browser) = crate::browser::autodetect_browser() {
            self.browser = Some(browser);
        }
        self
    }

    #[must_use]
    pub fn flaresolverr_url(mut self, url: impl Into<String>) -> Self {
        self.flaresolverr_url = Some(url.into());
        self
    }

    #[must_use]
    pub fn default_options(mut self, opts: FetchOptions) -> Self {
        self.default_options = opts;
        self
    }

    #[must_use]
    pub fn build(self) -> WebFetcher {
        let http = self.http.unwrap_or_else(|| {
            let ua = self
                .user_agent
                .clone()
                .unwrap_or_else(|| crate::DEFAULT_USER_AGENT.to_string());
            reqwest::Client::builder()
                .user_agent(ua)
                .gzip(true)
                .brotli(true)
                .cookie_store(true)
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap_or_default()
        });
        let cf = self
            .flaresolverr_url
            .clone()
            .map(|u| CloudflareBypassClient::with_client(http.clone(), Some(u)));
        WebFetcher {
            http,
            browser: self.browser,
            cf,
            default_options: self.default_options,
        }
    }
}
