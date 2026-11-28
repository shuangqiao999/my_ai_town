//! 成长回路：行动后评估（Post）与人格校准（Calibration）的请求组装。只读世界，不执行动作。
use super::{ResidentRuntime, Trace, RESERVE_MARGIN_PERCENT};
use crate::{
    context,
    conversation::{estimate_tokens, wire_bytes, Message, Role, ToolCall},
    growth, observation, prompts,
    session::Session,
};
use kernel::World;
use protocol::{ActionReflectionView, RequestKind};
use serde_json::{json, Value};

/// 等待动作/互动终态后做事后评估；只在内存里。
#[derive(Clone)]
pub(super) struct PendingPostReview {
    pub pre_view: ActionReflectionView,
    pub executed_calls: Vec<ToolCall>,
    pub revision: u64,
    pub seen_cursor: u64,
    pub sim_minutes: f64,
    /// 交谈发言本身会改变动作版本；回顾只检查通知与人身状态，不把它当成外部打断。
    pub chat: bool,
}

impl ResidentRuntime {
    /// 动作/互动进入终态后，发起一次事后评估；额度不足或没有成长能力时跳过。
    pub(super) fn begin_post_review(
        &mut self,
        world: &World,
        s: &mut Session,
        pending: &PendingPostReview,
        result: Value,
    ) {
        if !s.action_reflection || s.in_flight.is_some() {
            return;
        }
        let (raw, _) = match observation::observe(world, &s.actor_id, s.cursors.acknowledged) {
            Ok(value) => value,
            Err(_) => return,
        };
        let (observation, _) = context::view::observation(s, &raw, 16_000);
        let body = json!({
            "task": "行动后对照：输出 JSON 评估结果，不执行新动作。",
            "time": world.clock(),
            "character": s.instructions,
            "pre_reflection": {
                "internal_thought": pending.pre_view.internal_thought,
                "self_question": pending.pre_view.self_question,
                "predicted_outcome": pending.pre_view.predicted_outcome,
                "goal_alignment": pending.pre_view.goal_alignment,
                "risk": pending.pre_view.risk,
                "confidence": pending.pre_view.confidence,
                "reason": pending.pre_view.reason
            },
            "executed_calls": pending
                .executed_calls
                .iter()
                .map(|c| json!({"name": c.name, "arguments": c.arguments}))
                .collect::<Vec<_>>(),
            "execution_result": result,
            "observation_after": observation,
            "goals": context::view::goals(s)
        });
        self.queue_growth_request(
            world,
            s,
            RequestKind::PostActionReview,
            prompts::post_review_system(),
            body,
            vec![growth::post_tool()],
        );
    }

    /// 周期整理成功后发起一次人格校准；额度不足或没有成长能力时跳过。
    pub(super) fn begin_calibration(&mut self, world: &World, s: &mut Session) {
        if !s.action_reflection || s.in_flight.is_some() {
            return;
        }
        let body = json!({
            "task": "人格校准：输出 JSON 校准结果，不执行动作。",
            "time": world.clock(),
            "character": s.instructions,
            "personality": s.personality,
            "self_model": s.self_model,
            "outcome_records": s.outcome_records,
            "latest_consolidation": s.consolidation_records.last(),
            "pending_goal_adjustment": s.pending_goal_adjustment
        });
        self.queue_growth_request(
            world,
            s,
            RequestKind::Calibration,
            prompts::calibration_system(),
            body,
            vec![growth::calibration_tool()],
        );
    }

    fn queue_growth_request(
        &mut self,
        world: &World,
        s: &mut Session,
        kind: RequestKind,
        system: String,
        body: Value,
        tools: Vec<Value>,
    ) {
        let request_id = s.last_request_id;
        let tools_json = serde_json::to_string(&tools).expect("工具定义可序列化");
        let messages = vec![
            Message::text(Role::System, system),
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
                    "growth_skipped",
                    json!({"kind": kind, "reason": "额度不足"}),
                )
                .request(request_id),
            );
            return;
        }
        self.state.next_request += 1;
        let stamp = crate::session::RequestStamp {
            id: self.state.next_request,
            epoch: self.state.epoch,
            revision: world.actor(&s.actor_id).map_or(0, |a| a.actions.revision),
            seen_cursor: world.actor(&s.actor_id).map_or(0, |a| a.journal.cursor()),
            sim_minutes: world.minutes(),
            reserved_tokens: reserve,
            day: kernel::calendar::day_index(world.minutes()),
            sent_bytes,
            prefix_digest: 0,
            kind,
        };
        s.daily.charge(reserve);
        self.state.lifetime.charge(reserve);
        self.ledger_open(s, &stamp, kind, estimated);
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
            Trace::new("growth_request", json!({"request": request, "kind": kind}))
                .request(request.stamp.id),
        );
        self.queued_growth_requests.push(request);
    }

    pub(super) fn store_pending_post_review(&mut self, actor: &str, pending: PendingPostReview) {
        self.pending_post_reviews.insert(actor.into(), pending);
    }

    pub(super) fn take_pending_post_review(&mut self, actor: &str) -> Option<PendingPostReview> {
        self.pending_post_reviews.remove(actor)
    }

    pub(super) fn pending_post_review(&self, actor: &str) -> Option<&PendingPostReview> {
        self.pending_post_reviews.get(actor)
    }
}
