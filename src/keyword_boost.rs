//! 关键词注入：在 Readability 处理前，根据关键词预处理 HTML，提高目标内容的评分。

#![allow(clippy::redundant_closure_for_method_calls)]

use dom_query::Document;

/// Readability 的 CLASSES_POSITIVE 匹配规则是子串匹配。
/// 注入多个正向 class 可叠加 +25 分/个。
const BOOST_CLASSES: &[&str] = &["article", "content", "main", "text"];

/// Readability 会跳过字符数低于此阈值的元素（`grab.rs` line 240）。
/// 对于短内容（如天气数据 "25°"），仅注入 class 不够，
/// 还需要注入隐藏文本让元素进入评分流程。
/// 使用较大的值以确保长度奖励（min(len/100, 3)）最大化。
const MIN_SCOREABLE_LEN: usize = 300;

/// 零宽空格（U+200B）：计入字符数但不影响显示。
/// Readability 的 `CharCounterCache` 按 char 计数，零宽空格算一个 char。
const FILLER_CHAR: char = '\u{200B}';

/// 对 HTML 进行关键词注入预处理。
///
/// 找到包含任意 `keyword` 的文本节点，向上追溯祖先元素，
/// 注入正向 class 提高 Readability 评分。
///
/// `keywords` 为空或无匹配时返回原始 html。
pub fn boost_html(html: &str, keywords: &[&str]) -> String {
    if keywords.is_empty() || html.is_empty() {
        return html.to_string();
    }

    let lower_keywords: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();

    let doc = Document::from(html);
    let Some(body) = doc.body() else {
        return html.to_string();
    };

    // Pass 1: 扫描文本节点，收集需要注入的元素标识
    let mut targets: Vec<BoostTarget> = Vec::new();

    for node in body.descendants_it() {
        if !node.is_text() {
            continue;
        }
        let text = node.query(|n| {
            if let dom_query::NodeData::Text { ref contents } = n.data {
                contents.to_string()
            } else {
                String::new()
            }
        });
        let Some(text) = text else { continue };
        let lower_text = text.to_lowercase();
        if !lower_keywords.iter().any(|kw| lower_text.contains(kw.as_str())) {
            continue;
        }

        if let Some(target) = find_boost_target(&node) {
            let bt = BoostTarget::from_node(&target);
            if !targets.contains(&bt) {
                targets.push(bt);
            }
        }
    }

    if targets.is_empty() {
        return html.to_string();
    }

    // Pass 2: 用字符串操作注入 class（避免 RefCell 冲突）
    inject_classes(html, &targets)
}

/// 描述一个需要注入 class 的元素
#[derive(Debug, PartialEq)]
struct BoostTarget {
    tag: String,
    id: String,
    class: String,
}

