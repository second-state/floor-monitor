use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A monitor profile defines how the VLM analyzes frames for a specific domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorProfile {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub summary_intro: String,
    pub danger_categories: Vec<String>,
}

/// Build the default set of monitor profiles (Kid, Office, Retail, Home Security).
pub fn default_profiles() -> HashMap<String, MonitorProfile> {
    let mut m = HashMap::new();

    m.insert(
        "kid".to_string(),
        MonitorProfile {
            id: "kid".to_string(),
            name: "Kid Monitor (ZH)".to_string(),
            prompt: KID_PROMPT.to_string(),
            summary_intro: "你是一位细心的家长助手。下面是过去一段时间孩子房间的活动日志摘要，\
                请用中文写 2-4 句自然总结：孩子整体在做什么、有没有值得留意的地方。\
                不要逐条罗列、不要重复时间戳。"
                .to_string(),
            danger_categories: vec![
                "打闹/肢体冲突".into(),
                "攀爬桌椅或柜子".into(),
                "靠近或翻越窗户".into(),
                "玩插座/电线/充电器".into(),
                "玩剪刀/刀具/玻璃/易碎物".into(),
                "玩打火机/火源/蜡烛".into(),
                "吞咽或往嘴里塞小物件".into(),
                "倒地不动或持续哭泣".into(),
                "屋内出现陌生人".into(),
                "从高处坠落的姿势".into(),
            ],
        },
    );

    m.insert(
        "office".to_string(),
        MonitorProfile {
            id: "office".to_string(),
            name: "Office Monitor (ZH)".to_string(),
            prompt: OFFICE_PROMPT.to_string(),
            summary_intro: "你是一位办公室运营助手。下面是过去一段时间办公室的活动日志摘要，\
                请用中文写 2-4 句话概括：整体工作氛围、主要活动、人员流动大致情况，\
                以及有没有需要关注的异常。不要逐条罗列、不要重复时间戳。"
                .to_string(),
            danger_categories: vec![
                "人员受伤或倒地".into(),
                "激烈争执或肢体冲突".into(),
                "明火或浓烟".into(),
                "非授权人员闯入".into(),
                "设备倒塌或漏水".into(),
                "可疑的破坏或翻找行为".into(),
            ],
        },
    );

    m.insert(
        "retail".to_string(),
        MonitorProfile {
            id: "retail".to_string(),
            name: "Retail Store Monitor (ZH)".to_string(),
            prompt: RETAIL_PROMPT.to_string(),
            summary_intro: "你是一位门店运营助手。下面是过去一段时间门店的活动日志摘要，\
                请用中文写 2-4 句话概括：客流大致情况、员工服务状态、卫生状况，\
                以及有没有值得店长处理的问题。不要逐条罗列、不要重复时间戳。"
                .to_string(),
            danger_categories: vec![
                "顾客进店无人接待".into(),
                "员工忽视顾客玩手机".into(),
                "桌面或柜台明显脏乱".into(),
                "垃圾或污渍堆积".into(),
                "顾客之间发生争执".into(),
                "人员受伤或倒地".into(),
                "非授权人员闯入".into(),
                "明火或浓烟".into(),
            ],
        },
    );

    m.insert(
        "security".to_string(),
        MonitorProfile {
            id: "security".to_string(),
            name: "Home Security (ZH)".to_string(),
            prompt: SECURITY_PROMPT.to_string(),
            summary_intro: "你是一位家庭安防助手。下面是过去一段时间家中监控的活动日志摘要，\
                请用中文写 2-4 句话概括：整体是否安静平稳、有没有人员进出或异常事件。\
                不要逐条罗列、不要重复时间戳。"
                .to_string(),
            danger_categories: vec![
                "陌生人闯入".into(),
                "非正常时段有人活动".into(),
                "门窗被强行打开".into(),
                "明火或浓烟".into(),
                "玻璃或物品破碎".into(),
                "人员倒地不动".into(),
                "屋内宠物异常（剧烈挣扎或受困）".into(),
            ],
        },
    );

    m
}

