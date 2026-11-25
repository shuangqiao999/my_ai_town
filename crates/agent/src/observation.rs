//! 只投影本人状态、眼前对象与已知地图。观察仪的世界全貌不进入村民上下文。
use crate::{interaction, prompts, resident, AgentError};
use kernel::{intent::BuiltinVerb, props::Condition, topology::Prop, world::ActKind, World};
use protocol::{BodyConditionKind, FactKind, Need, NeedBand};
use serde_json::{json, Value};
use std::cmp::Ordering;

/// 建镇生成器给户外活动区域生成的落脚点编号前缀（scripts/mapgen/spatial.mjs 的 spots）。
const SPOT_PREFIX: &str = "spot_";

/// 一次观察最多带的新事件数。
const EVENTS_PER_OBSERVATION: usize = 24;
/// 事件正文进入观察时的截断长度。
pub const EVENT_TEXT_CHARS: usize = 400;

/// 补给地点最多列几条，避免把题面撑大。
const SUPPLY_HINTS_MAX: usize = 6;

/// 一类生存资源：水、食物或治疗。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SupplyKind {
    Water,
    Food,
    Health,
}

impl SupplyKind {
    fn label(self) -> &'static str {
        match self {
            Self::Water => "水",
            Self::Food => "食物",
            Self::Health => "治疗",
        }
    }
}

/// 本人知道的一处补给地点。只描述现有可供性，不替谁做决定。
struct Supply {
    kind: SupplyKind,
    /// 治疗的状况；只恢复健康（不对应具体状况）时为空。
    condition: Option<BodyConditionKind>,
    locale: String,
    name: String,
    verb: String,
    remaining: Option<u32>,
    /// 供餐时段说明；只在不限时以外的食物动作上有。
    serving: Option<String>,
    here: bool,
    home: bool,
}

/// 一天内的分钟数写成 HH:MM。
fn clock_hm(minutes: f64) -> String {
    let t = kernel::time_of_day(minutes);
    format!("{:02}:{:02}", (t / 60.0) as u32, (t % 60.0) as u32)
}

/// 三餐时段说明；空餐表（测试拓扑或不限时）为空。
fn serving_text(world: &World) -> Option<String> {
    (!world.topo.meals.is_empty()).then(|| {
        world
            .topo
            .meals
            .iter()
            .map(|meal| {
                format!(
                    "{} {}–{}",
                    meal.name,
                    clock_hm(meal.start),
                    clock_hm(meal.end)
                )
            })
            .collect::<Vec<_>>()
            .join("、")
    })
}

/// 食物动作带 meal_window 条件时，给出供餐时段说明。
fn food_serving(world: &World, spot: &kernel::affordance::Spot) -> Option<String> {
    spot.action
        .as_ref()
        .filter(|action| action.conditions.contains(&Condition::MealWindow))
        .and_then(|_| serving_text(world))
}

/// 此刻这个食物动作能不能开始；不限时的动作永远能开始。
fn serving_open(world: &World, serving: Option<&str>) -> bool {
    serving.is_none()
        || world.topo.meals.is_empty()
        || kernel::calendar::meal_at(&world.topo.meals, world.minutes()).is_some()
}

/// 先当前地点，再自己的家，再公共地点，按名字排。
fn supply_order(x: &Supply, y: &Supply) -> Ordering {
    y.here
        .cmp(&x.here)
        .then(y.home.cmp(&x.home))
        .then(x.name.cmp(&y.name))
        .then(x.kind.label().cmp(y.kind.label()))
}

