//! Cloudflare 挑战检测 + FlareSolverr bypass 客户端。
//!
//! 适用场景：站点用 Cloudflare / DDoS-Guard 挡 JS 执行 / cookie 校验，
//! 普通 reqwest GET 拿到的是 "Just a moment..." 挑战页。
//!
//! 解决：把请求转发到本地或远程的 [FlareSolverr](https://github.com/FlareSolverr/FlareSolverr)
//! 服务，由它驱动真浏览器解出 `cf_clearance` 等 cookie 后回放。

use crate::error::{FetchError, FetchResult};
use serde::{Deserialize, Serialize};

/// 一次 HTML 抓取的结果。
#[derive(Debug, Clone)]
pub struct CfFetchResult {
    pub status: u16,
    pub body: String,
    /// 实际生效的 URL（FlareSolverr 跟随重定向后的地址；普通 fetch 时等于请求 URL）
    pub final_url: String,
}

/// 走 FlareSolverr 的 Cloudflare bypass 客户端。
///
/// `flaresolverr_url` 为 None 时退化为普通 reqwest GET。
pub struct CloudflareBypassClient {
    http: reqwest::Client,
    flaresolverr_url: Option<String>,
}

#[derive(Serialize)]
struct FlareSolverrRequest {
    cmd: String,
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cookies: Option<Vec<FlareSolverrCookie>>,
    #[serde(rename = "maxTimeout")]
    max_timeout: u32,
}

#[derive(Serialize)]
struct FlareSolverrCookie {
    name: String,
    value: String,
}

#[derive(Deserialize)]
struct FlareSolverrResponse {
    status: Option<String>,
    message: Option<String>,
    solution: Option<FlareSolverrSolution>,
}

#[derive(Deserialize)]
struct FlareSolverrSolution {
    url: Option<String>,
    status: Option<u16>,
    response: Option<String>,
}

impl CloudflareBypassClient {
    #[must_use]
    pub fn new(flaresolverr_url: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(crate::DEFAULT_USER_AGENT)
            .build()
            .unwrap_or_default();
        Self { http, flaresolverr_url }
    }

    /// 用一个外部 reqwest::Client（比如带了 cookie store / 代理 / 自定义 UA）。
    #[must_use]
    pub fn with_client(http: reqwest::Client, flaresolverr_url: Option<String>) -> Self {
        Self { http, flaresolverr_url }
    }

    /// 抓 HTML：优先 FlareSolverr，失败回退普通 GET。
    pub async fn fetch_html(&self, url: &str, cookie: Option<&str>) -> FetchResult<CfFetchResult> {
        if let Some(ref flaresolverr_url) = self.flaresolverr_url {
            match self.fetch_via_flaresolverr(flaresolverr_url, url, cookie).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    tracing::warn!("FlareSolverr 失败，回退到直接抓取: {e}");
                }
            }
        }
        self.fetch_direct(url, cookie).await
    }

    async fn fetch_via_flaresolverr(
        &self,
        flaresolverr_url: &str,
        url: &str,
        cookie: Option<&str>,
    ) -> FetchResult<CfFetchResult> {
        let cookies = cookie.map(|c| {
            c.split(';')
                .filter_map(|pair| {
                    let pair = pair.trim();
                    let (name, value) = pair.split_once('=')?;
                    Some(FlareSolverrCookie {
                        name: name.trim().to_string(),
                        value: value.trim().to_string(),
                    })
                })
                .collect()
        });

        let req = FlareSolverrRequest {
            cmd: "request.get".to_string(),
            url: url.to_string(),
            cookies,
            max_timeout: 30_000,
        };

        let endpoint = format!("{}/v1", flaresolverr_url.trim_end_matches('/'));
        let resp = self
            .http
            .post(&endpoint)
            .json(&req)
            .send()
            .await
            .map_err(|e| FetchError::CloudflareBypass(format!("post failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(FetchError::CloudflareBypass(format!(
                "FlareSolverr HTTP {}",
                resp.status()
            )));
        }
        let data: FlareSolverrResponse = resp
            .json()
            .await
            .map_err(|e| FetchError::CloudflareBypass(format!("decode response: {e}")))?;
        if data.status.as_deref() != Some("ok") {
            return Err(FetchError::CloudflareBypass(format!(
                "FlareSolverr non-ok: {}",
                data.message.unwrap_or_default()
            )));
        }
        let solution = data
            .solution
            .ok_or_else(|| FetchError::CloudflareBypass("no solution returned".into()))?;
        Ok(CfFetchResult {
            status: solution.status.unwrap_or(200),
            body: solution.response.unwrap_or_default(),
            final_url: solution.url.unwrap_or_else(|| url.to_string()),
        })
    }

    async fn fetch_direct(&self, url: &str, cookie: Option<&str>) -> FetchResult<CfFetchResult> {
        let mut req = self.http.get(url);
        if let Some(cookie) = cookie {
            req = req.header("Cookie", cookie);
        }
        let resp = req.send().await?;
        let final_url = resp.url().to_string();
        let status = resp.status().as_u16();
        let body = resp.text().await?;
        Ok(CfFetchResult {
            status,
            body,
            final_url,
        })
    }
}