impl BoostTarget {
    fn from_node(node: &dom_query::NodeRef) -> Self {
        let tag = node.qual_name_ref().map(|qn| qn.local.to_string()).unwrap_or_default();
        let id = node
            .query(|n| {
                n.as_element()
                    .and_then(|e| e.id())
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        let class = node
            .query(|n| {
                n.as_element()
                    .and_then(|e| e.class())
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        BoostTarget { tag, id, class }
    }
}

/// 容器级块元素 — 优先作为注入目标
const CONTAINER_TAGS: &[&str] = &[
    "div", "section", "article", "main", "aside", "header", "footer", "nav", "figure", "table", "form",
];

/// 叶子级块元素 — 仅作为 fallback
const LEAF_BLOCK_TAGS: &[&str] = &["p", "pre", "blockquote", "figcaption", "ul", "ol", "dl", "fieldset"];

/// 从文本节点向上追溯，找到合适的注入目标。
///
/// 策略：
/// 1. 优先：有 id/class 的容器级元素（div/section/article 等）
/// 2. 次选：任意容器级元素
/// 3. 兜底：叶子级块元素（p/pre 等）
///
/// 跳过 body/html。
fn find_boost_target<'a>(node: &dom_query::NodeRef<'a>) -> Option<dom_query::NodeRef<'a>> {
    let mut current = node.parent();
    let mut depth = 0;
    const MAX_DEPTH: usize = 5;
    let mut container_fallback: Option<dom_query::NodeRef<'a>> = None;
    let mut leaf_fallback: Option<dom_query::NodeRef<'a>> = None;

    while let Some(ref parent) = current {
        if depth >= MAX_DEPTH {
            break;
        }
        if parent.has_name("body") || parent.has_name("html") {
            return container_fallback.or(leaf_fallback);
        }
        if let Some(qn) = parent.qual_name_ref() {
            let name = qn.local.as_ref();
            let is_container = CONTAINER_TAGS.contains(&name);
            let is_leaf = LEAF_BLOCK_TAGS.contains(&name);

            if is_container || is_leaf {
                let has_id = parent
                    .query(|n| n.as_element().and_then(|e| e.id()).is_some_and(|s| !s.is_empty()))
                    .unwrap_or(false);
                let has_class = parent
                    .query(|n| n.as_element().and_then(|e| e.class()).is_some_and(|s| !s.is_empty()))
                    .unwrap_or(false);

                if is_container {
                    if has_id || has_class {
                        return Some(*parent);
                    }
                    if container_fallback.is_none() {
                        container_fallback = Some(*parent);
                    }
                } else if is_leaf && (has_id || has_class) && leaf_fallback.is_none() {
                    leaf_fallback = Some(*parent);
                }
            }
        }
        current = parent.parent();
        depth += 1;
    }
    container_fallback.or(leaf_fallback)
}

/// 用字符串操作在 HTML 中注入 class 和隐藏填充内容。
///
/// 对于短内容元素（< MIN_SCOREABLE_LEN 字符），还会注入隐藏的 `<p>` 标签
/// 让 Readability 能对该元素进行评分。Readability 的 `CharCounterCache`
/// 统计所有 DOM 文本（含 `display:none`），但最终输出时会忽略隐藏元素。
fn inject_classes(html: &str, targets: &[BoostTarget]) -> String {
    let boost_str = BOOST_CLASSES.join(" ");
    let hidden_filler: String = FILLER_CHAR.to_string().repeat(MIN_SCOREABLE_LEN);
    let hidden_p = format!("<p>{hidden_filler}</p>");
    let mut result = html.to_string();

    for target in targets {
        let mut offset = 0;

        while offset < result.len() {
            let Some(pos) = find_tag_open(&result[offset..], &target.tag) else {
                break;
            };
            let abs_pos = offset + pos;

            let tag_end = if let Some(end) = result[abs_pos..].find('>') {
                abs_pos + end
            } else {
                break;
            };

            let tag_content = &result[abs_pos..tag_end];

            if is_target_tag(tag_content, target) {
                // 注入 class
                if tag_content.contains("class=\"") {
                    let class_start = tag_content.find("class=\"").unwrap() + 7;
                    let insert_at = abs_pos + class_start;
                    result.insert_str(insert_at, &format!("{boost_str} "));
                } else {
                    result.insert_str(tag_end, &format!(" class=\"{boost_str}\""));
                }

                // 对短内容元素注入隐藏填充，让 Readability 能评分
                // 注意：class 注入会改变字符串长度，需要重新定位闭合 >
                let content_len = estimate_element_text_len(&result, abs_pos, &target.tag);
                if content_len < MIN_SCOREABLE_LEN {
                    let new_tag_end = abs_pos + result[abs_pos..].find('>').unwrap();
                    let after_open = new_tag_end + 1;
                    result.insert_str(after_open, &hidden_p);
                }

                break;
            }

            offset = tag_end + 1;
        }
    }

    result
}

/// 估算元素的直接文本长度（不含子元素的文本）。
///
/// 通过查找开标签和闭标签之间的纯文本（跳过子标签）来近似计算。
/// 不精确，但足以判断是否低于 MIN_SCOREABLE_LEN。
fn estimate_element_text_len(html: &str, open_tag_start: usize, tag: &str) -> usize {
    let open_end = html[open_tag_start..]
        .find('>')
        .map_or(html.len(), |p| open_tag_start + p + 1);

    // 查找闭标签
    let close_pattern = format!("</{tag}");
    let Some(close_rel) = html[open_end..].find(&close_pattern) else {
        return 0; // 自闭合或无闭标签
    };
    let close_pos = open_end + close_rel;

    // 统计开标签和闭标签之间的可见文本字符数（跳过标签和空白）
    let between = &html[open_end..close_pos];
    let mut count = 0;
    let mut in_tag = false;
    for ch in between.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag && !ch.is_whitespace() => count += 1,
            _ => {}
        }
    }
    count
}

/// 查找下一个 `<tag` 的位置，确保标签名完整匹配。
fn find_tag_open(html: &str, tag: &str) -> Option<usize> {
    let pattern = format!("<{tag}");
    let mut search_from = 0;
    while let Some(pos) = html[search_from..].find(&pattern) {
        let abs = search_from + pos;
        let after = abs + pattern.len();
        if after >= html.len() {
            return Some(abs);
        }
        let next = html.as_bytes()[after];
        if next == b' ' || next == b'>' || next == b'/' || next == b'\n' || next == b'\t' {
            return Some(abs);
        }
        search_from = after;
    }
    None
}

/// 检查标签内容是否匹配目标。
fn is_target_tag(tag_content: &str, target: &BoostTarget) -> bool {
    if !target.id.is_empty() && tag_content.contains(&format!("id=\"{}\"", target.id)) {
        return true;
    }
    if !target.class.is_empty() && tag_content.contains(&format!("class=\"{}", target.class)) {
        return true;
    }
    // 无 id/class 的元素，靠标签名匹配（第一个同名标签即命中）
    target.id.is_empty() && target.class.is_empty()
}