/// 本人知道、还有剩量的补给地点。熟悉世界看全部地点；陌生世界只看记得的地方（含家和当前地点）。
/// 跳过别人住宅和看不见的东西；这里只查现有可供性，不新增动作，也不保证去了一定有。
fn known_supplies(world: &World, actor: &str, a: &kernel::world::Actor) -> Vec<Supply> {
    let here = a.locus.locale.as_str();
    let locales: Vec<String> = match world.known_places(actor) {
        Some(places) => places.into_iter().map(|p| p.id).collect(),
        None => world.topo.places.iter().map(|p| p.locale.clone()).collect(),
    };
    let mut supplies = Vec::new();
    for locale in locales {
        let Some(place) = world.topo.place_of(&locale) else {
            continue;
        };
        let home = a.home.as_deref() == Some(locale.as_str());
        if place.residential && !home {
            continue;
        }
        for spot in world.affordances.options(&locale) {
            if !world.sees_prop(actor, &spot.prop) {
                continue;
            }
            let remaining = spot
                .action
                .as_ref()
                .and_then(|action| world.props().supply_remaining(&spot.prop, action));
            if remaining == Some(0) {
                continue;
            }
            let mut push = |kind: SupplyKind, condition: Option<BodyConditionKind>| {
                supplies.push(Supply {
                    kind,
                    condition,
                    locale: locale.clone(),
                    name: place.name.clone(),
                    verb: spot.verb.clone(),
                    remaining,
                    serving: (kind == SupplyKind::Food)
                        .then(|| food_serving(world, spot))
                        .flatten(),
                    here: locale == here,
                    home,
                });
            };
            if spot.restores_water() {
                push(SupplyKind::Water, None);
            }
            if spot.restores_fullness() {
                push(SupplyKind::Food, None);
            }
            if spot.restores_health() {
                push(SupplyKind::Health, None);
            }
            for condition in [
                BodyConditionKind::Injury,
                BodyConditionKind::Illness,
                BodyConditionKind::Poison,
            ] {
                if spot.cures(condition) {
                    push(SupplyKind::Health, Some(condition));
                }
            }
        }
    }
    supplies.sort_by(supply_order);
    supplies
}

/// 静态地图里的补给索引：不带剩余量、路程和此刻是否供餐（system 前缀保持稳定），
/// 只给地点、需求和动词；食物另带时段说明。
fn supply_index(world: &World, actor: &str, a: &kernel::world::Actor) -> Vec<Value> {
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for s in known_supplies(world, actor, a) {
        if !seen.insert((s.locale.clone(), s.kind.label())) {
            continue;
        }
        let mut view = json!({
            "need": s.kind.label(),
            "place": s.locale,
            "name": s.name,
            "verb": s.verb
        });
        if let Some(serving) = &s.serving {
            view["serving"] = json!(serving);
        }
        out.push(view);
        if out.len() >= SUPPLY_HINTS_MAX {
            break;
        }
    }
    out
}

/// 饥渴进入紧急或崩溃档时，把本人知道的补水/进食地点随观察发出去：需要什么、去哪、要几分钟、
/// 到了做什么。水排在食物前面；供餐中的食物优先。只补信息，去不去、怎么去仍由模型决定；
/// 不知道时给空列表，模型得自己探索。
fn survival_hints(world: &World, actor: &str, a: &kernel::world::Actor) -> Option<Value> {
    let urgent: Vec<Need> = a
        .needs
        .states()
        .into_iter()
        .filter(|state| {
            matches!(state.kind, Need::Water | Need::Fullness)
                && matches!(state.band, NeedBand::Urgent | NeedBand::Collapse)
        })
        .map(|state| state.kind)
        .collect();
    if urgent.is_empty() {
        return None;
    }
    let mut supplies: Vec<Supply> = known_supplies(world, actor, a)
        .into_iter()
        .filter(|s| match s.kind {
            SupplyKind::Water => urgent.contains(&Need::Water),
            SupplyKind::Food => urgent.contains(&Need::Fullness),
            SupplyKind::Health => false,
        })
        .collect();
    supplies.sort_by(|x, y| {
        let rank = |kind| match kind {
            SupplyKind::Water => 0,
            SupplyKind::Food => 1,
            SupplyKind::Health => 2,
        };
        rank(x.kind)
            .cmp(&rank(y.kind))
            .then_with(|| {
                if x.kind == SupplyKind::Food {
                    serving_open(world, y.serving.as_deref())
                        .cmp(&serving_open(world, x.serving.as_deref()))
                } else {
                    Ordering::Equal
                }
            })
            .then_with(|| supply_order(x, y))
    });
    let mut seen = std::collections::BTreeSet::new();
    let mut hints = Vec::new();
    for s in supplies {
        if !seen.insert((s.locale.clone(), s.kind.label())) {
            continue;
        }
        let mut hint = json!({
            "need": s.kind.label(),
            "place": s.locale,
            "name": s.name,
            "verb": s.verb
        });
        if let Some(minutes) = world.travel_minutes(actor, &s.locale) {
            hint["minutes"] = json!(minutes.round().max(0.0) as u32);
        }
        if let Some(remaining) = s.remaining {
            hint["remaining"] = json!(remaining);
        }
        if s.kind == SupplyKind::Food {
            if let Some(serving) = &s.serving {
                hint["open"] = json!(serving_open(world, Some(serving)));
                hint["serving"] = json!(serving);
            }
        }
        if s.here {
            hint["here"] = json!(true);
        }
        hints.push(hint);
        if hints.len() >= SUPPLY_HINTS_MAX {
            break;
        }
    }
    Some(json!(hints))
}

