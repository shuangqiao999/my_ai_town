//! 行动前自省的协议与校验。模型只提交一个 JSON 对象；确认或修正由运行器决定，
//! 这里不执行世界动作。
use protocol::{ActionReflectionView, ReflectionGoalAlignment, ReflectionRisk};
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) const TOOL: &str = "submit_action_reflection";
const CALLS_MAX: usize = 8;
const THOUGHT_CHARS: usize = 160;
const QUESTION_CHARS: usize = 80;
const OUTCOME_CHARS: usize = 120;
const RISK_NOTE_CHARS: usize = 80;
const ALTERNATIVE_CHARS: usize = 120;
const REASON_CHARS: usize = 120;
const MEMORY_NOTE_CHARS: usize = 160;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActionReflectionReply {
    pub internal_thought: String,
    pub reflection: ReflectionPart,
    pub decision: DecisionPart,
    #[serde(default)]
    pub memory_note: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReflectionPart {
    pub self_question: String,
    pub predicted_outcome: String,
    pub goal_alignment: ReflectionGoalAlignment,
    pub risk: ReflectionRisk,
    pub risk_note: String,
    pub alternative_considered: String,
    pub correction_needed: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DecisionPart {
    pub calls: Vec<DecisionCall>,
    pub confidence: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DecisionCall {
    pub action_id: String,
    pub action_params: Value,
}

impl ActionReflectionReply {
    pub(crate) fn parse(content: &str) -> Result<Self, String> {
        let text = content.trim();
        let text = text
            .strip_prefix("```json")
            .or_else(|| text.strip_prefix("```"))
            .and_then(|s| s.strip_suffix("```"))
            .unwrap_or(text)
            .trim();
        let text = match (text.find('{'), text.rfind('}')) {
            (Some(start), Some(end)) if start <= end => &text[start..=end],
            _ => text,
        };
        let reply: Self =
            serde_json::from_str(text).map_err(|e| format!("自省回复不是有效 JSON：{e}"))?;
        reply.validate()?;
        Ok(reply)
    }

    fn validate(&self) -> Result<(), String> {
        if self.decision.calls.len() > CALLS_MAX {
            return Err(format!("自省回复最多 {CALLS_MAX} 个动作"));
        }
        if !self.decision.confidence.is_finite() || !(0.0..=1.0).contains(&self.decision.confidence)
        {
            return Err("confidence 须在 0–1".into());
        }
        if self
            .decision
            .calls
            .iter()
            .any(|call| call.action_id.trim().is_empty() || !call.action_params.is_object())
        {
            return Err("自省动作须有 action_id 和对象参数".into());
        }
        Ok(())
    }

    pub(crate) fn view(&self, at: f64) -> ActionReflectionView {
        ActionReflectionView {
            sim_minutes: at,
            clock: kernel::clock_string(at),
            internal_thought: cut(&self.internal_thought, THOUGHT_CHARS),
            self_question: cut(&self.reflection.self_question, QUESTION_CHARS),
            predicted_outcome: cut(&self.reflection.predicted_outcome, OUTCOME_CHARS),
            goal_alignment: self.reflection.goal_alignment,
            risk: self.reflection.risk,
            risk_note: cut(&self.reflection.risk_note, RISK_NOTE_CHARS),
            alternative_considered: cut(&self.reflection.alternative_considered, ALTERNATIVE_CHARS),
            correction_needed: self.reflection.correction_needed,
            confidence: self.decision.confidence,
            reason: cut(&self.decision.reason, REASON_CHARS),
            memory_note: cut(&self.memory_note, MEMORY_NOTE_CHARS),
        }
    }
}

fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

pub(crate) fn tool_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": TOOL,
            "description": "提交行动前自省的 JSON。只会记录和校验，不执行世界动作。",
            "parameters": result_schema()
        }
    })
}

pub(crate) fn result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "internal_thought": {"type": "string", "maxLength": THOUGHT_CHARS},
            "reflection": {
                "type": "object",
                "properties": {
                    "self_question": {"type": "string", "maxLength": QUESTION_CHARS},
                    "predicted_outcome": {"type": "string", "maxLength": OUTCOME_CHARS},
                    "goal_alignment": {"type": "string", "enum": ["high", "medium", "low"]},
                    "risk": {"type": "string", "enum": ["none", "low", "medium", "high"]},
                    "risk_note": {"type": "string", "maxLength": RISK_NOTE_CHARS},
                    "alternative_considered": {"type": "string", "maxLength": ALTERNATIVE_CHARS},
                    "correction_needed": {"type": "boolean"}
                },
                "required": ["self_question", "predicted_outcome", "goal_alignment", "risk", "risk_note", "alternative_considered", "correction_needed"],
                "additionalProperties": false
            },
            "decision": {
                "type": "object",
                "properties": {
                    "calls": {
                        "type": "array",
                        "maxItems": CALLS_MAX,
                        "items": {
                            "type": "object",
                            "properties": {
                                "action_id": {"type": "string"},
                                "action_params": {"type": "object"}
                            },
                            "required": ["action_id", "action_params"],
                            "additionalProperties": false
                        }
                    },
                    "confidence": {"type": "number", "minimum": 0, "maximum": 1},
                    "reason": {"type": "string", "maxLength": REASON_CHARS}
                },
                "required": ["calls", "confidence", "reason"],
                "additionalProperties": false
            },
            "memory_note": {"type": "string", "maxLength": MEMORY_NOTE_CHARS}
        },
        "required": ["internal_thought", "reflection", "decision"],
        "additionalProperties": false
    })
}
