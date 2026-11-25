//! 回复文字什么时候记成村民的想法。
use crate::{conversation::ModelReply, tools::ToolName};
use kernel::World;
use protocol::{Need, NeedBand, RequestKind};
use std::collections::BTreeSet;

/// 回复文字记成村民的想法。整理轮次的文字是整理记忆时的说明，不记；只是把说出口的话换个说法的
/// 不记；只查看或记目标的一轮只进想法记录，地图上不冒心声。
pub(super) fn note_thought(world: &mut World, actor: &str, kind: RequestKind, reply: &ModelReply) {
    const TRIGGER: &str = "Agent 决策";
    const SURVIVAL_TRIGGER: &str = "生理紧急";
    if kind == RequestKind::Reflection {
        return;
    }
    let thought = reply.thought.trim();
    let spoken = reply
        .calls
        .iter()
        .filter_map(|c| c.arguments["message"].as_str())
        .any(|message| echoes(thought, message));
    if spoken {
        return;
    }
    let lookup = !reply.calls.is_empty()
        && reply
            .calls
            .iter()
            .all(|c| c.name.parse::<ToolName>().is_ok_and(ToolName::is_lookup));
    // 求生时的自述单独标出来，日记和事件流里能和日常想法分开看。
    let trigger = if kind == RequestKind::Life && survival_pressure(world, actor) {
        SURVIVAL_TRIGGER
    } else {
        TRIGGER
    };
    if lookup {
        world.note_quiet_thought(actor, thought, trigger);
    } else {
        world.note_thought(actor, thought, trigger);
    }
}

/// 饥渴进入紧急/崩溃档，或正带着伤病：这一轮的自述算求生决策。
fn survival_pressure(world: &World, actor: &str) -> bool {
    world.actor(actor).is_some_and(|a| {
        a.needs.states().into_iter().any(|state| {
            matches!(state.kind, Need::Water | Need::Fullness)
                && matches!(state.band, NeedBand::Urgent | NeedBand::Collapse)
        }) || !a.needs.active_conditions().is_empty()
    })
}

/// 想法与说出口的话按相邻两字比较，想法里过半的两字片段都出现在这句话里，就算同一句话。
/// 实测模型改写台词当心声时重合在六成以上，真正的心声不到三成。
fn echoes(thought: &str, message: &str) -> bool {
    let pairs = |text: &str| -> BTreeSet<(char, char)> {
        let chars: Vec<char> = text.chars().filter(|c| c.is_alphanumeric()).collect();
        chars.windows(2).map(|w| (w[0], w[1])).collect()
    };
    let (said, heard) = (pairs(thought), pairs(message));
    !said.is_empty() && said.intersection(&heard).count() * 2 >= said.len()
}
