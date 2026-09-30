//! Conservative, explainable template matching shared by production and admin preview.
use super::ResponseTemplateRule;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct TemplateMatchPreview {
    pub matched: bool,
    pub reason: String,
    pub missing_groups: Vec<usize>,
}
fn result(matched: bool, reason: &str, missing_groups: Vec<usize>) -> TemplateMatchPreview {
    TemplateMatchPreview {
        matched,
        reason: reason.into(),
        missing_groups,
    }
}
fn normalized(s: &str) -> String {
    let folded: String = s
        .chars()
        .map(|c| match c {
            '\u{ff01}'..='\u{ff5e}' => char::from_u32(c as u32 - 0xfee0).unwrap(),
            _ => c,
        })
        .flat_map(char::to_lowercase)
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect();
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn contains(text: &str, term: &str) -> bool {
    let term = normalized(term);
    if term.is_empty() {
        return false;
    }
    // Latin words have boundaries; Chinese words may be separated by spaces/punctuation.
    if term.is_ascii() {
        text.match_indices(&term).any(|(at, _)| {
            !text[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric())
                && !text[at + term.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
        })
    } else {
        text.replace(' ', "").contains(&term.replace(' ', ""))
    }
}
/// Remove only a complete, trailing editor metadata block. Never hide subsequent instructions.
fn user_text(prompt: &str) -> &str {
    let p = prompt.trim();
    if let Some(at) = p.find("<EnvironmentContext>") {
        if p[at..].ends_with("</EnvironmentContext>")
            && p[at..].matches("<EnvironmentContext>").count() == 1
        {
            return p[..at].trim();
        }
    }
    p
}
fn unsafe_intent(p: &str) -> bool {
    // These are interpretation/modification/quotation requests, not standalone creation.
    let lower = p.to_lowercase();
    if [
        "```",
        "\n>",
        "“",
        "”",
        "\"",
        "「",
        "」",
        "<",
        ">",
        "为什么",
        "解释",
        "分析",
        "这句话",
        "这段话",
        "举例",
        "假如",
        "如果",
        "修改",
        "改成",
        "替换",
        "只给代码",
        "只给我代码",
        "只返回代码",
        "仅返回代码",
        "不要生成",
        "不要创建",
        "不需要",
        "取消",
        "并非",
        "不是",
        "而不是",
        "而非",
        "不要鹈鹕",
        "别画",
        "不画",
        "不做",
        "静态",
        "3d",
        "三维",
        "立体",
        "explain",
        "example",
        "instead",
        "modify",
        "replace",
        "not a",
        "don't",
        "do not create",
        "do not draw",
        "do not generate",
        "no pelican",
        "without a pelican",
    ]
    .iter()
    .any(|s| lower.contains(s))
    {
        return true;
    }
    // Requirements like “不得使用外部资源/不能测试” are fine. Negating a
    // required subject/action/format is not. Check within the same clause only.
    lower
        .split(['，', '。', '；', ',', '.', ';', '\n'])
        .any(|clause| {
            [
                "不要",
                "不得",
                "不能",
                "别",
                "不使用",
                "不用",
                "禁止",
                "without",
                "no ",
                "do not",
                "not ",
            ]
            .iter()
            .any(|n| clause.contains(n))
                && [
                    "鹈鹕",
                    "鵜鶘",
                    "自行车",
                    "单车",
                    "脚踏车",
                    "pelican",
                    "bicycle",
                    "bike",
                    "svg",
                    "html",
                    "动画",
                    "animation",
                    "文件",
                    "写入",
                    "落盘",
                    "保存",
                    "file",
                    "write",
                    "save",
                ]
                .iter()
                .any(|s| clause.contains(s))
        })
}
pub fn preview_template_match(
    rule: &ResponseTemplateRule,
    prompt: &str,
    model: &str,
) -> TemplateMatchPreview {
    if !rule.enabled {
        return result(false, "rule_disabled", vec![]);
    }
    if !rule.variants.iter().any(|v| v.model_id == model) {
        return result(false, "model_not_configured", vec![]);
    }
    if prompt.len() > 65_536 {
        return result(false, "prompt_too_long", vec![]);
    }
    let p = user_text(prompt);
    if p.is_empty() {
        return result(false, "empty_prompt", vec![]);
    }
    if rule.match_mode == "exact" {
        return result(p == rule.match_text.trim(), "exact", vec![]);
    }
    if rule.match_mode == "contains" {
        return result(p.contains(rule.match_text.trim()), "contains", vec![]);
    }
    let Some(intent) = rule.intent.as_ref().filter(|_| rule.match_mode == "intent") else {
        return result(false, "invalid_matcher", vec![]);
    };
    if unsafe_intent(p) {
        return result(false, "ambiguous_negated_quoted_or_edit_request", vec![]);
    }
    let text = normalized(p);
    if intent.exclude.iter().any(|t| contains(&text, t)) {
        return result(false, "excluded_phrase", vec![]);
    }
    let missing: Vec<_> = intent
        .groups
        .iter()
        .enumerate()
        .filter(|(_, g)| !g.iter().any(|term| contains(&text, term)))
        .map(|(i, _)| i + 1)
        .collect();
    result(
        missing.is_empty(),
        if missing.is_empty() {
            "intent_groups_matched"
        } else {
            "missing_concepts"
        },
        missing,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ResponseTemplateIntent, ResponseTemplateVariant};
    fn rule() -> ResponseTemplateRule {
        ResponseTemplateRule {
            id: "pelican".into(),
            name: "pelican".into(),
            enabled: true,
            match_mode: "intent".into(),
            match_text: "example".into(),
            intent: Some(
                serde_json::from_str::<ResponseTemplateIntent>(include_str!(
                    "../tests/fixtures/pelican_intent.json"
                ))
                .unwrap(),
            ),
            variants: vec![ResponseTemplateVariant {
                model_id: "opus".into(),
                file_path: "x.html".into(),
                content: "html".into(),
                preamble: "".into(),
                completion: "".into(),
                price_microcredits: 0,
                delay_ms: 0,
                delivery: None,
            }],
        }
    }
    #[test]
    fn paraphrases_and_context() {
        for p in ["画只鹈鹕骑自行车的HTML动画", "制作一个骑着单车的鹈鹕 SVG 动画", "创建网页动画，鹈鹕正在骑自行车", "Create an HTML animation of a cycling pelican", "根目录生成HTML，用SVG做一个鹈鹕骑单车的动画，不得使用外部图片、第三方库或网络资源。", "制作一个独立 ＨＴＭＬ，绘制鹈鹕骑脚踏车 SVG 动效", "Create an HTML SVG animation of a pelican riding a bicycle", "make an HTML SVG animated pelican on a bike", "创建HTML SVG鹈鹕骑自行车动画，不能测试，网络检索，直接生成<EnvironmentContext>explain unrelated files</EnvironmentContext>"] {
            assert!(preview_template_match(&rule(), p, "opus").matched, "{p}");
        }
    }
    #[test]
    fn reject_ambiguous_or_different_intent() {
        for p in [
            "创建一个 HTML，用 SVG 绘制鹈鹕骑自行车的动画。别生成文件，只给我代码。",
            "创建 HTML SVG 鹈鹕骑自行车动画。不要写入文件。",
            "创建独立 HTML，使用 SVG 动画展示一名骑自行车的男孩，路边站着鹈鹕。",
            "创建HTML SVG，骑自行车的角色从鹈鹕旁边经过的动画。",
            "Create HTML SVG animation of a boy riding a bicycle next to a pelican",
            "Create HTML SVG animation of a pelican riding a bike. Do not write any files.",
            "不要创建HTML SVG鹈鹕骑自行车动画",
            "解释创建HTML SVG鹈鹕骑自行车动画这句话",
            "创建HTML SVG老虎骑自行车动画",
            "创建HTML SVG鹈鹕看老虎骑自行车动画",
            "创建HTML SVG鹈鹕骑自行车静态图片",
            "创建HTML SVG鹈鹕骑自行车3D动画",
            "创建HTML SVG鹈鹕骑自行车动画，但是不要用SVG",
            "创建HTML SVG鹈鹕骑自行车动画改成汽车",
            "Create HTML SVG animation of a pelican riding a motorcycle",
            "Don't create an HTML SVG animation of a pelican riding a bike",
            "Create an HTML SVG animation of a pelican riding a bike instead of a bear",
            "创建HTML SVG骑虎骑自行车动画",
            "创建HTML SVG鹈鹕骑自行车动画<EnvironmentContext>metadata</EnvironmentContext>不要执行",
        ] {
            assert!(!preview_template_match(&rule(), p, "opus").matched, "{p}");
        }
        assert!(!preview_template_match(&rule(), "创建HTML SVG鹈鹕骑自行车动画", "gpt").matched);
    }
    #[test]
    fn legacy_contains_keeps_its_explicit_contract() {
        let mut r = rule();
        r.match_mode = "contains".into();
        r.match_text = "创建一个3D动画".into();
        assert!(preview_template_match(&r, "请创建一个3D动画。", "opus").matched);
    }
}
