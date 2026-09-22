//! 进程级运行时状态与 GetStatus / GetSnapshot 载荷组装。
//!
//! 配置为「多方案容器 ProfilesDoc」：`profiles` 锁 = `Mutex<LoadedProfiles>`（interior mut），
//! 每次读 `config_snapshot()` 锁内 clone 再锁外计算（锁窗口 μs）；写操作统一走
//! `write_guard` 串行化，与 `profiles` 锁分离以免嵌套死锁。
//! SetConfig 写入“当前激活 profile 的 config”，Switch 等改 active 并整体原子落盘 + 热重载。

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use dualnic_core::config::{
    AdapterEnv, EnvironmentSnapshot, PolicyConfig, Profile, ProfilesDoc,
};
use dualnic_core::env::{compute_env_fingerprint, match_profile};
use dualnic_core::ip::Prefix;
use dualnic_core::ipc::{
    AutoMatchData, ConfigSummary, EnvCandidate, EnvMatchReport, ProfileListData,
    ProfileListEntry, StatusData,
};
use dualnic_core::role::{AdapterMatcher, AdapterRole, RoleMatcher};

use crate::db::Db;
use crate::paths;

/// 激活 profile 的「配置 + 加载状态」投影（保持 build_* 既有调用点零改动）。
#[derive(Clone)]
pub struct LoadedConfig {
    pub config: PolicyConfig,
    pub loaded: bool,
    pub error: Option<dualnic_core::msg::MessageRef>,
}

/// 配置容器 + 加载状态。
#[derive(Clone)]
pub struct LoadedProfiles {
    pub doc: ProfilesDoc,
    pub loaded: bool,
    pub error: Option<dualnic_core::msg::MessageRef>,
    /// 本次加载是否由 v1 单份迁移而来（启动时需回写为 v2 容器）。
    pub was_migrated: bool,
}

/// 进程级运行时状态（config 可变以实现热重载，其余启动后不变）。
pub struct RuntimeState {
    /// 配置方案容器（active profile 通过 `active_config()` 投影）。
    pub profiles: Mutex<LoadedProfiles>,
    /// 串行化所有写操作，与 `profiles` 锁分离（防嵌套死锁 / 读改写丢更新）。
    pub write_guard: Mutex<()>,
    /// 串行化所有「写路由」操作（与 write_guard 解耦；手动 reconcile_once 阻塞、自动 try_reconcile 去重）。
    pub route_guard: Mutex<()>,
    pub config_path: PathBuf,
    pub listen_addr: SocketAddr,
    pub start_unix_secs: u64,
    /// SQLite（配置 + 事件日志）。
    pub db: Arc<Db>,
    /// 对账是否被暂停（托盘「暂停分流」置位；暂停时手动/自动对账均不写路由）。
    pub paused: AtomicBool,
}

/// 当前 unix 秒（启动时间戳 / ping / snapshot / 快照名用）。
pub fn unix_now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 从 SQLite 读配置容器（无配置 → 惰性默认）。
fn load_doc(db: &Db) -> LoadedProfiles {
    match db.get_config_raw() {
        Ok(Some(s)) => parse_doc(&s),
        Ok(None) => LoadedProfiles {
            doc: ProfilesDoc::default(),
            loaded: false,
            error: Some(dualnic_core::msgref!("DNERR", 83;)),
            was_migrated: false,
        },
        Err(e) => LoadedProfiles {
            doc: ProfilesDoc::default(),
            loaded: false,
            error: Some(e),
            was_migrated: false,
        },
    }
}