/// 有伤病或健康危险时，列出本人知道的、能治对应状况或恢复健康的地点。
fn treatment_hints(world: &World, actor: &str, a: &kernel::world::Actor) -> Option<Value> {
    let conditions: Vec<BodyConditionKind> = a
        .needs
        .active_conditions()
        .iter()
        .map(|condition| condition.kind)
        .collect();
    let health_low = a.needs.states().into_iter().any(|state| {
        state.kind == Need::Health && matches!(state.band, NeedBand::Urgent | NeedBand::Collapse)
    });
    if conditions.is_empty() && !health_low {
        return None;
    }
    let supplies: Vec<Supply> = known_supplies(world, actor, a)
        .into_iter()
        .filter(|s| s.kind == SupplyKind::Health)
        .filter(|s| match s.condition {
            Some(condition) => conditions.contains(&condition),
            None => health_low,
        })
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    let mut hints = Vec::new();
    for s in supplies {
        if !seen.insert((
            s.locale.clone(),
            s.condition.map(BodyConditionKind::label).unwrap_or("健康"),
        )) {
            continue;
        }
        let mut hint = json!({
            "place": s.locale,
            "name": s.name,
            "verb": s.verb
        });
        match s.condition {
            Some(condition) => hint["condition"] = json!(condition.label()),
            None => hint["need"] = json!("健康"),
        }
        if let Some(minutes) = world.travel_minutes(actor, &s.locale) {
            hint["minutes"] = json!(minutes.round().max(0.0) as u32);
        }
        if let Some(remaining) = s.remaining {
            hint["remaining"] = json!(remaining);
        }
        if s.here {
            hint["here"] = json!(true);
        }
        hints.push(hint);
        if hints.len() >= SUPPLY_HINTS_MAX {
            break;
        }
    }
    Some(json!(hints))
}

/// 不随位置变化的部分：家、已知地图与基础动作。放在 system 消息里，请求前缀保持稳定。
/// 陌生环境里已知地图随探索变化，挪进观察（known_places、exits），这里不列。
pub fn static_map(world: &World, actor: &str) -> Result<Value, AgentError> {
    let a = resident(world, actor)?;
    let basic_actions = json!([
        {"verb":"前往","parameters":prompts::GO_PARAMETERS},
        {"verb":"停留","parameters":prompts::STAY_PARAMETERS},
        {"verb":"查看四周","parameters":prompts::LOOK_PARAMETERS},
        {"verb":"搜寻","parameters":prompts::SEARCH_PARAMETERS},
        {"verb":"藏","parameters":prompts::HIDE_PARAMETERS}
    ]);
    if world.strangers() {
        return Ok(json!({"home": a.home, "basic_actions": basic_actions}));
    }
    let places: Vec<_> = world
        .topo
        .places
        .iter()
        .map(|p| {
            // 别人的住宅只列名字；本人的家和公共处所列那里能做的事。
            let others_home = p.residential && a.home.as_deref() != Some(p.locale.as_str());
            let verbs: Vec<&str> = if others_home {
                vec![]
            } else {
                world
                    .affordances
                    .options(&p.locale)
                    .iter()
                    .map(|s| s.verb.as_str())
                    .collect()
            };
            json!({"id":p.locale,"name":p.name,"verbs":verbs})
        })
        .collect();
    let mut map = json!({
        "home": a.home,
        "known_places": places,
        "basic_actions": basic_actions
    });
    // 补给索引单独给，避免被 known_places 的字节额度截掉；没有补给点时不写这一栏。
    let supplies = supply_index(world, actor, a);
    if !supplies.is_empty() {
        map["supply_places"] = json!(supplies);
    }
    Ok(map)
}

