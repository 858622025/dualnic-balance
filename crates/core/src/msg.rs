//! `msg` —— 消息地基（msgid + msgno + `&1..&9` 填空）。
//!
//! - [`MessageRef`]：结构化消息引用（消息类/消息号/参数）。服务与 GUI 之间传它，
//!   而非成品文案——显示语言由 GUI 侧决定；
//! - [`MsgCatalog`]：语言目录，ini 格式：`[消息类]` 分节、`消息号 = 文本模板`；
//! - 全局运行时：[`install`] 按 languages.ini **动态预载全部语言**、[`t`] 渲染、
//!   [`set_language`] 热切换；
//! - 兜底链：当前语言 → 兜底语言（**languages.ini 列表第一个**）→ 消息代码原文
//!   （`DNREC-001(v1, v2)`）。
//!
//! 约定：
//! - `lang/languages.ini` 是唯一语言清单，代码不写死语言列表；新增语言 = 清单加一行
//!   + 放一个 `<code>.ini`，无需改代码；
//! - 语言代码**一律 2 字符**（如 SC/TC/en/ja），非 2 字符的行跳过不予装载；
//! - 语言代码 **SC = 简体中文，TC = 繁体中文**；
//! - `FACTORY_PACKS` 是出厂内置兜底包（仅当 `<code>.ini` 文件缺失时用于该语言），
//!   属内嵌数据表而非语言逻辑。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, RwLock};

/// 出厂内置兜底包（数据表，非语言逻辑）：代码 → 目录文本。
/// `lang/<code>.ini` 缺失时该语言回退到这里的内嵌副本；新增语言不必动它。
pub const FACTORY_PACKS: &[(&str, &str)] = &[
    ("SC", include_str!("../../../lang/SC.ini")),
    ("en", include_str!("../../../lang/en.ini")),
];
/// 出厂语言列表：外部 `languages.ini` 缺失时的兜底清单（数据文件，非代码逻辑）。
pub const FACTORY_LANGUAGES: &str = include_str!("../../../lang/languages.ini");

// ──────────────────────────── 消息引用 ────────────────────────────

/// 一条结构化消息引用。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageRef {
    /// 消息类（按模块划分，如 DNREC / DNDIF / DNWIZ）。
    pub msgid: String,
    /// 类内消息号。
    pub msgno: u32,
    /// 填空参数，对应模板 `&1..&n`。
    #[serde(default)]
    pub args: Vec<String>,
}

impl MessageRef {
    pub fn new(msgid: &str, msgno: u32, args: Vec<String>) -> Self {
        Self { msgid: msgid.to_string(), msgno, args }
    }

    /// 代码形态：`DNREC-001(v1, v2)`。目录全链缺失时的最终兜底显示。
    pub fn code_form(&self) -> String {
        if self.args.is_empty() {
            format!("{}-{:03}", self.msgid, self.msgno)
        } else {
            format!("{}-{:03}({})", self.msgid, self.msgno, self.args.join(", "))
        }
    }
}

/// 构造 [`MessageRef`] 的便捷宏：`msgref!("DNREC", 1; "3", "2")` 或无参 `msgref!("DNREC", 1;)`。
#[macro_export]
macro_rules! msgref {
    // 无参（允许带空分号，调用点视觉统一）
    ($id:expr, $no:expr $(;)?) => {
        $crate::msg::MessageRef::new($id, $no, Vec::new())
    };
    // 带参：分号后跟 &1..&n 对应的表达式
    ($id:expr, $no:expr; $($arg:expr),+ $(,)?) => {
        $crate::msg::MessageRef::new($id, $no, vec![$($arg.to_string()),+])
    };
}

// ──────────────────────────── 语言目录 ────────────────────────────

/// 语言目录：msgid → (msgno → 模板)。
#[derive(Debug, Default, Clone)]
pub struct MsgCatalog {
    map: HashMap<String, HashMap<u32, String>>,
}

impl MsgCatalog {
    /// 解析 ini 文本。容忍 UTF-8 BOM、`;`/`#` 注释行、空行；键按 u32 归一（`001` == `1`）；
    /// 同键重复时后值覆盖；消息号非数字或行缺 `=` 视为解析错误（该语言文件损坏 → 沿链回退）。
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut map: HashMap<String, HashMap<u32, String>> = HashMap::new();
        let mut section = String::new();
        for (idx, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = s.trim().to_string();
                continue;
            }
            let lineno = idx + 1;
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| format!("line {lineno}: missing '=': {raw}"))?;
            let no: u32 = k
                .trim()
                .parse()
                .map_err(|_| format!("line {lineno}: msgno is not a number: {raw}"))?;
            map.entry(section.clone())
                .or_default()
                .insert(no, v.trim().to_string());
        }
        Ok(Self { map })
    }

    pub fn get(&self, msgid: &str, msgno: u32) -> Option<&str> {
        self.map.get(msgid)?.get(&msgno).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// 每个 msgid 的（词条数，最大消息号）——语言文件完整性比对用。
    pub fn section_stats(&self) -> HashMap<&str, (usize, u32)> {
        self.map
            .iter()
            .map(|(k, m)| (k.as_str(), (m.len(), *m.keys().max().unwrap_or(&0))))
            .collect()
    }
}