/// 解析配置 JSON：若顶层含 `profiles` → 视为容器；否则把 v1 单份 PolicyConfig 包裹为「默认」profile。
fn parse_doc(s: &str) -> LoadedProfiles {
    let v: serde_json::Value = match serde_json::from_str(s) {
        Ok(v) => v,
        Err(e) => {
            return LoadedProfiles {
                doc: ProfilesDoc::default(),
                loaded: false,
                error: Some(dualnic_core::msgref!("DNERR", 24; &e.to_string())),
                was_migrated: false,
            };
        }
    };
    if v.get("profiles").is_none() {
        // 顶层无 profiles → 视为 v1 单份，包装为「默认」profile
        match PolicyConfig::from_json_value(v) {
            Ok(cfg) => {
                let doc = ProfilesDoc {
                    schema_version: dualnic_core::config::PROFILES_SCHEMA_VERSION,
                    active_profile: dualnic_core::config::DEFAULT_PROFILE_NAME.to_string(),
                    profiles: vec![Profile::new(dualnic_core::config::DEFAULT_PROFILE_NAME, cfg)],
                    physical_nic_guids: Vec::new(),
                };
                LoadedProfiles { doc, loaded: true, error: None, was_migrated: true }
            }
            Err(e) => LoadedProfiles {
                doc: ProfilesDoc::default(),
                loaded: false,
                error: Some(e.message()),
                was_migrated: false,
            },
        }
    } else {
        match ProfilesDoc::from_json_value(v) {
            Ok((mut doc, _fb)) => {
                sanitize_default_profile(&mut doc);
                LoadedProfiles { doc, loaded: true, error: None, was_migrated: false }
            }
            Err(e) => LoadedProfiles {
                doc: ProfilesDoc::default(),
                loaded: false,
                error: Some(e.message()),
                was_migrated: false,
            },
        }
    }
}

/// 「默认」方案 = 内置的「不接管」占位：永远为空配置（无角色/无网段/无环境）。
/// 旧版本可能把它编辑过（写入了 matchers/environment），读取时一律归零——
/// 这同时保证它永不参与自动匹配（无 environment 的方案 match_profile 恒 false）。
fn sanitize_default_profile(doc: &mut ProfilesDoc) {
    if let Some(p) = doc.profiles.iter_mut().find(|p| p.name == dualnic_core::config::DEFAULT_PROFILE_NAME) {
        if p.config != dualnic_core::config::PolicyConfig::default() {
            tracing::info!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 20;)));
            p.config = dualnic_core::config::PolicyConfig::default();
        }
    }
}

/// 构建运行时状态（含一次配置容器加载）。若加载回退/损坏，服务照常跑（惰性）。
/// 若旧 v1 单份文件被迁移成 v2 容器，则首启时把容器回写磁盘，避免下一次仍读旧格式。
pub fn load_runtime(listen_addr: SocketAddr, start_unix_secs: u64) -> RuntimeState {
    let config_path = paths::config_path();
    // 打开 SQLite；失败降级内存库（服务照常跑，仅不持久化）
    let db = Arc::new(match Db::open() {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %dualnic_core::msg::t(&e), "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 21;)));
            Db::open_in_memory()
        }
    });
    let lp = load_doc(&db);
    let migrated = lp.was_migrated;
    let state = RuntimeState {
        profiles: Mutex::new(lp),
        write_guard: Mutex::new(()),
        route_guard: Mutex::new(()),
        config_path,
        listen_addr,
        start_unix_secs,
        db,
        paused: AtomicBool::new(false),
    };
    if migrated {
        let doc = { state.profiles.lock().unwrap().doc.clone() };
        if let Err(e) = write_profiles_locked(&state, &doc) {
            tracing::warn!(error = %dualnic_core::msg::t(&e), "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 22;)));
        } else {
            tracing::info!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 23;)));
        }
    }
    state
}

/// 锁内 clone 当前激活配置（投影成旧 LoadedConfig 形状）。随后锁释放，锁外计算。
pub fn config_snapshot(st: &RuntimeState) -> LoadedConfig {
    let lp = st.profiles.lock().unwrap();
    LoadedConfig {
        config: lp.doc.active_config().clone(),
        loaded: lp.loaded,
        error: lp.error.clone(),
    }
}

fn doc_snapshot(st: &RuntimeState) -> LoadedProfiles {
    st.profiles.lock().unwrap().clone()
}

/// 从 SQLite 重读容器并整体替换（热重载）。
pub fn reload_from_db(st: &RuntimeState) {
    let fresh = load_doc(&st.db);
    *st.profiles.lock().unwrap() = fresh;
    tracing::info!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 24;)));
}