/// 给村民看的一组物品：同样的东西一堆只列一个编号（堆里编号最小的那件）和件数，要几件时把编号写几遍，
/// 由系统从这一堆里挑（tools 的 expand_stacks）。编号是 64 位哈希的那些年头，一堆五件就是五个
/// 七十字的编号，背包在题面里被截成开头一段（大逃杀第三局）。`detail` 时再带能拿它做的事（动作名）
/// 和书信的元数据；动作的完整说明用 inspect 看。
pub(crate) fn item_stacks<'a>(
    world: &World,
    items: impl IntoIterator<Item = &'a Prop>,
    detail: bool,
) -> Vec<Value> {
    world
        .item_stacks(items)
        .into_iter()
        .map(|stack| {
            let p = stack[0];
            let mut view = json!({"id": p.id, "name": p.name, "quantity": stack.len()});
            if detail {
                let actions: Vec<_> = p
                    .verbs
                    .iter()
                    .filter(|v| v.action.is_some())
                    .map(|v| &v.verb)
                    .collect();
                if !actions.is_empty() {
                    view["actions"] = json!(actions);
                }
                if let Some(text) = &p.readable {
                    view["readable"] = json!(text.metadata());
                }
            }
            view
        })
        .collect()
}

/// 本人背包：格数照背包算，物品一堆一条。
fn inventory_view(world: &World, actor: &str) -> Value {
    let bag = world.inventory_view(actor);
    let carried = world
        .topo
        .props()
        .iter()
        .filter(|p| !p.withdrawn && world.inventory().carried_by(&p.id, actor));
    json!({"capacity": bag.capacity, "used": bag.used, "slots": item_stacks(world, carried, true)})
}

/// 本人看得到的容器里存着的东西，一堆一条；够不着或进不了别人家时为空。
fn stored_view(world: &World, actor: &str, container: &str) -> Vec<Value> {
    let ids: std::collections::BTreeSet<String> = world
        .container_items(actor, container)
        .into_iter()
        .map(|item| item.id)
        .collect();
    item_stacks(
        world,
        world.topo.props().iter().filter(|p| ids.contains(&p.id)),
        false,
    )
}

fn prop_view(world: &World, actor: &str, p: &kernel::topology::Prop) -> Value {
    let verbs: Vec<_> = p
        .verbs
        .iter()
        .map(|v| {
            json!({
                "verb": v.verb,
                "action_id": v.action.as_ref().map(|a| &a.id),
                "minutes": v.duration_minutes,
                "description": v.action.as_ref().map(|a| kernel::props::action_description_for(a, world.physiology())),
                "remaining": v.action.as_ref().and_then(|a| world.props().supply_remaining(&p.id, a))
            })
        })
        .collect();
    let mut view = json!({
        "id": p.id,
        "name": p.name,
        "portable": p.portable,
        "storage_slots": p.storage_slots,
        "stored_items": stored_view(world, actor, &p.id),
        "readable": p.readable.as_ref().map(kernel::content::Text::metadata),
        "board": p.board,
        "bulletin_board": p.bulletin_board,
        "verbs": verbs
    });
    with_description(&mut view, p);
    // 别人正在用：看得出要等，不必试了才知道（回声湾一台咖啡机三个人抢，第一个小时失败 6 次）。
    // 没人用时不带这一栏，和原来一字不差。
    let users: Vec<_> = world
        .prop_occupants(&p.id)
        .iter()
        .filter(|id| *id != actor)
        .filter_map(|id| world.actor(id))
        .map(|a| match a.current.left_text() {
            left if left.is_empty() => format!("{}正在{}", a.name, a.current.verb),
            left => format!("{}正在{}，{left}", a.name, a.current.verb),
        })
        .collect();
    if !users.is_empty() {
        view["in_use"] = json!(users.join("；"));
    }
    view
}

