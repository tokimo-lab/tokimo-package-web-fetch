//! Chrome headless browser integration tests.
//!
//! Requires Chrome or Chromium to be installed on the system.
//! CI installs it via `setup-chrome` action.

use tokimo_web_fetch::{BrowserFetch, ChromeBrowser, autodetect_browser};

#[tokio::test]
async fn chrome_autodetect_finds_binary() {
    let chrome = ChromeBrowser::autodetect();
    // 如果系统没装 Chrome，跳过（CI 会装）
    let Some(chrome) = chrome else {
        return;
    };
    assert!(!chrome.name().is_empty());
}

#[tokio::test]
async fn chrome_fetch_example_com() {
    let Some(chrome) = ChromeBrowser::autodetect() else {
        return;
    };

    let html = chrome
        .fetch_html("https://example.com")
        .await
        .expect("fetch_html should succeed");

    assert!(
        html.contains("Example Domain"),
        "HTML should contain 'Example Domain', got: {html:.200}"
    );
}

#[tokio::test]
async fn autodetect_browser_returns_some() {
    let browser = autodetect_browser();
    let Some(browser) = browser else {
        return;
    };

    let html = browser
        .fetch_html("https://example.com")
        .await
        .expect("fetch_html should succeed");

    assert!(html.contains("Example Domain"));
}