/// 内部统一：整容器 pretty 序列化 → SQLite UPSERT（原子）→ reload。
/// 调用方需已持有 `write_guard`。
/// 落盘前统一归一化对账开关（apply_reconciliation_policy）：对账不再由前台配置——
/// 接管 WAN 的方案生效即收敛（双开），未接管 WAN 一律关；「默认」占位除外（保持空配置）。
fn write_profiles_locked(st: &RuntimeState, doc: &ProfilesDoc) -> Result<(), dualnic_core::msg::MessageRef> {
    let mut doc = doc.clone();
    for p in doc.profiles.iter_mut() {
        if p.name != dualnic_core::config::DEFAULT_PROFILE_NAME {
            dualnic_core::config::apply_reconciliation_policy(&mut p.config);
        }
    }
    let json = serde_json::to_string_pretty(&doc)
        .map_err(|e| dualnic_core::msgref!("DNERR", 85; &e.to_string()))?;
    st.db.set_config_raw("main", &json)?;
    reload_from_db(st);
    Ok(())
}

/// SetConfig：写入“当前激活 profile 的 config”（读容器→替换 active→整容器落盘）。
pub fn apply_new_config(st: &RuntimeState, cfg: &PolicyConfig) -> Result<ConfigSummary, dualnic_core::msg::MessageRef> {
    // 注意：保存后立即触发一次自动对账，把刚保存的内网站段写入路由表 ——
    // 否则「配置保存≠修改路由」，用户加内网段后点保存看不到路由变化。
    let summary = {
        let _guard = st.write_guard.lock().unwrap();
        let mut cfg = cfg.clone();
        let mut doc = doc_snapshot(st).doc;
        // 自动按当前环境卡组合填 environment，让手动保存的方案也能参与自动匹配
        #[cfg(windows)]
        {
            fill_environment_from_current(&mut cfg, &doc.physical_nic_guids);
        }
        let active = doc.active_profile.clone();
        let idx = doc
            .profiles
            .iter()
            .position(|p| p.name == active)
            .ok_or_else(|| dualnic_core::msgref!("DNERR", 86; &active))?;
        doc.profiles[idx].config = cfg;
        write_profiles_locked(st, &doc)?;
        st.db.append_event_msg("info", "config", &dualnic_core::msgref!("DNREC", 2;));
        build_summary(st)
    };
    // 写完配置后（已释放 write_guard）触发一次自动对账（try_lock 去重）。
    #[cfg(windows)]
    {
        crate::watch::run_reconcile_if_enabled_ref(st);
    }
    Ok(summary)
}

/// 用当前环境的卡组合（guid+网段）填充方案的 environment 快照。
#[cfg(windows)]
fn fill_environment_from_current(cfg: &mut PolicyConfig, physical: &[String]) {
    if let Ok(adapters) = crate::net::adapters_views() {
        let fp = compute_env_fingerprint(physical, &adapters);
        let mut env_adapters: Vec<AdapterEnv> = fp
            .adapters
            .iter()
            .map(|a| AdapterEnv {
                guid: a.guid.clone().unwrap_or_default(),
                dhcp_server: a.dhcp_server,
            })
            .collect();
        env_adapters.sort_by(|a, b| a.guid.cmp(&b.guid));
        cfg.environment = Some(EnvironmentSnapshot { adapters: env_adapters });
    }
}

/// 从 JSON 文本导入整个配置容器（解析 + 校验 + 原子写 + 热重载）。
pub fn import_config(st: &RuntimeState, config_json: &str) -> Result<ConfigSummary, dualnic_core::msg::MessageRef> {
    let summary = {
        let _guard = st.write_guard.lock().unwrap();
        let parsed = parse_doc(config_json);
        if !parsed.loaded {
            return Err(parsed.error.unwrap_or_else(|| dualnic_core::msgref!("DNERR", 102;)));
        }
        write_profiles_locked(st, &parsed.doc)?;
        st.db.append_event_msg("info", "config", &dualnic_core::msgref!("DNREC", 3;));
        build_summary(st)
    };
    // 导入后触发一次自动对账，让新配置的路由立即落地。
    #[cfg(windows)]
    {
        crate::watch::run_reconcile_if_enabled_ref(st);
    }
    Ok(summary)
}

/// 构建当前配置概要（供各写操作返回）。
fn build_summary(st: &RuntimeState) -> ConfigSummary {
    let lc = config_snapshot(st);
    let mut summary = ConfigSummary::from_config(&lc.config);
    summary.loaded = lc.loaded;
    summary.source_path = Some(st.config_path.display().to_string());
    summary.load_error = lc.error.clone();
    summary
}