/// 道具定义写了说明才附带；遮蔽处另外标出。
fn with_description(view: &mut Value, p: &kernel::topology::Prop) {
    if !p.description.is_empty() {
        view["description"] = json!(p.description);
    }
    if p.shelter {
        view["shelter"] = json!(true);
    }
}

fn people_here(world: &World, actor: &str, locale: &str) -> Vec<Value> {
    world
        .actor_ids()
        .into_iter()
        .filter(|id| id != actor)
        .filter_map(|id| {
            let other = world.actor(&id)?;
            if other.locus.locale != locale {
                return None;
            }
            let mut body = json!({
                "life": if other.needs.body.dead { "dead" } else { "alive" },
                "conditions": other.needs.active_conditions().iter().map(|c| c.kind).collect::<Vec<_>>(),
                "limitation": other.needs.limitation()
            });
            // 遇害的尸体看得见伤口，看不见凶手。
            if let Some(k) = &other.needs.body.killed {
                body["wound"] = json!(k.wound);
            }
            let alive = !other.needs.body.dead;
            let awake = world.awake(&id);
            let carrying = item_stacks(
                world,
                world
                    .topo
                    .props()
                    .iter()
                    .filter(|p| !p.withdrawn && world.inventory().carried_by(&p.id, &id)),
                false,
            );
            // 眼下能对他动手的方式：活人能推；身上有东西时，醒着的能抢，睡着、倒下的能偷，死了的能搜身
            let mut force = Vec::new();
            if alive {
                force.push(BuiltinVerb::Push.label());
            }
            if !carrying.is_empty() {
                force.push(if !alive {
                    BuiltinVerb::Loot.label()
                } else if awake {
                    BuiltinVerb::Rob.label()
                } else {
                    BuiltinVerb::Steal.label()
                });
            }
            let mut view = json!({
                "id": id,
                "name": other.name,
                "activity": other.current.verb,
                "available": world.actor(&id).is_some_and(|a| a.interaction_available()),
                "can_talk": world.talk_available(&id),
                "body": body
            });
            if alive && !awake {
                view["asleep"] = json!(true);
            }
            if !carrying.is_empty() {
                view["carrying"] = json!(carrying);
            }
            if !other.is_avatar() && !force.is_empty() {
                view["force_verbs"] = json!(force);
            }
            Some(view)
        })
        .collect()
}

/// 没逐条列出的更早事件并进 earlier_events：几条、其中几句是交谈、从几点到几点。`skipped` 是
/// （时刻，是不是交谈），按时间先后；已有的 earlier_events 比它们更早，起点不变，别的栏目留着。
pub(crate) fn note_earlier(view: &mut Value, skipped: impl IntoIterator<Item = (f64, bool)>) {
    let earlier = &view["earlier_events"];
    let mut omitted = earlier["omitted"].as_u64().unwrap_or(0);
    let mut speech = earlier["speech"].as_u64().unwrap_or(0);
    let mut from = earlier["from"].as_str().map(str::to_owned);
    let mut to = None;
    for (at, said) in skipped {
        omitted += 1;
        speech += u64::from(said);
        from.get_or_insert_with(|| kernel::clock_string(at));
        to = Some(at);
    }
    if let Some(to) = to {
        let earlier = &mut view["earlier_events"];
        earlier["omitted"] = json!(omitted);
        earlier["speech"] = json!(speech);
        earlier["from"] = json!(from);
        earlier["to"] = json!(kernel::clock_string(to));
    }
}

