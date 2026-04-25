//! Ad-hoc smoke test for the auto-escalation logic.
//!
//! 跑法：`cargo run -p tokimo-web-fetch --example test_escalation`
//! 预期每个 URL 都应该拿到 >= 200 字符的 Readability 正文（经由
//! 无头浏览器升级后的结果），而不是 403/SPA 壳子。

use std::time::Duration;
use tokimo_web_fetch::{Denoise, FetchMode, FetchOptions, WebFetcher};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("tokimo_web_fetch=info")
        .init();

    let http = reqwest::Client::builder()
        .user_agent(tokimo_web_fetch::DEFAULT_USER_AGENT)
        .gzip(true)
        .brotli(true)
        .cookie_store(true)
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();
    let fetcher = WebFetcher::builder()
        .http_client(http)
        .with_lightpanda_autodetect()
        .build();

    let urls = [
        "https://tianqi.eastday.com/tianqi/shanghai/20260418.html",
        "https://m.tianqi.com/tianqi/shanghai/20260418.html?qd=tq7",
        "https://www.tianqi.com/shanghai/",
        "https://www.doubao.com/",
        "http://sh.cma.gov.cn/fx/home/",
        "https://www.shqp.gov.cn/shqp/ggfw/tq/",
    ];
    let opts = FetchOptions {
        mode: FetchMode::Auto,
        denoise: Denoise::Readability,
        timeout: Duration::from_secs(30),
        ..Default::default()
    };
    for url in urls {
        println!("\n===== {url} =====");
        match fetcher.fetch_with(url, &opts).await {
            Ok(resp) => {
                let text = resp
                    .denoised
                    .as_ref()
                    .map_or("(none)", |d| d.content_text.as_str());
                let chars: usize = text.chars().filter(|c| !c.is_whitespace()).count();
                println!(
                    "status={} used={:?} body_bytes={} readable_chars={}",
                    resp.status,
                    resp.used,
                    resp.body.len(),
                    chars
                );
                let preview: String = text.chars().take(300).collect();
                println!("preview: {preview}");
            }
            Err(e) => println!("ERR: {e}"),
        }
    }
}
