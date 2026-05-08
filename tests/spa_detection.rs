//! SPA 检测逻辑集成测试：验证 tianqi.qq.com 等 SPA 页面能否被正确识别。

#![allow(clippy::print_stdout)]

use dom_smoothie::{Config, Readability};
use tokimo_web_fetch::cloudflare::looks_like_spa_or_blank;
use tokimo_web_fetch::denoise_html;

const SPA_BLANK_MIN_CHARS: usize = 120;
const BROWSER_ESCALATION_MIN_READABLE_CHARS: usize = 200;

async fn fetch_tianqi() -> (u16, String) {
    let client = reqwest::Client::builder()
        .user_agent(tokimo_web_fetch::DEFAULT_USER_AGENT)
        .build()
        .unwrap();
    let resp = client
        .get("https://tianqi.qq.com/")
        .send()
        .await
        .expect("HTTP request failed");
    let status = resp.status().as_u16();
    let body = resp.text().await.expect("failed to read body");
    (status, body)
}

/// 抓取 tianqi.qq.com 的原始 HTML，验证两阶段 SPA 检测。
#[tokio::test]
async fn tianqi_qq_com_spa_detection() {
    let (status, body) = fetch_tianqi().await;

    println!("=== tianqi.qq.com ===");
    println!("HTTP status: {status}");
    println!("Body length: {} bytes", body.len());

    // ── Stage 1: looks_like_spa_or_blank ──
    let is_spa_stage1 = looks_like_spa_or_blank(&body, SPA_BLANK_MIN_CHARS);
    let stripped = strip_for_debug(&body);
    let visible_chars = stripped.chars().filter(|c| !c.is_whitespace()).count();

    println!("\n[Stage 1] looks_like_spa_or_blank(body, {SPA_BLANK_MIN_CHARS})");
    println!("  Visible chars after strip: {visible_chars}");
    println!("  Result: is_spa = {is_spa_stage1}");

    // ── Stage 2: Readability 提取后 ──
    let denoised = denoise_html(&body, "https://tianqi.qq.com/", "https://tianqi.qq.com/");
    match &denoised {
        Ok(article) => {
            let readable_chars = article.content_text.chars().filter(|c| !c.is_whitespace()).count();
            println!("\n[Stage 2] Readability extracted:");
            println!("  Title: {}", article.title);
            println!("  content_text length: {} chars", article.content_text.len());
            println!("  Non-whitespace chars: {readable_chars}");
            println!(
                "  content_text preview: {:?}",
                &article.content_text[..article.content_text.len().min(300)]
            );
            println!(
                "  Would escalate ({} < {BROWSER_ESCALATION_MIN_READABLE_CHARS}): {}",
                readable_chars,
                readable_chars < BROWSER_ESCALATION_MIN_READABLE_CHARS
            );
        }
        Err(e) => {
            println!("\n[Stage 2] Readability FAILED: {e}");
            println!("  (denoise failure also triggers escalation)");
        }
    }

    let stage2_needs_escalation = match &denoised {
        Ok(article) => {
            article.content_text.chars().filter(|c| !c.is_whitespace()).count() < BROWSER_ESCALATION_MIN_READABLE_CHARS
        }
        Err(_) => true,
    };

    println!("\n=== Summary ===");
    println!("  Stage 1 (SPA shell):  {is_spa_stage1}");
    println!("  Stage 2 (thin content): {stage2_needs_escalation}");

    assert!(
        is_spa_stage1 || stage2_needs_escalation,
        "tianqi.qq.com is a SPA page — at least one stage should detect it.\n\
         Stage 1 visible chars: {visible_chars} (threshold: {SPA_BLANK_MIN_CHARS})\n\
         Stage 2 needs escalation: {stage2_needs_escalation}"
    );
}