/// 略过的交谈是和谁、在哪：醒来知道「刚才在哪、和谁聊了多久」，原话要时再 recall。
fn chat_partners(
    world: &World,
    actor: &str,
    skipped: &[protocol::WorldFact],
) -> (Vec<String>, Vec<String>) {
    let (mut with, mut places) = (Vec::new(), Vec::new());
    for event in skipped.iter().filter(|e| e.kind == FactKind::Speech) {
        let speaker = event.objects.first().filter(|id| *id != actor);
        if let Some(other) = speaker.and_then(|id| world.actor(id)) {
            if !with.contains(&other.name) {
                with.push(other.name.clone());
            }
        }
        for object in event
            .objects
            .iter()
            .filter(|o| world.topo.locales.contains_key(*o))
        {
            let name = world.topo.locale_name(object);
            if !places.contains(&name) {
                places.push(name);
            }
        }
    }
    (with, places)
}

/// 随时间和位置变化的部分：新事件、身体、眼前道具与人、互动。
///
/// 新事件积压多时只列最近的：交谈轮次不推进生活这边的游标（runtime/chat.rs），聊得久，攒下的多是
/// 聊天里听过的台词。从最早的一页页补看，醒来就活在几十分钟前（回声湾：到了灯塔还以为在老屋，一起聊
/// 过的小满落后两个多小时）。更早的只在 earlier_events 里报条数和时间段，游标照样到最新；原话照样进
/// 记忆（memory 有自己的进度），可以 recall。通知例外，照常列出。
pub fn observe(world: &World, actor: &str, after: u64) -> Result<(Value, u64), AgentError> {
    let a = resident(world, actor)?;
    let unread = a.journal.events_after(after);
    let cursor = unread.last().map_or(after, |event| event.event_id);
    let (older, recent) = unread.split_at(unread.len().saturating_sub(EVENTS_PER_OBSERVATION));
    let events: Vec<_> = older
        .iter()
        .filter(|event| event.kind == FactKind::Notice)
        .chain(recent)
        .map(|event| {
            let mut event = event.clone();
            event.text = event.text.chars().take(EVENT_TEXT_CHARS).collect();
            event
        })
        .collect();
    let props: Vec<_> = world
        .topo
        .props()
        .iter()
        .filter(|p| p.locale == a.locus.locale && !p.withdrawn && world.sees_prop(actor, &p.id))
        .map(|p| prop_view(world, actor, p))
        .collect();
    let remaining = match &a.current.kind {
        ActKind::Travel { journey, .. } => format!(
            "去{}，{}",
            world.topo.locale_name(&journey.dest_locale),
            a.current.left_text()
        ),
        _ => a.current.left_text(),
    };
    let interactions: Vec<_> = world
        .interactions()
        .for_actor(actor)
        .iter()
        .map(|i| interaction::observation(world, actor, i))
        .collect();
    let mut value = json!({
        "actor_id": actor,
        "environment": world.weather_context(actor),
        "inventory": inventory_view(world, actor),
        "sim_minutes": world.minutes(),
        "clock": world.clock(),
        "revision": a.actions.revision,
        "locale": a.locus.locale,
        "location": world.topo.locale_name(&a.locus.locale),
        "needs": needs_for_model(a),
        "body": world.body_view(actor),
        "current": {
            "verb": a.current.verb,
            "description": a.current.content_label(),
            "remaining": remaining
        },
        "visible_props": props,
        "people_here": people_here(world, actor, &a.locus.locale),
        "events": events,
        "event_cursor": cursor,
        "interactions": interactions,
        "nearby_chats": world.nearby_chats(actor),
    });
    note_earlier(
        &mut value,
        older
            .iter()
            .filter(|event| event.kind != FactKind::Notice)
            .map(|event| (event.at, event.kind == FactKind::Speech)),
    );
    let (with, places) = chat_partners(world, actor, older);
    if !with.is_empty() {
        value["earlier_events"]["with"] = json!(with);
    }
    if !places.is_empty() {
        value["earlier_events"]["places"] = json!(places);
    }
    // 建镇时生成的落脚点（水边、东头……）：停留时 target 填它的 id 就走过去
    let spots: Vec<_> = world
        .topo
        .stations
        .values()
        .filter(|s| s.locale == a.locus.locale && s.id.starts_with(SPOT_PREFIX))
        .map(|s| json!({"id": s.id, "name": s.name}))
        .collect();
    if !spots.is_empty() {
        value["spots"] = json!(spots);
    }
    // 玩法（潮水圈……）：开着的世界才有这一栏，键名是玩法的名字；没开时观察和原来一字不差
    if let Some(context) = world.game_mode_context(actor) {
        value[context.key()] = json!(context);
    }
    // 陌生环境：记得的地方和眼前出去的路随探索变，跟着观察走（没变时按慢变字段省略）
    if let Some(known) = world.known_places(actor) {
        value["known_places"] = json!(known);
    }
    if let Some(exits) = world.exits(actor) {
        value["exits"] = json!(exits);
    }
    // 饥渴进入紧急档：把知道的补水/进食地点随观察发出去。只补信息，走不走由本人决定。
    if let Some(hints) = survival_hints(world, actor, a) {
        value["survival_hints"] = hints;
    }
    // 受伤、疾病、中毒或健康危险：列出能治的地方。
    if let Some(hints) = treatment_hints(world, actor, a) {
        value["treatment_hints"] = hints;
    }
    Ok((value, cursor))
}

