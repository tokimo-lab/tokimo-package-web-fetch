//! 统一 web 抓取入口：HTTP / 无头浏览器 / Cloudflare bypass + 可选 Readability 降噪。

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, HeaderValue};

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

/// 统计 content_text 中的可见字符数，剔除 Markdown 链接/图片里的 URL。
///
/// `content_text` 使用 `TextMode::Markdown` 输出，包含 `[text](url)` 格式链接，
/// URL 部分会虚高字符数。此函数将 URL 部分剥离后再统计，得到更准确的内容密度。
pub fn count_visible_content_chars(text: &str) -> usize {
    // 剥离 markdown 链接/图片 URL: [text](url) → text, ![alt](url) → alt
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut count = 0usize;

    while i < len {
        // 检查是否是图片链接 ![...](...)
        let bracket_start = if chars[i] == '!' && i + 1 < len && chars[i + 1] == '[' {
            i + 2 // 跳过 '!' 和 '['
        } else if chars[i] == '[' {
            i + 1 // 跳过 '['
        } else {
            // 普通字符
            if !chars[i].is_whitespace() {
                count += 1;
            }
            i += 1;
            continue;
        };

        // 找配对的 ']'，支持嵌套
        let mut depth = 1usize;
        let mut j = bracket_start;
        while j < len {
            if chars[j] == '[' {
                depth += 1;
            } else if chars[j] == ']' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            j += 1;
        }
        if depth != 0 {
            // 没找到配对的 ']', 当前字符不是链接起点
            if !chars[i].is_whitespace() {
                count += 1;
            }
            i += 1;
            continue;
        }

        // j 指向配对的 ']', 检查后面是否是 '('
        if j + 1 < len && chars[j + 1] == '(' {
            // 找到链接: 统计 bracket_start..j 之间的非空白字符
            for ch in &chars[bracket_start..j] {
                if !ch.is_whitespace() {
                    count += 1;
                }
            }
            // 跳过 '(' ... ')'
            if let Some(close) = chars[j + 2..].iter().position(|&c| c == ')') {
                i = j + 2 + close + 1;
            } else {
                // 没有配对的 ')', 不是有效链接
                if !chars[i].is_whitespace() {
                    count += 1;
                }
                i += 1;
            }
        } else {
            // 不是链接，当前字符正常计数
            if !chars[i].is_whitespace() {
                count += 1;
            }
            i += 1;
        }
    }
    count
}

/// 上面这些阈值对 "很短的反爬 403/验证页" 单独再加一道阈值：
/// body 小于这个长度时，几乎不可能承载有用内容，直接升级浏览器。
const TINY_BODY_BYTES: usize = 512;

/// 非 2xx 状态码错误里附带多少字节的 body 预览（按 char 边界截断）。
const BAD_STATUS_BODY_PREVIEW_BYTES: usize = 1024;