/// 用参数填充模板 `&1..&9`。`&` 后跟非数字保持原样；`&11` 按 `&1` + 字面 `1` 解析。
/// 任一被引用的占位符超出参数个数 → 返回 None（调用方沿链回退到下一目录）。
pub fn fill(template: &str, args: &[String]) -> Option<String> {
    let chars: Vec<char> = template.chars().collect();
    // 先校验：被引用的最大占位符不得超过参数个数
    let mut i = 0;
    while i + 1 < chars.len() {
        if chars[i] == '&' {
            if let Some(d) = chars[i + 1].to_digit(10) {
                if d as usize > args.len() {
                    return None;
                }
            }
        }
        i += 1;
    }
    // 再替换
    let mut out = String::with_capacity(template.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '&' && i + 1 < chars.len() {
            if let Some(d) = chars[i + 1].to_digit(10) {
                out.push_str(&args[(d - 1) as usize]);
                i += 2;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    Some(out)
}

/// 沿目录链渲染：第一个能完整渲染（键存在且参数个数匹配）的目录胜出；
/// 全链缺失 → 消息代码原文。永不失败、永不 panic。
pub fn resolve(chain: &[&MsgCatalog], msg: &MessageRef) -> String {
    for cat in chain {
        if let Some(t) = cat.get(&msg.msgid, msg.msgno) {
            if let Some(s) = fill(t, &msg.args) {
                return s;
            }
        }
    }
    msg.code_form()
}

// ──────────────────────────── 语言列表 ────────────────────────────

/// 解析 `languages.ini`：`[languages]` 节的 `code = 显示名`，保持文件出现顺序。
/// 解析失败/无该节 → 空列表（调用方回退内嵌列表）。
pub fn parse_language_list(text: &str) -> Vec<(String, String)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = Vec::new();
    let mut in_langs = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            in_langs = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) == Some("languages");
            continue;
        }
        if in_langs {
            if let Some((k, v)) = line.split_once('=') {
                out.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
    }
    out
}

// ──────────────────────────── 全局运行时 ────────────────────────────

/// 已装载语言的一项（保持 languages.ini 的登记顺序）。
#[derive(Debug, Clone)]
pub struct LangEntry {
    pub code: String,
    pub name: String,
}

struct Runtime {
    current: String,
    /// 兜底语言 = languages.ini 列表第一个。
    base: String,
    packs: HashMap<String, Arc<MsgCatalog>>,
    order: Vec<LangEntry>,
}

static RUNTIME: LazyLock<RwLock<Runtime>> = LazyLock::new(|| {
    RwLock::new(Runtime {
        current: String::new(),
        base: String::new(),
        packs: HashMap::new(),
        order: Vec::new(),
    })
});

/// 单个语言的目录装载：外部 `lang/<code>.ini` 优先，缺失回退出厂内置包，再缺失=空目录。
fn load_catalog(code: &str, lang_dir: Option<&Path>) -> Arc<MsgCatalog> {
    if let Some(dir) = lang_dir {
        let p = dir.join(format!("{code}.ini"));
        if let Ok(text) = std::fs::read_to_string(&p) {
            if let Ok(c) = MsgCatalog::parse(&text) {
                return Arc::new(c);
            }
        }
    }
    if let Some((_, text)) = FACTORY_PACKS.iter().find(|(c, _)| *c == code) {
        if let Ok(c) = MsgCatalog::parse(text) {
            return Arc::new(c);
        }
    }
    Arc::default()
}

/// 按语言文件比对基准校验各已装载目录：每个 msgid 的（词条数，最大消息号）
/// 任一落后即产出告警行；多出的词条不报（向前兼容旧程序读新文件）。
/// 基准取出厂 SC 包（`include_str` 与二进制同版本），因此「旧 lang 目录配新 exe」
/// 与「文件中间跳号（如缺 DNWIZ-119）」都会被抓到（后者靠词条数差）。
pub fn compare_packs(
    expected: &MsgCatalog,
    packs: &[(String, Arc<MsgCatalog>)],
) -> Vec<MessageRef> {
    let exp = expected.section_stats();
    let mut out = Vec::new();
    for (code, cat) in packs {
        let act = cat.section_stats();
        for (msgid, (cnt, max)) in &exp {
            match act.get(msgid) {
                None => out.push(msgref!("DNLOG", 90; &format!("{code}:{msgid}"))),
                Some((c2, m2)) if c2 < cnt || m2 < max => out.push(msgref!(
                    "DNLOG", 91; code, msgid,
                    &c2.to_string(), &cnt.to_string(), &m2.to_string(), &max.to_string()
                )),
                _ => {}
            }
        }
    }
    out
}

/// 按 languages.ini **动态预载全部语言**并设置当前语言，并做语言文件完整性校验。
/// - 清单：外部 `lang/languages.ini` 优先，缺失用出厂列表；
/// - 语言代码非 2 字符的行跳过（约定见模块注释）；
/// - 当前语言无效/缺文件时回退兜底语言（列表第一个）；
/// - 重复调用 = 重新预载（例如 lang 目录内容更新后）；
/// - 返回：与出厂基准（内嵌 SC）比对出的告警行（空=全部完整），调用方自行记日志。
pub fn install(lang_dir: Option<&Path>, choice: &str) -> Vec<String> {
    let list_text = lang_dir
        .and_then(|d| std::fs::read_to_string(d.join("languages.ini")).ok())
        .unwrap_or_else(|| FACTORY_LANGUAGES.to_string());
    let list = parse_language_list(&list_text);

    let mut order: Vec<LangEntry> = Vec::new();
    let mut packs: HashMap<String, Arc<MsgCatalog>> = HashMap::new();
    let mut skipped_codes: Vec<String> = Vec::new();
    for (code, name) in list {
        if code.chars().count() != 2 {
            skipped_codes.push(code);
            continue;
        }
        packs.insert(code.clone(), load_catalog(&code, lang_dir));
        order.push(LangEntry { code, name });
    }
    let base = order.first().map(|e| e.code.clone()).unwrap_or_default();
    let current = if !choice.is_empty() && packs.contains_key(choice) {
        choice.to_string()
    } else {
        base.clone()
    };
    // 先取被检目录与基准快照（避免移动后借用），渲染放运行时就位之后。
    let loaded: Vec<(String, Arc<MsgCatalog>)> =
        order.iter().map(|e| (e.code.clone(), packs[&e.code].clone())).collect();
    let factory_sc = FACTORY_PACKS
        .iter()
        .find(|(c, _)| *c == "SC")
        .and_then(|(_, text)| MsgCatalog::parse(text).ok());

    let mut rt = RUNTIME.write().unwrap();
    rt.current = current;
    rt.base = base;
    rt.packs = packs;
    rt.order = order;
    drop(rt);

    // 完整性校验（运行时已就位，告警按进程语言渲染）：
    // 基准 = 出厂内嵌 SC（与二进制同版本），被检 = 刚装载的全部目录
    // （含 SC 自身——外部 SC.ini 落后于内嵌版也会被抓到）。
    let mut warnings: Vec<String> = factory_sc
        .map(|exp| compare_packs(&exp, &loaded))
        .unwrap_or_default()
        .into_iter()
        .map(|m| t(&m))
        .collect();
    for code in skipped_codes {
        warnings.push(t(&msgref!("DNLOG", 92; &code)));
    }
    warnings
}

/// 热切换语言；代码未装载时回退兜底语言。返回实际生效的语言代码。
pub fn set_language(code: &str) -> String {
    let mut rt = RUNTIME.write().unwrap();
    if rt.packs.contains_key(code) {
        rt.current = code.to_string();
    }
    rt.current.clone()
}

/// 当前语言代码（未安装过 → 空串）。
pub fn current_language() -> String {
    RUNTIME.read().unwrap().current.clone()
}

/// 已装载语言列表（languages.ini 顺序）。
pub fn languages() -> Vec<LangEntry> {
    RUNTIME.read().unwrap().order.clone()
}

/// 渲染一条消息：当前语言 → 兜底语言 → 消息代码原文；未安装时同样落到代码原文。
pub fn t(msg: &MessageRef) -> String {
    let rt = RUNTIME.read().unwrap();
    let cur = rt.packs.get(&rt.current);
    let base = rt.packs.get(&rt.base);
    match (cur, base) {
        (Some(a), Some(b)) if Arc::ptr_eq(a, b) => resolve(&[a.as_ref()], msg),
        (Some(a), Some(b)) => resolve(&[a.as_ref(), b.as_ref()], msg),
        (Some(a), None) => resolve(&[a.as_ref()], msg),
        (None, Some(b)) => resolve(&[b.as_ref()], msg),
        (None, None) => msg.code_form(),
    }
}

// ──────────────────────────── 测试 ────────────────────────────
