//! 端到端测试：通过 WebFetcher + Chrome headless 从 tianqi.qq.com 抓取天气数据。

use std::sync::Arc;
use std::time::Duration;

use tokimo_web_fetch::browser::ChromeBrowser;
use tokimo_web_fetch::fetcher::{Denoise, FetchMode, FetchOptions, WebFetcher};

/// 验证 WebFetcher + Chrome headless 默认配置能从 tianqi.qq.com 拿到天气数据。
#[tokio::test]
async fn fetch_tianqi_weather_via_headless_browser() {
    let Some(chrome) = ChromeBrowser::autodetect() else {
        eprintln!("Chrome not found, skipping");
        return;
    };

    let fetcher = WebFetcher::builder()
        .browser(Arc::new(chrome))
        .build();

    let opts = FetchOptions {
        mode: FetchMode::Browser,
        denoise: Denoise::None,
        timeout: Duration::from_secs(30),
        ..FetchOptions::default()
    };

    let resp = fetcher
        .fetch_with("https://tianqi.qq.com/", &opts)
        .await
        .expect("fetch should succeed");

    let body = &resp.body;

    // 温度：数字 + °
    let has_temperature = body.contains("txt-temperature")
        && body.lines().any(|l| {
            l.contains("txt-temperature")
                && l.chars().any(|c| c.is_ascii_digit())
                && l.contains('°')
        });
    assert!(has_temperature, "should have temperature data, body preview:\n{:.500}", body);

    // 天气状况：至少包含常见天气词之一
    let weather_keywords = ["晴", "多云", "阴", "雨", "雪", "雾", "霾", "阵雨", "小雨", "中雨", "大雨"];
    let has_weather = body.contains("txt-name")
        && weather_keywords.iter().any(|kw| body.contains(kw));
    assert!(has_weather, "should have weather condition, body preview:\n{:.500}", body);

    // 风力
    let has_wind = body.contains("txt-wind") && body.contains("风");
    assert!(has_wind, "should have wind info, body preview:\n{:.500}", body);

    // 湿度
    let has_humidity = body.contains("txt-humidity") && body.contains("湿度");
    assert!(has_humidity, "should have humidity info, body preview:\n{:.500}", body);

    println!("=== tianqi.qq.com weather data OK ===");
    println!("channel: {:?}", resp.used);
    for line in body.lines() {
        let trimmed = line.trim();
        if ["txt-temperature", "txt-name", "txt-wind", "txt-humidity"]
            .iter()
            .any(|id| trimmed.contains(id))
        {
            println!("  {trimmed}");
        }
    }
}