// ──────────────────────────── 方案操作 ────────────────────────────

pub fn list_profiles(st: &RuntimeState) -> ProfileListData {
    let lp = doc_snapshot(st);
    let doc = &lp.doc;
    let entries: Vec<ProfileListEntry> = doc
        .profiles
        .iter()
        .map(|p| ProfileListEntry {
            name: p.name.clone(),
            active: p.name == doc.active_profile,
            lan_networks_count: p.config.lan_networks.len(),
            wan_rule_present: !p.config.wan_adapter.matchers.is_empty(),
            lan_rule_present: !p.config.lan_adapter.matchers.is_empty(),
        })
        .collect();
    ProfileListData { active: doc.active_profile.clone(), profiles: entries }
}

pub fn switch_profile(st: &RuntimeState, name: &str) -> Result<ConfigSummary, dualnic_core::msg::MessageRef> {
    let summary = {
        let _guard = st.write_guard.lock().unwrap();
        let mut doc = doc_snapshot(st).doc;
        let idx = doc
            .profiles
            .iter()
            .position(|p| p.name == name)
            .ok_or_else(|| dualnic_core::msgref!("DNERR", 87; name))?;
        if doc.active_profile != name {
            doc.active_profile = name.to_string();
            // 启用时刷新该方案的环境快照（把「此方案适用于当前环境」记录下来，供自动匹配）。
            #[cfg(windows)]
            {
                let mut cfg = doc.profiles[idx].config.clone();
                fill_environment_from_current(&mut cfg, &doc.physical_nic_guids);
                doc.profiles[idx].config = cfg;
            }
            write_profiles_locked(st, &doc)?;
            st.db.append_event_msg("info", "config", &dualnic_core::msgref!("DNREC", 4; &name));
        }
        // 启用已激活的方案 = 重新生效：跳过写盘，直接走下面的对账。
        build_summary(st)
    };
    // 启用后触发一次自动对账，让该方案的路由立即落地（含重新生效同方案的场景）。
    #[cfg(windows)]
    {
        crate::watch::run_reconcile_if_enabled_ref(st);
    }
    Ok(summary)
}

/// 读全局「物理网卡」标记集合（只读，无锁写）。
pub fn get_physical_nics(st: &RuntimeState) -> Vec<String> {
    let doc = doc_snapshot(st).doc;
    doc.physical_nic_guids.clone()
}

/// 写全局「物理网卡」标记集合（写盘 + 热重载 + 触发对账）。
pub fn set_physical_nics(st: &RuntimeState, guids: &[String]) -> Result<(), dualnic_core::msg::MessageRef> {
    {
        let _guard = st.write_guard.lock().unwrap();
        let mut doc = doc_snapshot(st).doc;
        doc.physical_nic_guids = guids.to_vec();
        write_profiles_locked(st, &doc)?;
        st.db.append_event_msg("info", "config", &dualnic_core::msgref!("DNREC", 5; &guids.len()));
    }
    // 标记变化可能影响角色识别/自动匹配 → 触发一次对账。
    #[cfg(windows)]
    {
        crate::watch::run_reconcile_if_enabled_ref(st);
    }
    Ok(())
}

/// 读指定方案的配置（编辑预览用，只读；与激活状态无关）。
pub fn get_profile_config(st: &RuntimeState, name: &str) -> Result<PolicyConfig, dualnic_core::msg::MessageRef> {
    let doc = doc_snapshot(st).doc;
    doc.find(name)
        .map(|p| p.config.clone())
        .ok_or_else(|| dualnic_core::msgref!("DNERR", 87; name))
}

/// 保存指定方案的配置（只持久化，**不触发对账**；生效由 switch_profile 负责）。
/// 入站 config 未带 environment 时保留该方案已存的环境快照（环境在「启用」时刷新）。
pub fn set_profile_config(st: &RuntimeState, name: &str, cfg: &PolicyConfig) -> Result<ConfigSummary, dualnic_core::msg::MessageRef> {
    if name == dualnic_core::config::DEFAULT_PROFILE_NAME {
        return Err(dualnic_core::msgref!("DNERR", 93;));
    }
    let summary = {
        let _guard = st.write_guard.lock().unwrap();
        let mut doc = doc_snapshot(st).doc;
        let idx = doc
            .profiles
            .iter()
            .position(|p| p.name == name)
            .ok_or_else(|| dualnic_core::msgref!("DNERR", 87; name))?;
        let mut new_cfg = cfg.clone();
        if new_cfg.environment.is_none() {
            new_cfg.environment = doc.profiles[idx].config.environment.clone();
        }
        doc.profiles[idx].config = new_cfg;
        write_profiles_locked(st, &doc)?;
        st.db.append_event_msg("info", "config", &dualnic_core::msgref!("DNREC", 6; &name));
        build_summary(st)
    };
    Ok(summary)
}