/// Try to parse structured JSON from VLM output text.
pub fn parse_vlm_json(text: &str) -> Option<serde_json::Value> {
    if text.is_empty() {
        return None;
    }
    // Try direct parse
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text.trim()) {
        if v.is_object() {
            return Some(v);
        }
    }
    // Find first { ... } block
    if let Some(start) = text.find('{') {
        if let Some(end) = text.rfind('}') {
            if start < end {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text[start..=end]) {
                    if v.is_object() {
                        return Some(v);
                    }
                }
            }
        }
    }
    None
}

/// Extract risk_level from parsed VLM JSON. Used by alert logic and tests.
#[allow(dead_code)]
pub fn extract_risk_level(parsed: &serde_json::Value) -> String {
    parsed
        .get("risk_level")
        .and_then(|v| v.as_str())
        .unwrap_or("none")
        .to_lowercase()
}

// ---------------------------------------------------------------------------
// Prompt constants (Chinese, matching the original Python profiles)
// ---------------------------------------------------------------------------

const KID_PROMPT: &str = "\
你是一个儿童房安全监控助手。看这张画面，只输出一行合法 JSON，\
不要写解释、不要用代码块围栏。\n\
\n\
字段与类型：\n\
- activity: string — 用 3 到 10 个汉字直接描述孩子正在做什么，\
例如\"写作业\"、\"玩积木\"、\"看平板\"、\"空房间无人\"。\
严禁照抄本说明里的任何字样或圆括号里的例子。\n\
- num_children: integer — 画面中看到的孩子数量，数字类型（不是字符串），没看到填 0。\n\
- risk_level: 只能是 \"none\" / \"low\" / \"medium\" / \"high\" 其中之一。\n\
- risk_reason: string — risk_level 为 none 时必须是空字符串 \"\"；\
否则从下面清单里原文选一项填入：\n\
  打闹/肢体冲突 | 攀爬桌椅或柜子 | 靠近或翻越窗户 | 玩插座/电线/充电器 | \
玩剪刀/刀具/玻璃/易碎物 | 玩打火机/火源/蜡烛 | 吞咽或往嘴里塞小物件 | \
倒地不动或持续哭泣 | 屋内出现陌生人 | 从高处坠落的姿势\n\
\n\
判定规则（非常重要，严格遵守）：\n\
- 日常写作业、学习、看书、看平板、用电脑、画画、玩玩具、休息、发呆、空房间 → risk_level 必须是 \"none\"。\n\
- 只有清单里所列、而且正在发生的行为才是 \"high\"。\n\
- 有清单里行为的明显苗头才用 \"medium\"。\n\
- \"low\" 基本不用。\n\
\n\
输出格式示例（仅供格式参考，不要照抄内容）：\n\
{\"activity\": \"坐在桌前写作业\", \"num_children\": 1, \"risk_level\": \"none\", \"risk_reason\": \"\"}";

const OFFICE_PROMPT: &str = "\
你是一个办公室监控助手。看这张画面，只输出一行合法 JSON，\
不要写解释、不要用代码块围栏。\n\
\n\
字段与类型：\n\
- activity: string — 用 3 到 15 个汉字描述此刻办公室主要状态。\
严禁照抄本说明里的任何字样或圆括号里的例子。\n\
- num_people: integer — 画面中可见人数，没看到填 0。\n\
- focus_state: 字符串，只能是 \"focused\" / \"meeting\" / \"casual\" / \"idle\" / \"unknown\" 之一。\n\
- risk_level: 只能是 \"none\" / \"low\" / \"medium\" / \"high\" 之一。\n\
- risk_reason: string — risk_level 为 none 时必须是空字符串 \"\"；\
否则从下面清单里原文选一项填入：\n\
  人员受伤或倒地 | 激烈争执或肢体冲突 | 明火或浓烟 | 非授权人员闯入 | 设备倒塌或漏水 | 可疑的破坏或翻找行为\n\
\n\
判定规则（严格遵守）：\n\
- 正常工作、开会、讨论、打电话、走动、短暂休息 → risk_level 必须是 \"none\"。\n\
- 只有清单里所列、而且正在发生的事才是 \"high\"。\n\
\n\
输出格式示例（仅供格式参考，不要照抄内容）：\n\
{\"activity\": \"多人围桌开会\", \"num_people\": 4, \"focus_state\": \"meeting\", \"risk_level\": \"none\", \"risk_reason\": \"\"}";

