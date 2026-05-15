//! 端到端测试：通过 WebFetcher + Chrome headless 从 tianqi.qq.com 抓取天气数据。
//!
//! 需要系统安装 Chrome/Chromium，且网络可达 tianqi.qq.com（国内环境）。
//! CI 海外 runner 可能无法访问，fetch 失败时自动跳过。

#![allow(clippy::print_stdout)]

use std::sync::Arc;
use std::time::Duration;

use tokimo_web_fetch::browser::ChromeBrowser;
use tokimo_web_fetch::fetcher::{Denoise, FetchMode, FetchOptions, WebFetcher};

const URL: &str = "https://tianqi.qq.com/";

#[tokio::test]
async fn fetch_tianqi_weather_via_headless_browser() {
    let Some(chrome) = ChromeBrowser::autodetect() else {
        return;
    };

    let fetcher = WebFetcher::builder().browser(Arc::new(chrome)).build();

    let opts = FetchOptions {
        mode: FetchMode::Browser,
        denoise: Denoise::None,
        timeout: Duration::from_secs(30),
        ..FetchOptions::default()
    };

    // CI 海外 runner 可能无法访问中国站点，fetch 失败时跳过而非 panic
    let Ok(resp) = fetcher.fetch_with(URL, &opts).await else {
        return;
    };

    let body = &resp.body;

    // 温度：数字 + °
    let has_temperature = body.contains("txt-temperature")
        && body
            .lines()
            .any(|l| l.contains("txt-temperature") && l.chars().any(|c| c.is_ascii_digit()) && l.contains('°'));
    assert!(
        has_temperature,
        "should have temperature data, body preview:\n{body:.500}"
    );

    // 天气状况
    let weather_keywords = [
        "晴", "多云", "阴", "雨", "雪", "雾", "霾", "阵雨", "小雨", "中雨", "大雨",
    ];
    let has_weather = body.contains("txt-name") && weather_keywords.iter().any(|kw| body.contains(kw));
    assert!(has_weather, "should have weather condition, body preview:\n{body:.500}");

    // 风力
    let has_wind = body.contains("txt-wind") && body.contains("风");
    assert!(has_wind, "should have wind info, body preview:\n{body:.500}");

    // 湿度
    let has_humidity = body.contains("txt-humidity") && body.contains("湿度");
    assert!(has_humidity, "should have humidity info, body preview:\n{body:.500}");
}

/// 浏览器抓取原始 HTML，搜索天气核心数据元素的位置。
#[tokio::test]
async fn tianqi_raw_html_inspect() {
    let Some(chrome) = ChromeBrowser::autodetect() else {
        return;
    };

    let fetcher = WebFetcher::builder().browser(Arc::new(chrome)).build();

    let opts = FetchOptions {
        mode: FetchMode::Browser,
        denoise: Denoise::None,
        timeout: Duration::from_secs(30),
        ..FetchOptions::default()
    };

    let Ok(resp) = fetcher.fetch_with(URL, &opts).await else {
        return;
    };

    let body = &resp.body;

    // 搜索关键 class，用 char boundary 安全的方式截取上下文
    for keyword in &[
        "txt-temperature",
        "txt-name",
        "txt-wind",
        "txt-humidity",
        "forecast",
        "weather",
        "city",
        "current",
    ] {
        if let Some(byte_pos) = body.find(keyword) {
            let start = body[..byte_pos].char_indices().rev().nth(150).map_or(0, |(i, _)| i);
            let end = body[byte_pos + keyword.len()..]
                .char_indices()
                .nth(300)
                .map_or(body.len(), |(i, _)| byte_pos + keyword.len() + i);

            println!("\n=== Found '{keyword}' at byte {byte_pos} ===");
            println!("...{}...", &body[start..end]);
        } else {
            println!("\n=== '{keyword}' NOT FOUND ===");
        }
    }
}

/// 浏览器抓取 + Readability Markdown 降噪，输出完整 Markdown 结果。
#[tokio::test]
async fn tianqi_markdown_output() {
    let Some(chrome) = ChromeBrowser::autodetect() else {
        return;
    };

    let fetcher = WebFetcher::builder().browser(Arc::new(chrome)).build();

    let opts = FetchOptions {
        mode: FetchMode::Browser,
        denoise: Denoise::Readability,
        timeout: Duration::from_secs(30),
        keywords: vec!["温度".into(), "天气".into(), "风".into(), "湿度".into()],
        ..FetchOptions::default()
    };

    let Ok(resp) = fetcher.fetch_with(URL, &opts).await else {
        return;
    };

    if let Some(ref article) = resp.denoised {
        println!("\n========== Denoised Article ==========");
        println!("Title: {}", article.title);
        println!("Byline: {:?}", article.byline);
        println!("Length: {}", article.length);
        println!("\n========== content_text (Markdown) ==========\n");
        println!("{}", article.content_text);
        println!("\n========== content_html ==========\n");
        println!("{}", article.content_html);
    } else {
        println!("No denoised article, raw body preview:\n{:.2000}", resp.body);
    }
}
