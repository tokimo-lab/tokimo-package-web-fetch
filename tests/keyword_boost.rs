//! Keyword boost 算法测试：验证关键词注入能提高 Readability 提取精度。

#![allow(clippy::print_stdout)]

use dom_smoothie::{Config, Readability, TextMode};
use tokimo_web_fetch::keyword_boost::boost_html;

/// 注入的第一个正向 class 名，用于快速断言
const BOOST_CLASS: &str = "article";

// ── 基础注入 ──

#[test]
fn boosts_element_containing_keyword() {
    let html = r#"
    <html><body>
        <div id="weather">
            <p>温度 25°</p>
            <p>天气 阴</p>
        </div>
        <div id="noise">
            <p>这是一大段无关内容，用来干扰 Readability 的评分算法。需要足够长才能胜出。</p>
            <p>更多的无关内容，填充填充填充填充填充填充填充填充填充填充。</p>
            <p>继续填充，确保这段文字的总长度远远超过天气数据的长度。</p>
        </div>
    </body></html>
    "#;

    let boosted = boost_html(html, &["温度"]);
    assert!(boosted.contains(BOOST_CLASS), "should contain boost class");
    let weather_pos = boosted.find("id=\"weather\"").unwrap();
    let boost_pos = boosted.find(BOOST_CLASS).unwrap();
    assert!(
        boost_pos > weather_pos && boost_pos < weather_pos + 200,
        "boost class should be near the weather div"
    );
}

#[test]
fn no_boost_without_keywords() {
    let html = r"<html><body><div><p>温度 25°</p></div></body></html>";
    let boosted = boost_html(html, &[]);
    assert!(
        !boosted.contains(BOOST_CLASS),
        "empty keywords should not inject anything"
    );
    assert_eq!(boosted, html);
}

#[test]
fn no_boost_when_no_match() {
    let html = r"<html><body><div><p>无关内容</p></div></body></html>";
    let boosted = boost_html(html, &["温度", "天气"]);
    assert!(!boosted.contains(BOOST_CLASS), "no match should not inject anything");
}

// ── 多关键词 ──

#[test]
fn multiple_keywords_any_match() {
    let html = r#"
    <html><body>
        <div id="a"><p>温度 25°</p></div>
        <div id="b"><p>风力 南风4级</p></div>
        <div id="c"><p>湿度 57%</p></div>
    </body></html>
    "#;

    let boosted = boost_html(html, &["温度", "风力", "湿度"]);
    assert!(
        boosted.matches(BOOST_CLASS).count() >= 3,
        "each matching div should be boosted"
    );
}

// ── 祖先追溯 ──

#[test]
fn boosts_ancestor_not_text_node() {
    let html = r#"
    <html><body>
        <div id="container">
            <span><em>温度</em> 25°</span>
        </div>
    </body></html>
    "#;

    let boosted = boost_html(html, &["温度"]);
    assert!(boosted.contains(BOOST_CLASS));
    assert!(
        !boosted.contains(&format!("<span class=\"{BOOST_CLASS}\"")),
        "should not boost inline elements like span"
    );
}

// ── 去重 ──

#[test]
fn dedup_same_element() {
    let html = r#"
    <html><body>
        <div id="d"><p>温度 25° 温度 温度</p></div>
    </body></html>
    "#;

    let boosted = boost_html(html, &["温度"]);
    assert_eq!(
        boosted.matches(BOOST_CLASS).count(),
        1,
        "same element should only be boosted once"
    );
}

// ── 不修改已有 class ──

#[test]
fn preserves_existing_class() {
    let html = r#"
    <html><body>
        <div class="existing" id="d"><p>温度 25°</p></div>
    </body></html>
    "#;

    let boosted = boost_html(html, &["温度"]);
    assert!(boosted.contains("existing"), "existing class should be preserved");
    assert!(boosted.contains(BOOST_CLASS), "boost class should be added");
}

// ── 不注入顶层元素 ──

#[test]
fn does_not_boost_body_or_html() {
    let html = r"<html><body>温度 25°</body></html>";
    let boosted = boost_html(html, &["温度"]);
    assert!(
        !boosted.contains(&format!("<body class=\"{BOOST_CLASS}\"")),
        "should not boost body"
    );
    assert!(
        !boosted.contains(&format!("<html class=\"{BOOST_CLASS}\"")),
        "should not boost html"
    );
}

// ── 大小写不敏感 ──

#[test]
fn case_insensitive_matching() {
    let html = r#"
    <html><body>
        <div id="d"><p>Temperature 25°</p></div>
    </body></html>
    "#;

    let boosted = boost_html(html, &["temperature"]);
    assert!(boosted.contains(BOOST_CLASS));
}

// ── Readability 集成 ──

