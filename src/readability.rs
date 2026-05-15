//! HTML 降噪：Readability 算法（通过 `dom_smoothie` crate）。
//!
//! 把抓来的整页 HTML 抽成"主文 + 标题 + byline"等结构化字段，
//! 适合喂给 LLM 或纯文本展示。

use crate::error::{FetchError, FetchResult};
use crate::keyword_boost::boost_html;
use dom_smoothie::{Article, Config, Readability, TextMode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DenoisedArticle {
    pub url: String,
    pub final_url: String,
    pub title: String,
    pub byline: Option<String>,
    pub excerpt: Option<String>,
    pub site_name: Option<String>,
    pub lang: Option<String>,
    pub length: usize,
    /// Readability 清洗后的主体 HTML
    pub content_html: String,
    /// 纯文本版本
    pub content_text: String,
}

/// 用 Readability 处理一段 HTML。
///
/// `base_url` 用于解析相对链接和 site_name，没有时传请求 URL 即可。
///
/// `keywords` 非空时，会先对 HTML 进行关键词注入预处理，
/// 提高包含关键词的元素在 Readability 评分中的权重。
pub fn denoise_html(html: &str, request_url: &str, base_url: &str, keywords: &[&str]) -> FetchResult<DenoisedArticle> {
    let processed = if keywords.is_empty() {
        html.to_string()
    } else {
        boost_html(html, keywords)
    };
    let config = Config {
        max_elements_to_parse: 0,
        text_mode: TextMode::Markdown,
        ..Config::default()
    };
    let mut readability = Readability::new(processed, Some(base_url), Some(config))
        .map_err(|e| FetchError::Readability(e.to_string()))?;
    let article: Article = readability
        .parse()
        .map_err(|e| FetchError::Readability(e.to_string()))?;
    // 剥离关键词注入的零宽填充字符
    let content_text: String = article.text_content.chars().filter(|&c| c != '\u{200B}').collect();
    let content_html: String = article.content.to_string().replace('\u{200B}', "");
    Ok(DenoisedArticle {
        url: request_url.to_string(),
        final_url: base_url.to_string(),
        title: article.title,
        byline: article.byline,
        excerpt: article.excerpt,
        site_name: article.site_name,
        lang: article.lang,
        length: article.length,
        content_html,
        content_text,
    })
}