pub fn create_profile(
    st: &RuntimeState,
    name: &str,
    from: Option<&str>,
) -> Result<ConfigSummary, dualnic_core::msg::MessageRef> {
    let _guard = st.write_guard.lock().unwrap();
    let mut doc = doc_snapshot(st).doc;
    if name.trim().is_empty() || name.len() > 48 {
        return Err(dualnic_core::msgref!("DNERR", 89; name));
    }
    if doc.profiles.iter().any(|p| p.name == name) {
        return Err(dualnic_core::msgref!("DNERR", 88; name));
    }
    let cfg = match from {
        Some(src) => doc
            .find(src)
            .ok_or_else(|| dualnic_core::msgref!("DNERR", 92; src))?
            .config
            .clone(),
        None => PolicyConfig::default(),
    };
    doc.profiles.push(Profile::new(name, cfg));
    write_profiles_locked(st, &doc)?;
    Ok(build_summary(st))
}


pub fn delete_profile(st: &RuntimeState, name: &str) -> Result<ConfigSummary, dualnic_core::msg::MessageRef> {
    if name == dualnic_core::config::DEFAULT_PROFILE_NAME {
        return Err(dualnic_core::msgref!("DNERR", 94;));
    }
    let _guard = st.write_guard.lock().unwrap();
    let mut doc = doc_snapshot(st).doc;
    if doc.profiles.len() <= 1 {
        return Err(dualnic_core::msgref!("DNERR", 95;));
    }
    let idx = doc
        .profiles
        .iter()
        .position(|p| p.name == name)
        .ok_or_else(|| dualnic_core::msgref!("DNERR", 87; name))?;
    let was_active = doc.active_profile == name;
    doc.profiles.remove(idx);
    if was_active {
        // 删的是当前激活方案 → 自动切换到剩余第一个，保证 active 始终有效
        doc.active_profile = doc.profiles[0].name.clone();
    }
    write_profiles_locked(st, &doc)?;
    Ok(build_summary(st))
}

pub fn rename_profile(
    st: &RuntimeState,
    old_name: &str,
    new_name: &str,
) -> Result<ConfigSummary, dualnic_core::msg::MessageRef> {
    let _guard = st.write_guard.lock().unwrap();
    if new_name.trim().is_empty() || new_name.len() > 48 {
        return Err(dualnic_core::msgref!("DNERR", 89; new_name));
    }
    if old_name == dualnic_core::config::DEFAULT_PROFILE_NAME || new_name == dualnic_core::config::DEFAULT_PROFILE_NAME {
        return Err(dualnic_core::msgref!("DNERR", 96;));
    }
    let mut doc = doc_snapshot(st).doc;
    let idx = doc
        .profiles
        .iter()
        .position(|p| p.name == old_name)
        .ok_or_else(|| dualnic_core::msgref!("DNERR", 87; old_name))?;
    if doc.profiles.iter().any(|p| p.name == new_name && p.name != old_name) {
        return Err(dualnic_core::msgref!("DNERR", 88; new_name));
    }
    let was_active = doc.active_profile == old_name;
    doc.profiles[idx].name = new_name.to_string();
    if was_active {
        doc.active_profile = new_name.to_string();
    }
    write_profiles_locked(st, &doc)?;
    Ok(build_summary(st))
}

// ──────────────────────────── 环境检测 / 自动匹配 ────────────────────────────