/// 检测 HTML 是否是 Cloudflare / DDoS-Guard 挑战页。
#[must_use]
pub fn is_under_challenge(body: &str) -> bool {
    let lower = body.to_lowercase();
    lower.contains("just a moment")
        || lower.contains("请稍候")
        || lower.contains("cf-challenge-running")
        || lower.contains("cf-please-wait")
        || lower.contains("challenge-spinner")
        || lower.contains("trk_jschal_js")
        || lower.contains("ddos-guard")
}

/// 检测 HTML 是否命中典型"反爬 / UA 黑名单 / 人机验证"页面。
///
/// 这些页面通常很短、内容与用户 query 完全无关，但不是 Cloudflare
/// 标准挑战（[`is_under_challenge`] 不会命中）。典型来源：
/// - 阿里 Tengine CDN 的 `denied by UA ACL = blacklist` 403 页
/// - 百度系的"百度安全验证 / 网络不给力"墙
/// - 通用 "Access Denied" / "人机验证" / "滑动验证"
///
/// 命中这些的页面应走无头浏览器通道重试，或者交由上层 fallback。
#[must_use]
pub fn has_anti_bot_wall(body: &str) -> bool {
    if body.len() > 32 * 1024 {
        // 大页面一般是正常内容带少量反爬关键字（例如新闻正文里提到"人机验证"）。
        // 小页面 + 关键字组合才是反爬墙。
        return false;
    }
    let lower = body.to_lowercase();
    lower.contains("denied by ua acl")
        || lower.contains("百度安全验证")
        || lower.contains("网络不给力")
        || lower.contains("access denied")
        || lower.contains("人机验证")
        || lower.contains("滑动验证")
        || lower.contains("访问验证")
        || (lower.contains("403 forbidden") && body.len() < 2048)
}

/// 检测 HTML 是否"看起来是 SPA 壳子 / 无实质文本"。
///
/// 策略：粗略去掉 `<script>` / `<style>` / `<noscript>` 后统计可见文本长度；
/// 小于 `min_chars` 视为 SPA 壳或空白页（真正内容需要 JS 执行才能看到），
/// 此时应该升级到无头浏览器。
#[must_use]
pub fn looks_like_spa_or_blank(body: &str, min_chars: usize) -> bool {
    let text = strip_scripts_and_tags(body);
    let visible: usize = text.chars().filter(|c| !c.is_whitespace()).count();
    visible < min_chars
}

/// 极简 HTML → 纯文本：跳过 `<script>` / `<style>` / `<noscript>` 块和所有标签。
/// 不追求严谨，只用于"页面是不是实质为空"的启发式判断。
fn strip_scripts_and_tags(body: &str) -> String {
    let mut out = String::with_capacity(body.len() / 2);
    let bytes = body.as_bytes();
    let lower: Vec<u8> = bytes.iter().map(u8::to_ascii_lowercase).collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            // Check for script/style/noscript blocks and skip their content entirely.
            let skip_end = find_closing_block(&lower, i);
            if let Some(end) = skip_end {
                i = end;
                continue;
            }
            // Otherwise skip just the single tag.
            if let Some(rel) = memchr_gt(&bytes[i..]) {
                i += rel + 1;
                continue;
            }
            break;
        }
        // Copy char; we operate byte-wise but only emit ASCII-safe boundaries.
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// 若 `i` 指向 `<script` / `<style` / `<noscript`，返回对应闭合标签后的偏移；否则 `None`。
fn find_closing_block(lower: &[u8], i: usize) -> Option<usize> {
    const BLOCKS: &[(&[u8], &[u8])] = &[
        (b"<script", b"</script>"),
        (b"<style", b"</style>"),
        (b"<noscript", b"</noscript>"),
    ];
    for (open, close) in BLOCKS {
        if lower[i..].starts_with(open) {
            let search_from = i + open.len();
            if let Some(rel) = find_subslice(&lower[search_from..], close) {
                return Some(search_from + rel + close.len());
            }
            return Some(lower.len());
        }
    }
    None
}

fn memchr_gt(bytes: &[u8]) -> Option<usize> {
    bytes.iter().position(|&b| b == b'>')
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}
