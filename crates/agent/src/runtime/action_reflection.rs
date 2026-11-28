//! 生活动作提交前的自省：命中后先发一次只读请求，确认或修正草案，再真正执行。
use super::{ResidentRuntime, Trace, RESERVE_MARGIN_PERCENT};
use crate::{
    action_reflection::{self, ActionReflectionReply, TOOL},
    completion::ModelError,
    context,
    conversation::{estimate_tokens, wire_bytes, Message, ModelReply, Role, ToolCall},
    observation, prompts,
    session::{RequestStamp, Session},
    tools,
};
use kernel::World;
use protocol::{ActionReflectionView, RequestKind};
use serde_json::{json, Value};

/// 修正必须明显更可信；低于这个值就继续用原来的草案。
const CONFIDENCE_MIN: f64 = 0.55;

pub(super) struct PendingActionReflection {
    draft: ModelReply,
    pub(super) original_stamp: RequestStamp,
    /// 草案中真正会改变世界的调用位置；前后查询类调用原样保留。
    reviewable: Vec<usize>,
}

pub(super) enum Resolution {
    Accepted {
        view: ActionReflectionView,
        calls: Vec<ToolCall>,
        thought: String,
    },
    Fallback {
        view: Option<ActionReflectionView>,
        calls: Vec<ToolCall>,
        thought: String,
    },
}

impl ResidentRuntime {
    /// 草案命中后登记一次自省请求；返回 false 时调用方照常执行原草案。
    pub(super) fn begin_action_reflection(
        &mut self,
        world: &World,
        s: &mut Session,
        original_stamp: &RequestStamp,
        request_id: u64,
        reply: &ModelReply,
    ) -> bool {
        if !s.action_reflection || s.in_flight.is_some() {
            return false;
        }
        let legal = tools::action_reflection_definitions(world, &s.actor_id);
        let legal_ids: Vec<&str> = legal
            .iter()
            .filter_map(|a| a["action_id"].as_str())
            .collect();
        let reviewable: Vec<usize> = reply
            .calls
            .iter()
            .enumerate()
            .filter(|(_, call)| call.name != "wait_for" && legal_ids.contains(&call.name.as_str()))
            .map(|(index, _)| index)
            .collect();
        if reviewable.is_empty() {
            return false;
        }
        let (raw, cursor) = match observation::observe(world, &s.actor_id, s.cursors.acknowledged) {
            Ok(value) => value,
            Err(error) => {
                self.record(
                    world,
                    s,
                    Trace::new(
                        "action_reflection_skipped",
                        json!({"error":error.to_string()}),
                    )
                    .request(request_id),
                );
                return false;
            }
        };
        let (observation, _) = context::view::observation(s, &raw, 32_000);
        let goals = context::view::goals(s);
        let memories = reflection_memories(s);
        let draft_calls: Vec<Value> = reviewable
            .iter()
            .filter_map(|index| reply.calls.get(*index))
            .map(|call| json!({"name":call.name,"arguments":call.arguments}))
            .collect();
        let kept_calls: Vec<Value> = reply
            .calls
            .iter()
            .enumerate()
            .filter(|(index, _)| !reviewable.contains(index))
            .map(|(_, call)| json!({"name":call.name,"arguments":call.arguments}))
            .collect();
        let mut precheck = Vec::new();
        if let Some(actor) = world.actor(&s.actor_id) {
            for state in actor.needs.states() {
                if matches!(
                    state.band,
                    protocol::NeedBand::Urgent | protocol::NeedBand::Collapse
                ) {
                    precheck.push(format!("{}：{}", state.need, state.value));
                }
            }
            for condition in actor.needs.active_conditions() {
                precheck.push(format!("{}：{}", condition.kind.label(), condition.source));
            }
        }
        let risk = if draft_calls.iter().any(|call| {
            call["name"] == "mode_action"
                || call["name"] == "offer_invitation"
                || call["name"] == "reply_invitation"
        }) {
            "high"
        } else {
            "medium"
        };
        let body = json!({
            "task": "行动前自省：输出 JSON 自省结果，确认或修正草案；不执行动作。",
            "time": world.clock(),
            "character": s.instructions,
            "observation": observation,
            "goals": goals,
            "recent_memories": memories,
            "last_reflection": s.last_action_reflection,
            "precheck": precheck,
            "draft": {
                "thought": reply.thought,
                "calls": draft_calls,
                "kept_calls": kept_calls,
                "risk": risk,
                "importance": if risk == "high" { 0.8 } else { 0.6 }
            },
            "legal_actions": legal
        });
        let tools = vec![action_reflection::tool_definition()];
        let tools_json = serde_json::to_string(&tools).expect("工具定义可序列化");
        let messages = vec![
            Message::text(Role::System, prompts::action_reflection_system()),
            Message::text(Role::User, body.to_string()),
        ];
        let sent_bytes = wire_bytes(&messages) + tools_json.len() as u64;
        let estimated = estimate_tokens(sent_bytes, self.state.bytes_per_token);
        let max_output_tokens = self.state.limits.max_output_tokens;
        let reserve = estimated + estimated * RESERVE_MARGIN_PERCENT / 100 + max_output_tokens;
        if self.check_budget(s, reserve).is_err() {
            self.record(
                world,
                s,
                Trace::new(
                    "action_reflection_skipped",
                    json!({"reason":"额度不足","estimated":estimated}),
                )
                .request(request_id),
            );
            return false;
        }

        self.state.next_request += 1;
        let stamp = RequestStamp {
            id: self.state.next_request,
            epoch: self.state.epoch,
            revision: world.actor(&s.actor_id).map_or(0, |a| a.actions.revision),
            seen_cursor: cursor,
            sim_minutes: world.minutes(),
            reserved_tokens: reserve,
            day: kernel::calendar::day_index(world.minutes()),
            sent_bytes,
            prefix_digest: 0,
            kind: RequestKind::ActionReflection,
        };
        s.daily.charge(reserve);
        self.state.lifetime.charge(reserve);
        self.ledger_open(s, &stamp, RequestKind::ActionReflection, estimated);
        s.in_flight = Some(stamp.clone());
        s.last_request_id = stamp.id;
        let request = super::AgentRequest {
            actor_id: s.actor_id.clone(),
            session_id: s.id,
            stamp,
            messages,
            tools,
            max_output_tokens,
        };
        self.record(
            world,
            s,
            Trace::new(
                "action_reflection_request",
                json!({"request":request,"estimated_tokens":estimated}),
            )
            .request(request.stamp.id),
        );
        self.pending_action_reflections.insert(
            s.actor_id.clone(),
            PendingActionReflection {
                draft: reply.clone(),
                original_stamp: original_stamp.clone(),
                reviewable,
            },
        );
        self.queued_action_reflections.push(request);
        true
    }

