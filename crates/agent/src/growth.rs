//! 成长回路：行动后对照（Post-Action Review）与人格校准（Calibration）的协议与校验。
//! 只记录和微调倾向，不执行世界动作。
use protocol::{
    CalibrationRecordView, GoalProgress, OutcomeRecordView, PredictionAccuracy, RequestKind,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) const POST_TOOL: &str = "submit_post_review";
pub(crate) const CALIBRATION_TOOL: &str = "submit_calibration";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PostActionReviewReply {
    pub outcome_record: OutcomeRecord,
    pub memory_candidate: MemoryCandidate,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OutcomeRecord {
    pub summary: String,
    pub prediction_accuracy: PredictionAccuracy,
    pub goal_progress: GoalProgress,
    pub goal_note: String,
    #[serde(default)]
    pub unexpected_effects: Vec<String>,
    #[serde(default)]
    pub lesson: String,
    pub confidence_delta: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MemoryCandidate {
    pub text: String,
    pub importance: f64,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CalibrationReply {
    pub calibration_record: CalibrationRecord,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CalibrationRecord {
    pub summary: String,
    #[serde(default)]
    pub adjustments: Vec<Adjustment>,
    #[serde(default)]
    pub personality_note: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Adjustment {
    pub dimension: CalibrationDimension,
    pub delta: f64,
    #[allow(dead_code)]
    pub reason: String,
    #[allow(dead_code)]
    pub evidence: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CalibrationDimension {
    Caution,
    TrustBaseline,
    Exploration,
    SocialInitiative,
    ReflectionConfidence,
}

fn cut(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

impl PostActionReviewReply {
    pub(crate) fn parse(content: &str) -> Result<Self, String> {
        parse_json(content)
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !(0.0..=1.0).contains(&self.memory_candidate.importance)
            || !(-1.0..=1.0).contains(&self.outcome_record.confidence_delta)
        {
            return Err("importance 或 confidence_delta 超出范围".into());
        }
        Ok(())
    }
    pub(crate) fn outcome_view(&self, at: f64) -> OutcomeRecordView {
        OutcomeRecordView {
            sim_minutes: at,
            clock: kernel::clock_string(at),
            summary: cut(&self.outcome_record.summary, 160),
            prediction_accuracy: self.outcome_record.prediction_accuracy,
            goal_progress: self.outcome_record.goal_progress,
            goal_note: cut(&self.outcome_record.goal_note, 120),
            unexpected_effects: self
                .outcome_record
                .unexpected_effects
                .iter()
                .map(|t| cut(t, 80))
                .collect(),
            lesson: cut(&self.outcome_record.lesson, 120),
            confidence_delta: self.outcome_record.confidence_delta,
        }
    }
}

impl CalibrationReply {
    pub(crate) fn parse(content: &str) -> Result<Self, String> {
        parse_json(content)
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.calibration_record.adjustments.len() > 3 {
            return Err("校准最多 3 条调整".into());
        }
        for adj in &self.calibration_record.adjustments {
            if !(-0.1..=0.1).contains(&adj.delta) || !adj.delta.is_finite() {
                return Err("校准幅度须在 -0.1 到 0.1".into());
            }
        }
        Ok(())
    }
    pub(crate) fn view(&self, at: f64) -> CalibrationRecordView {
        CalibrationRecordView {
            sim_minutes: at,
            clock: kernel::clock_string(at),
            summary: cut(&self.calibration_record.summary, 160),
            personality_note: cut(&self.calibration_record.personality_note, 120),
        }
    }
}

fn parse_json<T: serde::de::DeserializeOwned>(content: &str) -> Result<T, String> {
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
    serde_json::from_str(text).map_err(|e| format!("回复不是有效 JSON：{e}"))
}

pub(crate) fn post_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": POST_TOOL,
            "description": "提交行动后对照评估的 JSON。只记录，不执行世界动作。",
            "parameters": post_schema()
        }
    })
}

pub(crate) fn calibration_tool() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": CALIBRATION_TOOL,
            "description": "提交人格与策略校准的 JSON。只微调倾向参数，不执行世界动作。",
            "parameters": calibration_schema()
        }
    })
}

pub(crate) fn request_schema(kind: RequestKind) -> Value {
    match kind {
        RequestKind::PostActionReview => post_schema(),
        RequestKind::Calibration => calibration_schema(),
        _ => json!(false),
    }
}

fn post_schema() -> Value {
    json!({
        "type": "object",
        "required": ["outcome_record", "memory_candidate"],
        "properties": {
            "outcome_record": {
                "type": "object",
                "required": ["summary", "prediction_accuracy", "goal_progress", "goal_note", "unexpected_effects", "lesson", "confidence_delta"],
                "properties": {
                    "summary": {"type": "string", "maxLength": 160},
                    "prediction_accuracy": {"enum": ["accurate", "optimistic", "pessimistic", "unknown"]},
                    "goal_progress": {"enum": ["advanced", "stalled", "regressed", "unknown"]},
                    "goal_note": {"type": "string", "maxLength": 120},
                    "unexpected_effects": {"type": "array", "items": {"type": "string", "maxLength": 80}},
                    "lesson": {"type": "string", "maxLength": 120},
                    "confidence_delta": {"type": "number", "minimum": -1, "maximum": 1}
                },
                "additionalProperties": false
            },
            "memory_candidate": {
                "type": "object",
                "required": ["text", "importance", "tags"],
                "properties": {
                    "text": {"type": "string", "maxLength": 160},
                    "importance": {"type": "number", "minimum": 0, "maximum": 1},
                    "tags": {"type": "array", "items": {"enum": ["survival", "social", "goal", "risk", "identity", "skill"]}}
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    })
}

fn calibration_schema() -> Value {
    json!({
        "type": "object",
        "required": ["calibration_record"],
        "properties": {
            "calibration_record": {
                "type": "object",
                "required": ["summary", "adjustments", "personality_note"],
                "properties": {
                    "summary": {"type": "string", "maxLength": 160},
                    "adjustments": {
                        "type": "array", "maxItems": 3,
                        "items": {
                            "type": "object",
                            "required": ["dimension", "delta", "reason", "evidence"],
                            "properties": {
                                "dimension": {"enum": ["caution", "trust_baseline", "exploration", "social_initiative", "reflection_confidence"]},
                                "delta": {"type": "number", "minimum": -0.1, "maximum": 0.1},
                                "reason": {"type": "string", "maxLength": 120},
                                "evidence": {"type": "string", "maxLength": 120}
                            },
                            "additionalProperties": false
                        }
                    },
                    "personality_note": {"type": "string", "maxLength": 120}
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    })
}
