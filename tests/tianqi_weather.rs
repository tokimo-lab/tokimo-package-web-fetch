//! 端到端测试：通过 WebFetcher + Chrome headless 从 tianqi.qq.com 抓取天气数据。
//!
//! 需要系统安装 Chrome/Chromium，且网络可达 tianqi.qq.com（国内环境）。
//! CI 海外 runner 可能无法访问，fetch 失败时自动跳过。

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