pub fn inspect(world: &World, actor: &str, id: &str) -> Result<Value, AgentError> {
    resident(world, actor)?;
    if let Some(item) = world.interactions().of(actor, id) {
        return Ok(json!({"interaction":interaction::observation(world,actor,item)}));
    }
    if let Some(prop) = world
        .topo
        .props()
        .iter()
        .find(|p| p.id == id && world.can_access_prop(actor, p))
    {
        let switches = world.physiology();
        let actions: Vec<_> = prop
            .verbs
            .iter()
            .filter_map(|v| {
                let spot = world.affordances.for_prop(&prop.id, &v.verb)?;
                let mut action = json!({
                    "verb": v.verb,
                    "action_id": v.action.as_ref().map(|a| &a.id),
                    "description": v.action.as_ref().map(|a| kernel::props::action_description_for(a, switches)),
                    "minimum_minutes": spot.minutes,
                    "remaining": v.action.as_ref().and_then(|a| world.props().supply_remaining(&prop.id, a)),
                    "labor": spot.labor
                });
                // 关掉的生理项不出现在村民能看到的数值里。
                for (on, key, rate) in [
                    (switches.fullness, "fullness_per_minute", spot.fullness_rate),
                    (switches.energy, "energy_per_minute", spot.energy_rate),
                    (switches.water, "water_per_minute", spot.water_rate),
                    (switches.health, "health_per_minute", spot.health_rate),
                ] {
                    if on {
                        action[key] = json!(rate);
                    }
                }
                Some(action)
            })
            .collect();
        let mut view = json!({
            "at": world.minutes(),
            "prop_id": prop.id,
            "portable": prop.portable,
            "stored_items": stored_view(world, actor, &prop.id),
            "location": world.inventory().location(&prop.id),
            "locale": prop.locale,
            "readable": prop.readable.as_ref().map(kernel::content::Text::metadata),
            "board": prop.board,
            "actions": actions,
            "occupants": world.prop_occupants(&prop.id)
        });
        with_description(&mut view, prop);
        return Ok(view);
    }
    let known = world
        .known_places(actor)
        .is_none_or(|list| list.iter().any(|p| p.id == id));
    if let Some(place) = world.topo.places.iter().find(|p| p.locale == id && known) {
        return Ok(json!({"at":world.minutes(),"place_id":id,"name":place.name}));
    }
    Err(AgentError::NotFound(
        "对象不在眼前，也不属于已知地图".into(),
    ))
}

/// 给模型看的需求：名字、数值、档位与说明。需求种类只给前端判断用，不进观察，
/// 模型看到的内容与加这个字段之前相同。
fn needs_for_model(a: &kernel::world::Actor) -> Vec<Value> {
    a.needs
        .states()
        .into_iter()
        .map(|n| json!({"need": n.need, "value": n.value, "band": n.band, "text": n.text}))
        .collect()
}