/// 直接用 dom_smoothie 的不同 char_threshold 测试 Readability 提取效果。
#[tokio::test]
async fn tianqi_readability_with_different_thresholds() {
    let (_status, body) = fetch_tianqi().await;

    println!("=== Readability char_threshold 对比 ===");

    for threshold in [0, 100, 200, 500] {
        let cfg = Config {
            char_threshold: threshold,
            ..Config::default()
        };
        let result =
            Readability::new(body.clone(), Some("https://tianqi.qq.com/"), Some(cfg)).and_then(|mut r| r.parse());

        match result {
            Ok(article) => {
                let text = article.text_content.trim();
                let non_ws = text.chars().filter(|c| !c.is_whitespace()).count();
                println!(
                    "\n[char_threshold={threshold}] OK — title: {:?}, text chars: {non_ws}",
                    article.title
                );
                let preview: String = text.chars().take(100).collect();
                println!("  text preview: {preview:?}");
            }
            Err(e) => {
                println!("\n[char_threshold={threshold}] FAILED: {e}");
            }
        }
    }
}

/// 直接用 dom_smoothie 的 is_probably_readable 检查页面可读性。
#[tokio::test]
async fn tianqi_is_probably_readable() {
    let (_status, body) = fetch_tianqi().await;

    let result = Readability::new(body.clone(), Some("https://tianqi.qq.com/"), None);
    match result {
        Ok(reader) => {
            let readable = reader.is_probably_readable();
            println!("tianqi.qq.com is_probably_readable: {readable}");
            // SPA 页面通常不被认为可读
        }
        Err(e) => {
            println!("Readability::new failed: {e}");
        }
    }
}

/// 测试一个已知的非 SPA 静态页面（example.com）不应被误判。
#[tokio::test]
async fn example_com_not_spa() {
    let client = reqwest::Client::new();
    let body = client
        .get("https://example.com")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let is_spa = looks_like_spa_or_blank(&body, SPA_BLANK_MIN_CHARS);
    let visible = strip_for_debug(&body).chars().filter(|c| !c.is_whitespace()).count();

    println!("example.com: visible chars = {visible}, is_spa = {is_spa}");
    assert!(
        !is_spa,
        "example.com should NOT be detected as SPA (visible: {visible})"
    );
}

/// 测试一个极短的空白页应该被检测为 SPA。
#[test]
fn empty_html_is_spa() {
    let html = "<html><head><title></title></head><body></body></html>";
    assert!(looks_like_spa_or_blank(html, SPA_BLANK_MIN_CHARS));
}

/// 测试只有 script 的 SPA 壳应该被检测到。
#[test]
fn script_only_shell_is_spa() {
    let html = r#"
    <html>
    <head><title>Loading...</title></head>
    <body>
        <div id="app"></div>
        <script>
            window.__INITIAL_STATE__ = {"user":"test","data":[1,2,3]};
            fetch('/api/data').then(r => r.json()).then(render);
        </script>
    </body>
    </html>
    "#;
    assert!(looks_like_spa_or_blank(html, SPA_BLANK_MIN_CHARS));
}

/// 测试有丰富静态内容的页面不应被误判为 SPA。
#[test]
fn rich_static_page_not_spa() {
    let html = r#"
    <html>
    <head><title>News Article</title></head>
    <body>
        <nav><a href="/">Home</a> | <a href="/news">News</a></nav>
        <article>
            <h1>Breaking News: Something Important Happened</h1>
            <p>Lorem ipsum dolor sit amet, consectetur adipiscing elit.
            Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.
            Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris
            nisi ut aliquip ex ea commodo consequat.</p>
            <p>Duis aute irure dolor in reprehenderit in voluptate velit esse
            cillum dolore eu fugiat nulla pariatur. Excepteur sint occaecat
            cupidatat non proident, sunt in culpa qui officia deserunt mollit
            anim id est laborum.</p>
        </article>
        <footer>Copyright 2026</footer>
    </body>
    </html>
    "#;
    assert!(!looks_like_spa_or_blank(html, SPA_BLANK_MIN_CHARS));
}

/// 复制 strip_scripts_and_tags 的逻辑用于调试输出（不改源码可见性）。
fn strip_for_debug(body: &str) -> String {
    let mut out = String::with_capacity(body.len() / 2);
    let bytes = body.as_bytes();
    let lower: Vec<u8> = bytes.iter().map(u8::to_ascii_lowercase).collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(end) = find_closing_block(&lower, i) {
                i = end;
                continue;
            }
            if let Some(rel) = memchr_gt(&bytes[i..]) {
                i += rel + 1;
                continue;
            }
            break;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

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
