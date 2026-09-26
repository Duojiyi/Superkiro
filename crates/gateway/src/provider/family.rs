//! What an upstream model accepts, by family. One table decides, from the target model's
//! name, every sampling and reasoning field a request carries, for both request formats.
//!
//! Claude Opus 4.7 and later, Sonnet 5 and Fable refuse `temperature`, `top_p` and `top_k`
//! and a thinking budget with a 400; they reason with `thinking: {type: "adaptive"}` and
//! `output_config.effort`, and stream an empty thinking text unless asked for a summary.
//! Opus 4.6 and Sonnet 4.6 take adaptive thinking and sampling but have no `xhigh` effort.
//! Older Claude models think within `budget_tokens`, which must leave room for the answer.
//! OpenAI's reasoning models take `max_completion_tokens` and no temperature. A model the
//! table does not know is sent no sampling parameters.

use kiro_wire::requests::conversation::ReasoningEffort;

/// How a model is asked to reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reasoning {
    /// Claude 4.6 and later.
    Adaptive {
        /// Has the `xhigh` effort level.
        xhigh: bool,
        /// Streams a readable summary when asked (`display: "summarized"`); its default is
        /// an empty thinking text.
        summarized: bool,
        /// Thinks without being asked to.
        by_default: bool,
    },
    /// Claude before 4.6: a thinking budget below the output limit.
    Budget,
    /// OpenAI's reasoning models: `reasoning_effort`.
    OpenAi,
    /// Unknown: each format's own form when effort is asked for, nothing otherwise.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelFamily {
    /// Accepts `temperature`, `top_p` and `top_k`.
    pub sampling: bool,
    pub reasoning: Reasoning,
}

impl ModelFamily {
    /// Whether the model reasons before it answers, unasked or because `effort` asks: its
    /// first output can take minutes.
    pub fn reasons(&self, effort: Option<ReasoningEffort>) -> bool {
        effort.is_some()
            || matches!(
                self.reasoning,
                Reasoning::OpenAi
                    | Reasoning::Adaptive {
                        by_default: true,
                        ..
                    }
            )
    }

    /// OpenAI's reasoning models take `max_completion_tokens`, not `max_tokens`.
    pub fn max_completion_tokens(&self) -> bool {
        self.reasoning == Reasoning::OpenAi
    }

    /// The `output_config.effort` an adaptive model is sent: a level it lacks becomes the
    /// next one down, never up.
    pub fn adaptive_effort(xhigh: bool, effort: ReasoningEffort) -> &'static str {
        match effort {
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
            ReasoningEffort::Xhigh if xhigh => "xhigh",
            ReasoningEffort::Xhigh => "high",
            ReasoningEffort::Max => "max",
        }
    }
}

/// The family of the model a request is sent to, from its name as an upstream knows it:
/// `claude-opus-4-7`, `claude-sonnet-4.6`, `anthropic/claude-opus-5-5`, `o3-mini`, ...
pub fn family(model: &str) -> ModelFamily {
    let name = model.to_ascii_lowercase().replace(['.', '_'], "-");
    let name = name.rsplit('/').next().unwrap_or(&name);
    if let Some(family) = claude_family(name) {
        return family;
    }
    let base = name.strip_prefix("openai-").unwrap_or(name);
    let reasoning_model = ["o1", "o3", "o4", "gpt-5", "gpt-6"].iter().any(|prefix| {
        base.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
    });
    if reasoning_model {
        return ModelFamily {
            sampling: false,
            reasoning: Reasoning::OpenAi,
        };
    }
    let sampled = ["gpt-4o", "gpt-4-1", "gpt-4-turbo", "gpt-4", "gpt-3-5"]
        .iter()
        .any(|prefix| base.starts_with(prefix));
    ModelFamily {
        sampling: sampled,
        reasoning: Reasoning::Unknown,
    }
}

fn claude_family(name: &str) -> Option<ModelFamily> {
    let (word, at) = ["opus", "sonnet", "haiku", "fable", "mythos"]
        .iter()
        .find_map(|word| name.find(word).map(|at| (*word, at)))?;
    if matches!(word, "fable" | "mythos") {
        return Some(adaptive(true, true));
    }
    // "claude-opus-4-7-20260101" names its version after the model; "claude-3-5-sonnet"
    // before it. A date is not a version.
    let after = version(&name[at + word.len()..]);
    let before = version(&name[..at]);
    let (major, minor) = after.or(before)?;
    Some(match (major, minor) {
        (5.., _) => adaptive(true, true),
        (4, 7..) => adaptive(true, false),
        (4, 6) => ModelFamily {
            sampling: true,
            reasoning: Reasoning::Adaptive {
                xhigh: false,
                summarized: false,
                by_default: false,
            },
        },
        _ => ModelFamily {
            sampling: true,
            reasoning: Reasoning::Budget,
        },
    })
}