/// 按字符边界把 body 截到大约 `max_bytes` 字节，避免在错误信息里塞整页 HTML。
fn truncate_body_preview(body: &str, max_bytes: usize) -> String {
    let body = body.trim();
    if body.len() <= max_bytes {
        return body.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = body[..end].to_string();
    out.push_str("…[truncated]");
    out
}

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
    None,
    /// Readability 抽主文，返回结构化 [`DenoisedArticle`]（Markdown 格式）。
    #[default]
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
    /// 关键词列表，用于 Readability 降噪前的关键词注入预处理。
    /// 非空时会提高包含关键词的元素在 Readability 评分中的权重。
    pub keywords: Vec<String>,
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
            keywords: Vec::new(),
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
    /// Lower-cased `Content-Type` header (e.g. `text/html; charset=utf-8`,
    /// `application/json`). `None` for channels that don't surface it
    /// (browser / CloudflareBypass).
    pub content_type: Option<String>,
    pub used: UsedChannel,
    /// `denoise = Readability` 时为 `Some`；否则为 `None`。
    /// Non-HTML responses (JSON / plain text) intentionally leave this as
    /// `None`; consumers should fall back to `body`.
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

    /// 默认配置：reqwest + headless 浏览器 autodetect（Chrome）+ 无 FlareSolverr。
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

        // 4xx / 5xx 直接报错，附上 status + 截断 body 预览，让上游（含 LLM）
        // 看到真实失败原因，而不是经过 Readability 失败包装后的"failed to extract"。
        // 3xx 在 reqwest 默认 redirect policy（limited(10)）下会被自动跟随，
        // 这里保留兜底分支：万一 redirect chain 超限或 client 关闭了跟随，
        // 也按非错误返回，让 body 落到下面 HTML/非 HTML 分支正常处理。
        if raw.status >= 400 {
            return Err(FetchError::BadStatus {
                status: raw.status,
                final_url: raw.final_url,
                body_preview: truncate_body_preview(&raw.body, BAD_STATUS_BODY_PREVIEW_BYTES),
            });
        }

        // 非 HTML 响应（典型：JSON / 纯文本 API）直接返回原始 body，
        // Readability 对它们没有意义，强行降噪只会得到空结果。
        if !raw.is_html_like() {
            return Ok(FetchResponse {
                status: raw.status,
                final_url: raw.final_url,
                body: raw.body,
                content_type: raw.content_type,
                used: raw.used,
                denoised: None,
            });
        }

        let denoised = match opts.denoise {
            Denoise::None => None,
            // Readability 失败时先不直接报错，留给下面的浏览器升级兜底；
            // 如果最后仍然没有可用降噪结果，再返回错误。
            Denoise::Readability => {
                let kw_refs: Vec<&str> = opts.keywords.iter().map(String::as_str).collect();
                denoise_html(&raw.body, url, &raw.final_url, &kw_refs).ok()
            }
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
            content_type: raw.content_type,
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
            .map_or(0, |a| count_visible_content_chars(&a.content_text));
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
                let kw_refs: Vec<&str> = opts.keywords.iter().map(String::as_str).collect();
                let new_denoised = denoise_html(&new_raw.body, url, &new_raw.final_url, &kw_refs).ok();
                let new_chars: usize = new_denoised
                    .as_ref()
                    .map_or(0, |d| count_visible_content_chars(&d.content_text));
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
                content_type: None,
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
            content_type: None,
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
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_ascii_lowercase);
        let body = resp.text().await?;
        Ok(RawFetch {
            status,
            final_url,
            body,
            content_type,
            used: UsedChannel::Http,
        })
    }
}

struct RawFetch {
    status: u16,
    final_url: String,
    body: String,
    /// Lower-cased `Content-Type` header value if the channel surfaced one.
    content_type: Option<String>,
    used: UsedChannel,
}

impl RawFetch {
    /// True iff the response is (or is assumed to be) HTML/XHTML — only then
    /// is Readability extraction meaningful. Channels that don't expose
    /// `Content-Type` (browser, CF bypass) are treated as HTML since they
    /// always produce rendered DOM.
    fn is_html_like(&self) -> bool {
        let Some(ct) = &self.content_type else {
            return true;
        };
        ct.starts_with("text/html") || ct.starts_with("application/xhtml")
    }
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

    /// 自动探测可用的 headless 浏览器。当前实现只检测 Chrome / Chromium。
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
            let mut default_headers = HeaderMap::new();