/// 两个内容量相当的 div，class boost 应该决定胜出者
#[test]
fn readability_boost_tips_balanced_scales() {
    let html = r#"
    <html><body>
        <div id="target">
            <p>温度 25°，天气阴，南风4-5级，湿度57%。今日空气质量良好，适合户外活动。</p>
            <p>明天预计温度 22°，多云转晴，北风3-4级。</p>
        </div>
        <div id="noise">
            <p>穿衣建议：天气热，建议着短裙、短裤、短薄外套、T恤等夏季服装。</p>
            <p>雨伞建议：天气较好，不会降水，因此您可放心出门，无须带雨伞。</p>
        </div>
    </body></html>
    "#;

    // 不注入：Readability 可能选 noise（内容稍多）
    let config = Config {
        text_mode: TextMode::Markdown,
        ..Config::default()
    };
    let mut reader = Readability::new(html.to_string(), Some("http://test.com"), Some(config.clone())).unwrap();
    let article = reader.parse().unwrap();
    let without_boost = article.text_content;

    // 注入关键词到 target
    let boosted_html = boost_html(html, &["温度"]);
    let mut reader = Readability::new(boosted_html, Some("http://test.com"), Some(config)).unwrap();
    let article = reader.parse().unwrap();
    let with_boost = article.text_content;

    // boost 后 target 的内容应该出现
    assert!(
        with_boost.contains("25°"),
        "with boost, Readability should extract target content.\n\
         Without: {without_boost}\nWith: {with_boost}"
    );
}

/// 模拟 tianqi.qq.com 的结构：天气数据内容短，生活指数内容多
/// 关键词注入 + 隐藏填充应该让 Readability 选择天气数据
#[test]
fn readability_extracts_boosted_content() {
    let html = r#"
    <html><body>
        <div id="ct-current">
            <p id="txt-temperature">25°</p>
            <p id="txt-name">阴</p>
            <span id="txt-wind">南风 4-5级</span>
            <span id="txt-humidity">湿度 57%</span>
        </div>
        <div id="ct-pages">
            <ul>
                <li><div><p>穿衣 热</p></div><div><p>天气热，建议着短裙、短裤、短薄外套、T恤等夏季服装。</p></div></li>
                <li><div><p>雨伞 不带伞</p></div><div><p>天气较好，不会降水，因此您可放心出门，无须带雨伞。</p></div></li>
                <li><div><p>感冒 易发</p></div><div><p>相对于今天将会出现大幅度降温，易发生感冒，请注意适当增加衣服。</p></div></li>
                <li><div><p>洗车 适宜</p></div><div><p>适宜洗车，至少可维持3天</p></div></li>
                <li><div><p>运动 适宜</p></div><div><p>天气较好，赶快投身大自然参与户外运动，尽情感受运动的快乐吧。</p></div></li>
                <li><div><p>防晒 强</p></div><div><p>属强紫外辐射天气，应加强防护，建议涂擦SPF在15-20之间的防晒护肤品。</p></div></li>
                <li><div><p>钓鱼 较适宜</p></div><div><p>较适合垂钓，但天气稍热，会对垂钓产生一定的影响。</p></div></li>
                <li><div><p>旅游 适宜</p></div><div><p>天气较好，但丝毫不会影响您出行的心情。温度适宜又有微风相伴，适宜旅游。</p></div></li>
                <li><div><p>交通 良好</p></div><div><p>天气较好，路面干燥，交通气象条件良好，车辆可以正常行驶。</p></div></li>
                <li><div><p>舒适度 舒适</p></div><div><p>白天温度适宜，风力不大，相信您在这样的天气条件下，应会感到比较清爽和舒适。</p></div></li>
            </ul>
        </div>
    </body></html>
    "#;

    let config = Config {
        text_mode: TextMode::Markdown,
        ..Config::default()
    };

    // 不注入关键词：Readability 会选择 ct-pages（内容更多）
    let mut reader = Readability::new(html.to_string(), Some("http://test.com"), Some(config.clone())).unwrap();
    let article = reader.parse().unwrap();
    let without_boost = article.text_content;
    println!("Without boost:\n{without_boost}\n");
    assert!(
        !without_boost.contains("25°"),
        "without boost, Readability should NOT extract temperature"
    );

    // 注入关键词后：Readability 应该选择 ct-current
    let boosted_html = boost_html(html, &["湿度"]);
    for line in boosted_html.lines() {
        if line.contains("opacity") || line.contains("article") {
            println!("BOOSTED: {}", line.trim());
        }
    }
    let mut reader = Readability::new(boosted_html, Some("http://test.com"), Some(config)).unwrap();
    let article = reader.parse().unwrap();
    // 模拟 denoise_html 的后处理：剥离零宽填充
    let with_boost: String = article.text_content.chars().filter(|&c| c != '\u{200B}').collect();
    println!("With boost:\n{with_boost}\n");
    assert!(
        with_boost.contains("25°"),
        "with boost, Readability SHOULD extract temperature"
    );
}

// ── 边界情况 ──

#[test]
fn empty_html() {
    let boosted = boost_html("", &["温度"]);
    assert_eq!(boosted, "");
}

#[test]
fn malformed_html() {
    let html = "<div><p>温度 25°</div>";
    let boosted = boost_html(html, &["温度"]);
    assert!(boosted.contains(BOOST_CLASS));
}

#[test]
fn keyword_in_attribute_not_text() {
    let html = r#"<html><body><div data-info="温度"><p>无关内容</p></div></body></html>"#;
    let boosted = boost_html(html, &["温度"]);
    println!("attr-only match: {}", boosted.contains(BOOST_CLASS));
}