/// 只读：算当前环境指纹，与所有方案逐一全匹配，产出报告。
#[cfg(windows)]
pub fn detect_environment(st: &RuntimeState) -> EnvMatchReport {
    let doc = doc_snapshot(st).doc;
    match crate::net::adapters_views() {
        Ok(adapters) => {
            let fp = compute_env_fingerprint(&doc.physical_nic_guids, &adapters);
            let mut candidates: Vec<EnvCandidate> = Vec::with_capacity(doc.profiles.len());
            let mut matched_count = 0usize;
            let mut matched_name: Option<String> = None;
            for p in &doc.profiles {
                let r = match_profile(&p.config, &fp, &adapters);
                if r.matched {
                    matched_count += 1;
                    matched_name = Some(p.name.clone());
                }
                candidates.push(EnvCandidate {
                    name: p.name.clone(),
                    matched: r.matched,
                    reason: r.reason,
                });
            }
            let matched_profile = if matched_count == 1 { matched_name } else { None };
            EnvMatchReport {
                current_fingerprint: fp,
                matched_profile,
                candidates,
                read_error: None,
            }
        }
        Err(e) => EnvMatchReport {
            current_fingerprint: Default::default(),
            matched_profile: None,
            candidates: Vec::new(),
            read_error: Some(e),
        },
    }
}

#[cfg(not(windows))]
pub fn detect_environment(_st: &RuntimeState) -> EnvMatchReport {
    EnvMatchReport {
        current_fingerprint: Default::default(),
        matched_profile: None,
        candidates: Vec::new(),
        read_error: Some(dualnic_core::msgref!("DNERR", 97;)),
    }
}

/// 自动匹配并应用：唯一匹配→切换；无匹配→自动新建并切换。只写配置方案，不写路由。
#[cfg(windows)]
pub fn auto_match(st: &RuntimeState) -> Result<AutoMatchData, dualnic_core::msg::MessageRef> {
    let _guard = st.write_guard.lock().unwrap();
    let adapters = crate::net::adapters_views()?;
    let routes = crate::net::all_ipv4_routes()?;
    let mut doc = doc_snapshot(st).doc;
    let fp = compute_env_fingerprint(&doc.physical_nic_guids, &adapters);

    let matches: Vec<String> = doc
        .profiles
        .iter()
        // 「默认」= 不接管占位，永不参与自动匹配；只在用户自定义方案间选择
        .filter(|p| p.name != dualnic_core::config::DEFAULT_PROFILE_NAME)
        .filter(|p| match_profile(&p.config, &fp, &adapters).matched)
        .map(|p| p.name.clone())
        .collect();

    if matches.len() == 1 {
        let target = matches[0].clone();
        if doc.active_profile != target {
            doc.active_profile = target.clone();
            write_profiles_locked(st, &doc)?;
            st.db.append_event_msg("info", "watch", &dualnic_core::msgref!("DNREC", 7; &target));
            tracing::info!(profile = %target, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 25;)));
        }
        return Ok(AutoMatchData {
            action: if doc.active_profile == target { "noop".into() } else { "switched".into() },
            profile: Some(target),
            summary: build_summary(st),
        });
    }
    if matches.len() > 1 {
        return Ok(AutoMatchData { action: "noop".into(), profile: None, summary: build_summary(st) });
    }

    // ── 0 匹配的两种收敛（避免产生重复/垃圾方案）──
    if doc.active_profile == dualnic_core::config::DEFAULT_PROFILE_NAME {
        // 激活的是「默认」（不接管）：没有可认领/可抑制的对象，直接走新建判定。
    } else if let Some(idx) = doc.profiles.iter().position(|p| p.name == doc.active_profile) {
        let cfg0 = &doc.profiles[idx].config;
        let has_matchers =
            !cfg0.wan_adapter.matchers.is_empty() || !cfg0.lan_adapter.matchers.is_empty();
        // A) 认领：激活方案配置了角色但未记录环境（旧档/手工档）→ 把当前环境写进它，
        //    沿用该方案而不是新建双胞胎。
        if cfg0.environment.is_none() && has_matchers {
            let mut cfg = doc.profiles[idx].config.clone();
            fill_environment_from_current(&mut cfg, &doc.physical_nic_guids);
            doc.profiles[idx].config = cfg;
            write_profiles_locked(st, &doc)?;
            let name = doc.active_profile.clone();
            st.db.append_event_msg(
                "info",
                "watch",
                &dualnic_core::msgref!("DNREC", 8; &name),
            );
            return Ok(AutoMatchData {
                action: "adopted".into(),
                profile: Some(name),
                summary: build_summary(st),
            });
        }
        // B) 掉卡抑制：新指纹的卡集合是激活方案记录环境的真子集（只少不多，疑似拔线/瞬态）
        //    → 保持现状，不新建垃圾方案；卡回来后指纹恢复，自然匹配回正方案。
        if let Some(env) = &doc.profiles[idx].config.environment {
            if dualnic_core::env::is_card_loss_subset(&fp, env) {
                tracing::info!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 26;)));
                return Ok(AutoMatchData {
                    action: "noop".into(),
                    profile: None,
                    summary: build_summary(st),
                });
            }
        }
    }

    // 无匹配 → 自动新建并切换
    let (name, newcfg) = auto_create_from_env(&doc, &adapters, &routes)?;
    doc.profiles.push(Profile::new(name.clone(), newcfg));
    doc.active_profile = name.clone();
    write_profiles_locked(st, &doc)?;
    st.db.append_event_msg("info", "watch", &dualnic_core::msgref!("DNREC", 9; &name));
    tracing::info!(profile = %name, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 27;)));
    Ok(AutoMatchData { action: "created".into(), profile: Some(name), summary: build_summary(st) })
}