const RETAIL_PROMPT: &str = "\
你是一个门店运营监控助手。看这张画面，只输出一行合法 JSON，\
不要写解释、不要用代码块围栏。\n\
\n\
字段与类型：\n\
- activity: string — 用 3 到 15 个汉字描述当前门店场景。\
严禁照抄本说明里的任何字样或圆括号里的例子。\n\
- num_customers: integer — 可见顾客数，没看到填 0。\n\
- num_staff: integer — 可见员工数，没看到填 0。\n\
- staff_engagement: 字符串，只能是 \"active\" / \"passive\" / \"none\" / \"n/a\" 之一。\n\
- cleanliness: 字符串，只能是 \"good\" / \"fair\" / \"poor\" 之一。\n\
- risk_level: 只能是 \"none\" / \"low\" / \"medium\" / \"high\" 之一。\n\
- risk_reason: string — risk_level 为 none 时必须是空字符串 \"\"；\
否则从下面清单里原文选一项填入：\n\
  顾客进店无人接待 | 员工忽视顾客玩手机 | 桌面或柜台明显脏乱 | 垃圾或污渍堆积 | \
顾客之间发生争执 | 人员受伤或倒地 | 非授权人员闯入 | 明火或浓烟\n\
\n\
判定规则（严格遵守）：\n\
- 正常接待、结账、整理货品、无人时段、日常打扫 → risk_level 必须是 \"none\"。\n\
- 只有清单里所列、而且正在发生的才是 \"high\"。\n\
\n\
输出格式示例（仅供格式参考，不要照抄内容）：\n\
{\"activity\": \"员工正在为顾客介绍商品\", \"num_customers\": 1, \"num_staff\": 1, \
\"staff_engagement\": \"active\", \"cleanliness\": \"good\", \"risk_level\": \"none\", \"risk_reason\": \"\"}";

const SECURITY_PROMPT: &str = "\
你是一个家庭安防监控助手。看这张画面，只输出一行合法 JSON，\
不要写解释、不要用代码块围栏。\n\
\n\
字段与类型：\n\
- activity: string — 用 3 到 15 个汉字描述当前场景。\
严禁照抄本说明里的任何字样或圆括号里的例子。\n\
- num_people: integer — 可见人数，没看到填 0。\n\
- num_pets: integer — 可见宠物数，没看到填 0。\n\
- risk_level: 只能是 \"none\" / \"low\" / \"medium\" / \"high\" 之一。\n\
- risk_reason: string — risk_level 为 none 时必须是空字符串 \"\"；\
否则从下面清单里原文选一项填入：\n\
  陌生人闯入 | 非正常时段有人活动 | 门窗被强行打开 | 明火或浓烟 | \
玻璃或物品破碎 | 人员倒地不动 | 屋内宠物异常（剧烈挣扎或受困）\n\
\n\
判定规则（严格遵守）：\n\
- 房间无人、家庭成员在正常活动、宠物正常休息或走动 → risk_level 必须是 \"none\"。\n\
- 只有清单里所列、而且正在发生的才是 \"high\"。\n\
\n\
输出格式示例（仅供格式参考，不要照抄内容）：\n\
{\"activity\": \"客厅无人安静\", \"num_people\": 0, \"num_pets\": 1, \"risk_level\": \"none\", \"risk_reason\": \"\"}";