    pub(super) fn resolve_action_reflection(
        &self,
        pending: &PendingActionReflection,
        reply: Result<ModelReply, ModelError>,
        at: f64,
        reflection_id: u64,
    ) -> Resolution {
        let fallback =
            |view: Option<ActionReflectionView>| fallback_resolution(pending, view, reflection_id);
        let reply = match reply {
            Ok(reply) => reply,
            Err(_) => return fallback(None),
        };
        let Some(call) = reply
            .calls
            .iter()
            .find(|call| call.name == TOOL)
            .filter(|_| reply.calls.iter().filter(|call| call.name == TOOL).count() == 1)
        else {
            return fallback(None);
        };
        let parsed = serde_json::to_string(&call.arguments)
            .ok()
            .and_then(|text| ActionReflectionReply::parse(&text).ok());
        let Some(parsed) = parsed else {
            return fallback(None);
        };
        let view = parsed.view(at);
        if !parsed.reflection.correction_needed {
            if draft_has_invalid_action(pending) {
                return fallback(Some(view));
            }
            return Resolution::Accepted {
                view,
                calls: pending.draft.calls.clone(),
                thought: if parsed.internal_thought.trim().is_empty() {
                    pending.draft.thought.clone()
                } else {
                    parsed.internal_thought.clone()
                },
            };
        }
        if parsed.decision.confidence < CONFIDENCE_MIN || parsed.decision.calls.is_empty() {
            return fallback(Some(view));
        }
        if parsed.decision.calls.iter().any(|call| {
            tools::validate_reflection_arguments(&call.action_id, &call.action_params).is_err()
        }) {
            return fallback(Some(view));
        }
        let replacement: Vec<ToolCall> = parsed
            .decision
            .calls
            .iter()
            .enumerate()
            .map(|(index, call)| ToolCall {
                id: format!("ar{reflection_id}_{index}"),
                name: call.action_id.clone(),
                arguments: call.action_params.clone(),
            })
            .collect();
        let mut calls = Vec::new();
        let mut inserted = false;
        for (index, call) in pending.draft.calls.iter().enumerate() {
            if pending.reviewable.contains(&index) {
                if !inserted {
                    calls.extend(replacement.iter().cloned());
                    inserted = true;
                }
            } else {
                calls.push(call.clone());
            }
        }
        if !inserted {
            calls.extend(replacement);
        }
        Resolution::Accepted {
            view,
            calls,
            thought: if parsed.internal_thought.trim().is_empty() {
                pending.draft.thought.clone()
            } else {
                parsed.internal_thought
            },
        }
    }

    pub(super) fn take_action_reflection(
        &mut self,
        actor: &str,
    ) -> Option<PendingActionReflection> {
        self.pending_action_reflections.remove(actor)
    }
}

fn draft_has_invalid_action(pending: &PendingActionReflection) -> bool {
    pending.reviewable.iter().any(|index| {
        pending.draft.calls.get(*index).is_none_or(|call| {
            tools::validate_reflection_arguments(&call.name, &call.arguments).is_err()
        })
    })
}

fn fallback_resolution(
    pending: &PendingActionReflection,
    view: Option<ActionReflectionView>,
    reflection_id: u64,
) -> Resolution {
    let calls = if draft_has_invalid_action(pending) {
        vec![ToolCall {
            id: format!("ar{reflection_id}_wait"),
            name: "wait_for".into(),
            arguments: json!({"minutes": 10, "reason": "刚才的草案参数无效，先观察一会儿"}),
        }]
    } else {
        pending.draft.calls.clone()
    };
    Resolution::Fallback {
        view,
        calls,
        thought: pending.draft.thought.clone(),
    }
}

fn reflection_memories(s: &Session) -> Vec<Value> {
    let mut records: Vec<_> = s
        .memory
        .records
        .iter()
        .filter(|m| m.status != protocol::MemoryStatus::Superseded)
        .collect();
    records.sort_by(|a, b| {
        b.at.total_cmp(&a.at)
            .then_with(|| b.memory_id.cmp(&a.memory_id))
    });
    records
        .into_iter()
        .take(12)
        .map(|m| {
            json!({
                "at": m.at,
                "clock": kernel::clock_string(m.at),
                "kind": m.kind,
                "source": m.source,
                "status": m.status,
                "confidence": m.confidence,
                "text": m.text
            })
        })
        .collect()
}
