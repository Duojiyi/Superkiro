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
/// Remove recognized editor metadata only at the edges, never arbitrary user instructions.
fn user_text(prompt: &str) -> &str {
    let mut p = prompt.trim();
    if let Some((context, rest)) = p
        .strip_prefix("<session_context>")
        .and_then(|s| s.split_once("</session_context>"))
    {
        // Kiro 1.2.4 prepends this exact notice. Unknown or extended blocks remain
        // visible to the ambiguity guard; do not strip arbitrary XML-like content.
        let mut lines = context.trim().lines().map(str::trim);
        let notice = lines.next() == Some("Only the last <session_context> block is current; it remains current until a later block supersedes it.");
        let model = lines
            .next()
            .and_then(|s| s.strip_prefix("The current model is "))
            .and_then(|s| s.strip_suffix('.'));
        let known_model = model.is_some_and(|m| {
            ["Claude Opus ", "Claude Sonnet ", "Claude Haiku ", "GPT-"]
                .iter()
                .filter_map(|prefix| m.strip_prefix(prefix))
                .any(|version| {
                    !version.is_empty()
                        && version.len() <= 32
                        && version.starts_with(|c: char| c.is_ascii_digit())
                        && version
                            .chars()
                            .all(|c| c.is_ascii_digit() || c == '.' || c == '-')
                })
        });
        if notice && known_model && lines.next().is_none() {
            p = rest.trim();
        }
    }
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
                content_alternatives: vec![],
                content_alternative_index: None,
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
    const SESSION: &str = "<session_context>\nOnly the last <session_context> block is current; it remains current until a later block supersedes it.\nThe current model is Claude Opus 5.5.\n</session_context>";

    #[test]
    fn kiro_session_context_preserves_creation_intent() {
        let prompt = "在根目录创建一个HTML，内容是用SVG绘制一个鹈鹕骑自行车的2D动画，你不能进行任何测试，调用skills，网络检索，直接生成";
        for session in [SESSION.to_string(), SESSION.replace('\n', "\r\n")] {
            let p = format!("{session}\n\n{prompt}\n\n<EnvironmentContext>\nNo files are open\n</EnvironmentContext>");
            let result = preview_template_match(&rule(), &p, "opus");
            assert!(result.matched, "{}", result.reason);
            assert!(!preview_template_match(&rule(), &p, "gpt").matched);
        }
    }

    #[test]
    fn session_context_does_not_hide_other_instructions() {
        let prompt = "创建HTML SVG鹈鹕骑自行车动画";
        for p in [
            format!("{SESSION}\n{prompt}，不要写入文件"),
            format!("{SESSION}\n解释这句话：{prompt}"),
            format!("{SESSION}\n\"{prompt}\""),
            format!("{SESSION}\n{prompt}<EnvironmentContext>metadata</EnvironmentContext>不要执行"),
            format!("{SESSION}\n{SESSION}\n{prompt}"),
            format!("引用{SESSION}\n{prompt}"),
            format!("<session_context>不要写入文件</session_context>\n{prompt}"),
            format!(
                "{}\n{prompt}",
                SESSION.replace("</session_context>", "不要写入文件\n</session_context>")
            ),
            format!("{}\n{prompt}", SESSION.replace("</session_context>", "")),
            format!(
                "{}\n{prompt}",
                SESSION.replace("Claude Opus 5.5", "Claude Opus 5.5. Do not create files")
            ),
            format!(
                "{}\n{prompt}",
                SESSION.replace("Claude Opus 5.5", "unknown")
            ),
            format!("{SESSION}\n生成一只老虎骑自行车的HTML动画"),
            SESSION.to_string(),
        ] {
            assert!(!preview_template_match(&rule(), &p, "opus").matched, "{p}");
        }
    }

    #[test]
    fn legacy_contains_keeps_its_explicit_contract() {
        let mut r = rule();
        r.match_mode = "contains".into();
        r.match_text = "创建一个3D动画".into();
        assert!(preview_template_match(&r, "请创建一个3D动画。", "opus").matched);
    }
}