#[cfg(not(windows))]
pub fn auto_match(_st: &RuntimeState) -> Result<AutoMatchData, String> {
    Err(dualnic_core::msgref!("DNERR", 98;))
}

/// 由当前环境预填一个新方案（WAN=持默认路由卡、LAN=另一块；网段/网关/环境快照一并填）。
#[cfg(windows)]
fn auto_create_from_env(
    doc: &ProfilesDoc,
    adapters: &[dualnic_core::diff::AdapterView],
    routes: &[dualnic_core::diff::RouteView],
) -> Result<(String, PolicyConfig), dualnic_core::msg::MessageRef> {
    let connected = dualnic_core::env::connected_physical(&doc.physical_nic_guids, adapters);
    if connected.is_empty() {
        return Err(dualnic_core::msgref!("DNERR", 100;));
    }
    let default_if: Vec<u32> = routes.iter().filter(|r| r.is_default()).map(|r| r.if_index).collect();

    // WAN = 持默认路由且有网关的卡；否则有网关的第一块；否则第一块
    let wan = connected
        .iter()
        .copied()
        .find(|a| a.gateway_ipv4.is_some() && default_if.contains(&a.if_index))
        .or_else(|| connected.iter().copied().find(|a| a.gateway_ipv4.is_some()))
        .or_else(|| connected.first().copied())
        .ok_or_else(|| dualnic_core::msgref!("DNERR", 101;))?;

    let lan = connected.iter().copied().find(|a| a.if_index != wan.if_index);

    let mut cfg = PolicyConfig::default();
    cfg.wan_adapter = RoleMatcher::new(AdapterRole::Wan).with_matcher(matcher_of(wan));
    // environment = 每块已连接物理卡的「guid+DHCP服务器」组合（按 guid 排序，保证可比）
    let fp = compute_env_fingerprint(&doc.physical_nic_guids, adapters);
    let mut env_adapters: Vec<AdapterEnv> = fp
        .adapters
        .iter()
        .map(|a| AdapterEnv {
            guid: a.guid.clone().unwrap_or_default(),
            dhcp_server: a.dhcp_server,
        })
        .collect();
    env_adapters.sort_by(|a, b| a.guid.cmp(&b.guid));
    cfg.environment = Some(EnvironmentSnapshot { adapters: env_adapters });

    if let Some(lan) = lan {
        cfg.lan_adapter = RoleMatcher::new(AdapterRole::Lan).with_matcher(matcher_of(lan));
        if let (Some(ip), Some(len)) = (lan.primary_ipv4, lan.prefix_len) {
            if let Ok(p) = Prefix::new(ip, len) {
                let note = format!("自动：{}", lan.alias.as_deref().unwrap_or(""));
                cfg.lan_networks.push(dualnic_core::config::LanNetwork::new(p, Some(&note)));
            }
        }
    }

    let name = env_profile_name(doc, wan, lan);
    Ok((name, cfg))
}

