//! HTML 降噪：Readability 算法（通过 `dom_smoothie` crate）。
//!
//! 把抓来的整页 HTML 抽成"主文 + 标题 + byline"等结构化字段，
//! 适合喂给 LLM 或纯文本展示。

use crate::error::{FetchError, FetchResult};
use dom_smoothie::{Article, Config, Readability};
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
pub fn denoise_html(html: &str, request_url: &str, base_url: &str) -> FetchResult<DenoisedArticle> {
    let config = Config {
        max_elements_to_parse: 0,
        ..Config::default()
    };
    let mut readability = Readability::new(html.to_string(), Some(base_url), Some(config))
        .map_err(|e| FetchError::Readability(e.to_string()))?;
    let article: Article = readability
        .parse()
        .map_err(|e| FetchError::Readability(e.to_string()))?;
    Ok(DenoisedArticle {
        url: request_url.to_string(),
        final_url: base_url.to_string(),
        title: article.title,
        byline: article.byline,
        excerpt: article.excerpt,
        site_name: article.site_name,
        lang: article.lang,
        length: article.length,
        content_html: article.content.to_string(),
        content_text: article.text_content.to_string(),
    })
}