fn adaptive(xhigh: bool, by_default: bool) -> ModelFamily {
    ModelFamily {
        sampling: false,
        reasoning: Reasoning::Adaptive {
            xhigh,
            summarized: true,
            by_default,
        },
    }
}

/// The first number in `text` and the one after it, when short: "-4-7-2026..." is 4.7,
/// "claude-3-5-" is 3.5, "-5" is 5.0, and "-20250219" (a date) is none.
fn version(text: &str) -> Option<(u32, u32)> {
    let short = |part: &str| part.parse::<u32>().ok().filter(|_| part.len() <= 2);
    let mut parts = text
        .split('-')
        .filter(|part| !part.is_empty())
        .skip_while(|part| part.parse::<u32>().is_err());
    let major = short(parts.next()?)?;
    Some((major, parts.next().and_then(short).unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adaptive_family(xhigh: bool, by_default: bool) -> ModelFamily {
        adaptive(xhigh, by_default)
    }

    #[test]
    fn current_claude_models_take_adaptive_thinking_and_no_sampling() {
        for model in [
            "claude-opus-5-5",
            "claude-opus-5",
            "claude-sonnet-5",
            "anthropic/claude-opus-5.5",
            "us.anthropic.claude-sonnet-5-20260601-v1:0",
        ] {
            assert_eq!(family(model), adaptive_family(true, true), "{model}");
        }
        for model in ["claude-fable-5", "claude-fable-5-1", "claude-mythos-5-1"] {
            assert_eq!(family(model), adaptive_family(true, true), "{model}");
        }
        for model in [
            "claude-opus-4-7",
            "claude-opus-4.8",
            "claude-opus-4-8-20260101",
        ] {
            assert_eq!(family(model), adaptive_family(true, false), "{model}");
        }
    }

    #[test]
    fn claude_4_6_takes_sampling_and_adaptive_thinking_without_xhigh() {
        for model in [
            "claude-sonnet-4-6",
            "claude-opus-4.6",
            "claude-opus-4-6-20260205",
        ] {
            let family = family(model);
            assert!(family.sampling, "{model}");
            assert_eq!(
                family.reasoning,
                Reasoning::Adaptive {
                    xhigh: false,
                    summarized: false,
                    by_default: false
                },
                "{model}"
            );
        }
        assert_eq!(
            ModelFamily::adaptive_effort(false, ReasoningEffort::Xhigh),
            "high"
        );
        assert_eq!(
            ModelFamily::adaptive_effort(false, ReasoningEffort::Max),
            "max"
        );
        assert_eq!(
            ModelFamily::adaptive_effort(true, ReasoningEffort::Xhigh),
            "xhigh"
        );
    }

    #[test]
    fn older_claude_models_think_within_a_budget() {
        for model in [
            "claude-haiku-4-5",
            "claude-haiku-4-5-20251001",
            "claude-sonnet-4-5",
            "claude-opus-4-1",
            "claude-3-7-sonnet-20250219",
            "claude-3-5-sonnet-20241022",
        ] {
            assert_eq!(
                family(model),
                ModelFamily {
                    sampling: true,
                    reasoning: Reasoning::Budget
                },
                "{model}"
            );
        }
    }

    #[test]
    fn openai_reasoning_models_and_unknown_models_are_told_apart() {
        for model in [
            "o3",
            "o3-mini",
            "o4-mini",
            "gpt-5",
            "gpt-5.6",
            "gpt-6-astra",
        ] {
            let family = family(model);
            assert!(
                !family.sampling && family.max_completion_tokens(),
                "{model}"
            );
            assert!(family.reasons(None), "{model}");
        }
        for model in ["gpt-4o", "gpt-4o-mini", "gpt-4.1"] {
            assert!(family(model).sampling, "{model}");
            assert!(!family(model).max_completion_tokens(), "{model}");
        }
        for model in ["deepseek-chat", "kimi-k2", "glm-4.6", "qwen3-coder", "o1ne"] {
            assert_eq!(
                family(model),
                ModelFamily {
                    sampling: false,
                    reasoning: Reasoning::Unknown
                },
                "{model}"
            );
        }
    }

    #[test]
    fn a_model_reasons_unasked_only_when_its_family_does() {
        assert!(family("claude-opus-5-5").reasons(None));
        assert!(!family("claude-opus-4-7").reasons(None));
        assert!(family("claude-opus-4-7").reasons(Some(ReasoningEffort::Low)));
        assert!(!family("deepseek-chat").reasons(None));
    }
}