/// 网卡 → matcher（优先 GUID，其次描述，再其次名字）。
fn matcher_of(a: &dualnic_core::diff::AdapterView) -> AdapterMatcher {
    if let Some(g) = &a.guid {
        return AdapterMatcher::Guid(g.clone());
    }
    if let Some(d) = &a.description {
        if !d.is_empty() {
            return AdapterMatcher::DescContains(d.clone());
        }
    }
    AdapterMatcher::NameEq(a.alias.clone().unwrap_or_default())
}

/// 生成方案名「环境-<wan网关>-<lan网段>」并去重。
#[cfg(windows)]
fn env_profile_name(
    doc: &ProfilesDoc,
    wan: &dualnic_core::diff::AdapterView,
    lan: Option<&dualnic_core::diff::AdapterView>,
) -> String {
    let wan_part = wan
        .gateway_ipv4
        .map(|g| g.to_string())
        .unwrap_or_else(|| "无网关".to_string());
    let lan_part = lan
        .and_then(|l| l.primary_ipv4.zip(l.prefix_len))
        .and_then(|(ip, len)| Prefix::new(ip, len).ok())
        .map(|p| p.to_string())
        .unwrap_or_else(|| "无内网".to_string());
    let base = format!("环境-{wan_part}-{lan_part}");
    let existing: Vec<&str> = doc.profiles.iter().map(|p| p.name.as_str()).collect();
    unique_name(&existing, base)
}

/// 生成不与 existing 冲突的名字。
fn unique_name(existing: &[&str], base: String) -> String {
    let base = if base.chars().count() > 48 {
        base.chars().take(45).collect::<String>() + "..."
    } else {
        base
    };
    if !existing.contains(&base.as_str()) {
        return base;
    }
    for i in 2.. {
        let cand = format!("{base} {i}");
        if !existing.contains(&cand.as_str()) {
            return cand;
        }
    }
    unreachable!()
}

// ──────────────────────────── 状态载荷 ────────────────────────────

/// 组装 GetStatus 载荷。risk 每次现算（只读，<1ms）；非 Windows 下返回 None。
pub fn build_status(st: &RuntimeState) -> StatusData {
    let doc = doc_snapshot(st).doc;
    let lc = config_snapshot(st);
    let mut config = ConfigSummary::from_config(&lc.config);
    config.loaded = lc.loaded;
    config.source_path = Some(st.config_path.display().to_string());
    config.load_error = lc.error.clone();

    StatusData {
        protocol_version: dualnic_core::ipc::IPC_PROTOCOL_VERSION,
        service: "running".to_string(),
        pid: std::process::id(),
        start_unix_secs: st.start_unix_secs,
        listen_addr: st.listen_addr.to_string(),
        config,
        risk: current_risk(),
        paused: st.paused.load(Ordering::Acquire),
        active_profile: doc.active_profile,
    }
}

/// 读默认路由风险概要；非 Windows 平台（或未枚举到）返回 None（老协议行为）。
#[cfg(windows)]
fn current_risk() -> Option<dualnic_core::ipc::RiskSummary> {
    Some(crate::net::risk_summary())
}

#[cfg(not(windows))]
fn current_risk() -> Option<dualnic_core::ipc::RiskSummary> {
    None
}

/// 组装 GetSnapshot 载荷（只读网络快照 + 意图 diff）。任一步失败 → `read_error` 不崩。
#[cfg(windows)]
pub fn build_snapshot(st: &RuntimeState) -> Option<dualnic_core::ipc::SnapshotData> {
    use dualnic_core::ipc::SnapshotData;
    let fetched = unix_now_secs();
    let adapters = crate::net::adapters_views();
    let routes = crate::net::all_ipv4_routes();
    match (adapters, routes) {
        (Ok(a), Ok(r)) => {
            let cfg = config_snapshot(st).config;
            Some(SnapshotData {
                fetched_at_unix_secs: fetched,
                read_error: None,
                report: dualnic_core::diff::diff_snapshot(&cfg, &a, &r),
            })
        }
        (a, r) => {
            let msg = match (a, r) {
                (Err(e), _) => e,
                (_, Err(e)) => e,
                _ => unreachable!(),
            };
            Some(SnapshotData {
                fetched_at_unix_secs: fetched,
                read_error: Some(msg),
                report: Default::default(),
            })
        }
    }
}

#[cfg(not(windows))]
pub fn build_snapshot(_st: &RuntimeState) -> Option<dualnic_core::ipc::SnapshotData> {
    None
}