            // 浏览器标配 headers
            default_headers.insert("accept", HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7"));
            default_headers.insert("accept-language", HeaderValue::from_static("en-US,en;q=0.9,zh-CN;q=0.8,zh;q=0.7"));
            default_headers.insert("sec-ch-ua", HeaderValue::from_static(r#""Google Chrome";v="147", "Not.A/Brand";v="8", "Chromium";v="147""#));
            default_headers.insert("sec-ch-ua-mobile", HeaderValue::from_static("?0"));
            default_headers.insert("sec-ch-ua-platform", HeaderValue::from_static(r#""Windows""#));
            default_headers.insert("sec-fetch-dest", HeaderValue::from_static("document"));
            default_headers.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
            default_headers.insert("sec-fetch-site", HeaderValue::from_static("none"));
            default_headers.insert("sec-fetch-user", HeaderValue::from_static("?1"));
            default_headers.insert("upgrade-insecure-requests", HeaderValue::from_static("1"));

            // 生成随机无害 Cookie，避免被识别为无 cookie 的纯净爬虫
            if let Ok(cookie) = generate_benign_cookie() {
                default_headers.insert("cookie", HeaderValue::from_str(&cookie).unwrap());
            }

            reqwest::Client::builder()
                .user_agent(ua)
                .default_headers(default_headers)
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

/// 生成无害的随机 Cookie，让请求看起来像正常浏览器访问。
///
/// 包含常见的追踪/会话 cookie 名（如 `_ga`、`_gid`、`__cf_bm`），
/// 值是随机但格式正确的字符串，不指向任何真实会话。
fn generate_benign_cookie() -> Result<String, &'static str> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock error")?
        .as_secs();

    // LCG 常数 (MMIX by Knuth)，用时间戳做简单伪随机，不引入额外依赖
    const M: u64 = 6_364_136_223_846_793_005;
    const A: u64 = 1_442_695_040_888_963_407;
    let r1 = (ts.wrapping_mul(M).wrapping_add(A)) >> 32;
    let r2 = (r1.wrapping_mul(M).wrapping_add(A)) >> 32;
    let r3 = (r2.wrapping_mul(M).wrapping_add(A)) >> 32;

    // Google Analytics 风格: GA1.2.随机数.时间戳
    let ga_value = format!("GA1.2.{}.{}", r1 % 1_000_000_000, ts - 86400);
    // Cloudflare 风格: 基于时间戳的 hex
    let cf_bm = format!("{r2:016x}{r3:016x}");
    // 简单的 session id
    let sid = format!("{r1:08x}-{:04x}-{:04x}", r2 & 0xFFFF, r3 & 0xFFFF);

    Ok(format!(
        "_ga={ga_value}; _gid={ga_value}; __cf_bm={cf_bm}; session_id={sid}"
    ))
}

#[cfg(test)]
mod tests {
    use super::count_visible_content_chars;

    #[test]
    fn plain_text_count() {
        assert_eq!(count_visible_content_chars("hello world"), 10);
        assert_eq!(count_visible_content_chars(""), 0);
        assert_eq!(count_visible_content_chars("  \n\t  "), 0);
    }

    #[test]
    fn markdown_link_strips_url() {
        // [text](url) should count only "text", not the URL
        let md = "see [Google](https://www.google.com/?q=test&utm_source=x) for details";
        // "see" + "Google" + "for" + "details" = 3+6+3+7 = 19
        assert_eq!(count_visible_content_chars(md), 19);
    }

    #[test]
    fn markdown_image_strips_url() {
        let md = "![alt text](https://example.com/image.png)";
        // "alttext" = 7
        assert_eq!(count_visible_content_chars(md), 7);
    }

    #[test]
    fn mixed_content() {
        let md = "[关于腾讯](http://www.tencent.com/) | [About](http://www.tencent.com/index_e.shtml)";
        // "关于腾讯" + "|" + "About" = 4+1+5 = 10
        assert_eq!(count_visible_content_chars(md), 10);
    }

    #[test]
    fn bare_brackets_not_links() {
        let md = "array[0] is not a link";
        // "array" + "[0]" + "is" + "not" + "a" + "link" = 5+3+2+3+1+4 = 18
        assert_eq!(count_visible_content_chars(md), 18);
    }

    #[test]
    fn nested_brackets() {
        let md = "see [the [inner] thing](http://example.com)";
        // "see" + "the[inner]thing" = 3+15 = 18
        assert_eq!(count_visible_content_chars(md), 18);
    }
}
