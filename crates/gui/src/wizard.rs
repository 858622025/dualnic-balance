//! 「配置向导」第四页签——四列角色表（外网/内网/默认）。
//!
//! 每张已连接物理卡默认在「默认」（不参与分流）；可手动把某卡设为「外网」或「内网」，
//! 或点「自动识别（推荐）」由工具按连通探测/默认网关把两块主卡设好。简易分流 = 至多
//! 1 外 + 1 内，多余卡保持「默认」（多卡多内网留待后续 schema 扩展）。
//! 保存仍走 IPC 握手 → SetConfig → 服务热重载（见 client.rs / service）。

use std::sync::{Arc, Mutex};

use eframe::egui;

use dualnic_core::config::{LanNetwork, PolicyConfig, SCHEMA_VERSION};
use dualnic_core::diff::{AdapterView, ReachSource, ReachableNet};
use dualnic_core::ip::Prefix;
use dualnic_core::role::{normalize_guid, AdapterMatcher, AdapterProbe, AdapterRole, RoleMatcher};

use crate::client;
use crate::msgs;
use crate::state::PollState;

/// 底部消息栏的一条消息（单槽：新消息直接覆盖旧消息显示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// 操作成功。
    Ok,
    /// 警示（回退判定 / 重复等，可继续操作）。
    Warn,
    /// 失败（需要用户处理）。
    Err,
}

impl Notice {
    pub fn ok(text: impl Into<String>) -> Self {
        Notice { kind: NoticeKind::Ok, text: text.into() }
    }
    pub fn warn(text: impl Into<String>) -> Self {
        Notice { kind: NoticeKind::Warn, text: text.into() }
    }
    pub fn err(text: impl Into<String>) -> Self {
        Notice { kind: NoticeKind::Err, text: text.into() }
    }
    fn color(&self) -> egui::Color32 {
        match self.kind {
            NoticeKind::Ok => egui::Color32::from_rgb(60, 160, 90),
            NoticeKind::Warn => egui::Color32::from_rgb(220, 170, 60),
            NoticeKind::Err => egui::Color32::from_rgb(230, 90, 70),
        }
    }
}

/// 后台共享态（UI 只读快照；token/结果由后台写、UI 读）。
#[derive(Default)]
pub struct WizardShared {
    pub baseline: Option<Result<PolicyConfig, String>>,
    pub loading_baseline: bool,
    pub adapters: Vec<AdapterView>,
    pub reachable: Vec<ReachableNet>,
    pub loading_adapters: bool,
    /// 进页签强制刷新网卡（USB 网卡随时插拔）：置位后 ensure_data 无条件重拉一次。
    pub refresh_adapters_wanted: bool,
    /// 全局「物理网卡」标记集合（GetPhysicalNics；物理卡判定唯一依据）。
    pub physical_guids: Vec<String>,
    pub saving: bool,
    pub token: Option<String>,
    pub probe_wanted: bool,
    pub probe_running: bool,
    pub probe_outcome: Option<Result<Vec<u32>, String>>,
    /// 探测目标（UI 每帧从编辑缓冲同步；空则用默认 apple.com）。
    pub probe_target: String,
    /// 点「自动识别」置位：探测完成后把角色自动应用到两块主卡。
    pub auto_apply_requested: bool,
    /// 配置方案列表（app 顶部方案栏用）。
    pub profiles: Vec<dualnic_core::ipc::ProfileListEntry>,
    pub active_profile: String,
    pub loading_profiles: bool,
    /// 底部消息栏：最近一条消息（保存/探测/方案操作/网段编辑的结果），新消息覆盖旧消息。
    pub notice: Option<Notice>,
    /// 切换成功后置位：editor 下帧据此刻重置编辑缓冲并刷新 profiles。
    pub profile_switch_done: bool,
    /// 环境检测报告（方案栏环境状态行用）。
    pub env_report: Option<Result<dualnic_core::ipc::EnvMatchReport, String>>,
    pub loading_env: bool,
    /// 当前正在编辑（预览）的方案名。与 active_profile 解耦：编辑归编辑，生效归生效。
    /// 空 = 尚未确定（profiles 加载后默认取激活方案）。
    pub editing_profile: String,
}

/// 编辑缓冲（UI 线程独有）。
struct EditBuf {
    lans: Vec<(String, String, Option<String>)>, // (cidr, note, 关联网卡 GUID)
    /// 选中为「外网」的物理卡（None=默认/不参与）。
    wan_if: Option<u32>,
    /// 选中为「内网」的物理卡（None=默认/不参与）。
    lan_if: Option<u32>,
    metric_lan: u32,
    metric_wan: u32,
    protected: Vec<String>,
    add_protected: String,
    new_cidr: String,
    /// 外网连通性探测的目标地址（空则用默认 www.apple.com）。
    probe_target: String,
}

pub struct ConfigEditor {
    shared: Arc<Mutex<WizardShared>>,
    buf: Option<EditBuf>,
    dirty: bool,
}

impl ConfigEditor {
    pub fn new() -> Self {
        ConfigEditor {
            shared: Arc::new(Mutex::new(WizardShared::default())),
            buf: None,
            dirty: false,
        }
    }

    /// 是否有未保存的修改（app 切页签/切方案前警告用）。
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// 丢弃未保存修改（用户确认丢弃后调用）；下次按当前激活配置重灌。
    pub fn discard_edits(&mut self) {
        self.buf = None;
        self.dirty = false;
        let mut s = self.shared.lock().unwrap();
        s.baseline = None;
        s.loading_baseline = false;
    }

    /// 切换编辑目标方案（预览）：丢弃未保存修改并按该方案重灌。
    pub fn set_editing_target(&mut self, name: String) {
        self.discard_edits();
        self.shared.lock().unwrap().editing_profile = name;
    }

    /// 由 app 在切进「配置向导」页签时调用：强制刷新网卡列表（USB 网卡随时插拔），
    /// 并把编辑缓冲/基线作废重灌——刷新后 if_index 与卡列表都可能变化，旧缓冲不可信。
    /// 连通性探测等网卡刷新完成后再跑（拉取完成处统一置 probe_wanted）。
    pub fn on_entered(&mut self) {
        {
            let mut s = self.shared.lock().unwrap();
            s.refresh_adapters_wanted = true;
            s.baseline = None;
            s.loading_baseline = false;
            s.probe_wanted = false; // 拿旧列表出结论没意义，等新列表到位再探测
        }
        self.buf = None;
        self.dirty = false;
    }

    /// 共享态句柄（app 方案栏读写 profiles / 触发编辑缓冲重置）。
    pub fn shared(&self) -> Arc<Mutex<WizardShared>> {
        self.shared.clone()
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, status_force: Arc<Mutex<PollState>>) {
        // 启用/切换成功后：只强制重拉 profiles（active 显示更新）。
        // 编辑缓冲不动 —— 编辑目标与激活方案已解耦（编辑归编辑，生效归生效）。
        {
            let mut s = self.shared.lock().unwrap();
            if s.profile_switch_done {
                s.profile_switch_done = false;
                s.profiles = Vec::new();
                s.loading_profiles = false;
            }
        }
        // 后台激活变化（自动匹配切了方案）：只刷新方案列表与 active 显示；编辑缓冲不动。
        {
            let status_active = status_force
                .lock().unwrap()
                .last.as_ref()
                .and_then(|o| o.as_ref().ok())
                .map(|st| st.active_profile.clone());
            if let Some(sa) = status_active {
                if !sa.is_empty() {
                    let mut s = self.shared.lock().unwrap();
                    if sa != s.active_profile && !s.loading_profiles {
                        s.profiles = Vec::new(); // 触发重拉 → active 显示更新
                    }
                }
            }
        }
        let ctx = ui.ctx().clone();
        ensure_data(ctx.clone(), self.shared.clone());
        run_probe_if_ready(ctx.clone(), self.shared.clone());

        // 底部固定消息栏要占住窗口内容区最下方：ScrollArea 不吃满剩余高度，预留两行给消息栏。
        const NOTICE_BAR_H: f32 = 40.0;
        let avail_h = ui.available_height();

        let (baseline, adapters, reachable, physical_guids, loading_b, loading_a, saving, probe_running, probe_outcome, editing_profile) = {
            let s = self.shared.lock().unwrap();
            (
                s.baseline.clone(),
                s.adapters.clone(),
                s.reachable.clone(),
                s.physical_guids.clone(),
                s.loading_baseline,
                s.loading_adapters,
                s.saving,
                s.probe_running,
                s.probe_outcome.clone(),
                s.editing_profile.clone(),
            )
        };
        let probe_ok: Vec<u32> = match &probe_outcome {
            Some(Ok(v)) => v.clone(),
            _ => Vec::new(),
        };

        egui::ScrollArea::vertical()
            .max_height((avail_h - NOTICE_BAR_H).max(160.0))
            .show(ui, |ui| {
            let mut just_reset = false;
            ui.horizontal(|ui| {
                ui.heading(egui::RichText::new(crate::i18n::tr(msgs::WIZ_PAGE_TITLE, &[])));
                ui.add_space(6.0);
                if ui.button(crate::i18n::tr(msgs::WIZ_RELOAD_BTN, &[])).clicked() {
                    self.buf = None;
                    self.dirty = false;
                    {
                        let mut s = self.shared.lock().unwrap();
                        // 只重置编辑相关状态；保留环境检测报告 —— 「无匹配方案」提示属于
                        // 自动匹配/方案操作语境，重新载入（纯编辑器刷新）不应带出它。
                        let env_report = s.env_report.take();
                        let loading_env = s.loading_env;
                        *s = WizardShared::default();
                        s.env_report = env_report;
                        s.loading_env = loading_env;
                    }
                    just_reset = true;
                    ctx.request_repaint();
                }
            });
            // 「默认」方案 = 内置「不接管」占位：只读，禁止编辑。
            if editing_profile == dualnic_core::config::DEFAULT_PROFILE_NAME {
                ui.add_space(4.0);
                ui.colored_label(eframe::egui::Color32::from_rgb(220, 170, 60),
                    crate::i18n::tr(msgs::WIZ_DEFAULT_HINT, &[]));
                ui.label(crate::i18n::tr(msgs::WIZ_DEFAULT_HINT2, &[]));
                return;
            }
            ui.add_space(2.0);
            ui.label(crate::i18n::tr(msgs::WIZ_PAGE_HINT, &[]));

            if just_reset {
                ui.label(crate::i18n::tr(msgs::WIZ_RELOADING, &[]));
                return;
            }
            match &baseline {
                Some(Err(msg)) => {
                    ui.colored_label(egui::Color32::from_rgb(200, 160, 60),
                        crate::i18n::tr(msgs::WIZ_BASELINE_FAIL, &[&msg]));
                    return;
                }
                None => {
                    ui.label(if loading_b { crate::i18n::tr(msgs::WIZ_BASELINE_READING, &[]) } else { crate::i18n::tr(msgs::WIZ_NO_BASELINE, &[]) });
                    return;
                }
                Some(Ok(cfg)) => {
                    // 等网卡枚举完成再建缓冲，才能从配置 matcher 反推选中的网卡（否则显示“默认”）
                    if self.buf.is_none() && !loading_a {
                        self.buf = Some(buf_from_config(cfg, &adapters));
                    }
                }
            }
            if self.buf.is_none() {
                ui.label(if loading_a { crate::i18n::tr(msgs::WIZ_ENUMING, &[]) } else { crate::i18n::tr(msgs::WIZ_PREPARING, &[]) });
                return;
            }
            let buf = self.buf.as_mut().expect("buffer exists");
            // 探测目标同步进共享态，供后台探测 worker 使用。
            self.shared.lock().unwrap().probe_target = buf.probe_target.clone();

            if adapters.is_empty() {
                ui.label(if loading_a { crate::i18n::tr(msgs::WIZ_ENUMING, &[]) } else { crate::i18n::tr(msgs::WIZ_NO_ADAPTERS, &[]) });
                return;
            }

            // 点「自动识别」后：等探测完成再把角色套用到两块主卡。
            {
                let apply = {
                    let mut s = self.shared.lock().unwrap();
                    // 仅当这次探测真的完成（有新鲜结果）才应用，避免用旧结果抢先。
                    if s.auto_apply_requested && !s.probe_running && s.probe_outcome.is_some() {
                        s.auto_apply_requested = false;
                        true
                    } else {
                        false
                    }
                };
                if apply {
                    apply_auto_roles(&physical_guids, buf, &adapters, &probe_ok, &reachable);
                    self.dirty = true;
                }
            }

            let (physical, virtual_assignable, others) = split_adapters(&physical_guids, &adapters);

            // ============ ① 快速设置 ============
            ui.separator();
            ui.label(egui::RichText::new(crate::i18n::tr(msgs::WIZ_SECTION_QUICK, &[])).size(18.0).strong());

            // 操作行：自动识别（含探测）/ 全部默认
            ui.horizontal(|ui| {
                let label = if probe_running { crate::i18n::tr(msgs::WIZ_AUTOMATCH_PROBING, &[]) } else { crate::i18n::tr(msgs::WIZ_AUTODETECT_BTN, &[]) };
                if ui
                    .add_enabled(!probe_running, egui::Button::new(egui::RichText::new(label).strong()))
                    .clicked()
                {
                    let mut s = self.shared.lock().unwrap();
                    // 点自动识别 = 强制重新探测（清旧结果，避免拿缓存直接出结论）
                    s.probe_outcome = None;
                    s.probe_wanted = true;
                    s.auto_apply_requested = true;
                }
                if ui.button(crate::i18n::tr(msgs::WIZ_RESET_ALL_BTN, &[])).clicked() {
                    buf.wan_if = None;
                    buf.lan_if = None;
                    self.dirty = true;
                }
            });
            if buf.wan_if.is_none() && buf.lan_if.is_none() {
                ui.colored_label(egui::Color32::from_rgb(120, 120, 120),
                    crate::i18n::tr(msgs::WIZ_NO_ROLE_NOTE, &[]));
            } else if buf.lan_if.is_none() {
                ui.colored_label(egui::Color32::from_rgb(200, 160, 60),
                    crate::i18n::tr(msgs::WIZ_WAN_ONLY_NOTE, &[]));
            }
            ui.add_space(2.0);

            // 四列角色表（物理网卡）
            if !physical.is_empty() {
                role_grid(ui, buf, &physical, &adapters, &reachable, &mut self.dirty);
            } else {
                ui.colored_label(egui::Color32::from_rgb(220, 150, 60),
                    crate::i18n::tr(msgs::WIZ_NO_PHYSICAL, &[]));
            }

            // 可采用的虚拟网卡（默认折叠，避免混入“物理网卡”误导）
            if !virtual_assignable.is_empty() {
                egui::CollapsingHeader::new(crate::i18n::tr(
                    msgs::WIZ_VIRTUAL_HDR,
                    &[&virtual_assignable.len()],
                ))
                .default_open(false)
                .show(ui, |ui| {
                    role_grid(ui, buf, &virtual_assignable, &adapters, &reachable, &mut self.dirty);
                });
            }

            if !buf.lans.is_empty() {
                let list = buf.lans.iter().map(|(c, _, _)| c.as_str()).collect::<Vec<_>>().join(", ");
                ui.label(crate::i18n::tr(msgs::WIZ_LAN_WILL_SAVE, &[&buf.lans.len(), &list]));
            }
            {
                let pv = |i: Option<u32>| -> String {
                    match i {
                        Some(x) => matcher_preview(&adapters, Some(x)),
                        None => crate::i18n::tr(msgs::WIZ_ROLE_NONE, &[]),
                    }
                };
                let w = pv(buf.wan_if);
                let l = pv(buf.lan_if);
                ui.label(crate::i18n::tr(msgs::WIZ_WILL_SAVE_AS, &[&w, &l]));
            }

            // 其它网卡折叠（未连接/无 IP/回环等仅展示）
            if !others.is_empty() {
                ui.add_space(4.0);
                egui::CollapsingHeader::new(crate::i18n::tr(msgs::WIZ_OTHERS_HDR, &[&others.len()]))
                    .default_open(false)
                    .show(ui, |ui| {
                        for a in &others {
                            ui.label(format!(
                                "{}{}",
                                adapter_name(a),
                                if a.oper_up {
                                    "".to_string()
                                } else {
                                    crate::i18n::tr(msgs::WIZ_DISCONNECTED, &[])
                                }
                            ));
                        }
                    });
            }

            // ============ ② 高级设置 ============
            ui.add_space(6.0);
            ui.separator();
            let mut add_notice: Option<Notice> = None;
            egui::CollapsingHeader::new(crate::i18n::tr(msgs::WIZ_SECTION_ADV, &[]))
                .default_open(false)
                .show(ui, |ui| {
                    add_notice = advanced_body(ui, buf, &physical_guids, &adapters, &reachable, &mut self.dirty);
                });
            if let Some(n) = add_notice {
                self.shared.lock().unwrap().notice = Some(n);
            }

            // ============ 校验 + 保存 ============
            ui.separator();
            let build_res = build_config(buf, &adapters);
            // 状态与保存按钮同行：编辑中无效只显示一条黄色警告（宽度变化不影响上方
            // 单选组布局，切换动画不被打断）；红色硬错误留给保存动作。
            ui.horizontal(|ui| {
                let saveable = build_res.is_ok() && !saving;
                if ui.add_enabled(saveable, egui::Button::new(egui::RichText::new(crate::i18n::tr(msgs::WIZ_SAVE_BTN, &[])).size(16.0))).clicked() {
                    if let Ok(cfg) = &build_res {
                        self.dirty = false;
                        spawn_save(ctx.clone(), self.shared.clone(), status_force, cfg.clone());
                    }
                }
                match &build_res {
                    Ok(_) => {
                        ui.colored_label(egui::Color32::from_rgb(60, 160, 90), crate::i18n::tr(msgs::WIZ_VALID_OK, &[]));
                    }
                    Err(errs) => {
                        let first = errs.first().cloned().unwrap_or_default();
                        ui.colored_label(egui::Color32::from_rgb(220, 170, 60),
                            crate::i18n::tr(msgs::WIZ_VALID_WARN, &[&first]));
                    }
                }
                if saving {
                    ui.spinner();
                    ui.label(crate::i18n::tr(msgs::WIZ_SAVING, &[]));
                }
            });
            ui.label(crate::i18n::tr(msgs::WIZ_SAVE_NOTE, &[]));
            let _ = self.dirty;
        });

        // ── 底部消息栏（固定在窗口内容区最下方）：最近一条消息，新消息直接覆盖旧消息 ──
        // 只收「事件类」消息（保存/探测/方案操作/网段编辑的结果）；持续状态提示（配置有效性、
        // 行内 CIDR 校验等）保留在各原位，避免互相覆盖丢失上下文。
        ui.separator();
        ui.horizontal(|ui| {
            ui.strong(crate::i18n::tr(msgs::WIZ_NOTICE_HDR, &[]));
            ui.separator();
            let notice = { self.shared.lock().unwrap().notice.clone() };
            match notice {
                Some(n) => {
                    ui.colored_label(n.color(), n.text);
                }
                None => {
                    ui.colored_label(egui::Color32::from_rgb(120, 120, 120), crate::i18n::tr(msgs::WIZ_NOTICE_EMPTY, &[]));
                }
            }
        });
    }
}

/// 四列角色表：每行 外网 / 内网 / 默认，末列网卡名。默认=不参与。
fn role_grid(
    ui: &mut egui::Ui,
    buf: &mut EditBuf,
    physical: &[&AdapterView],
    all: &[AdapterView],
    reachable: &[ReachableNet],
    dirty: &mut bool,
) {
    let _ = all;
    egui::Grid::new("quick_role")
        .num_columns(4)
        .striped(true)
        .spacing([26.0, 4.0])
        .show(ui, |ui| {
            ui.strong(crate::i18n::tr(msgs::WIZ_ROLE_WAN_HDR, &[]));
            ui.strong(crate::i18n::tr(msgs::WIZ_ROLE_LAN_HDR, &[]));
            ui.strong(crate::i18n::tr(msgs::WIZ_ROLE_DEFAULT_HDR, &[]));
            ui.strong(crate::i18n::tr(msgs::WIZ_ROLE_NIC_HDR, &[]));
            ui.end_row();
            for a in physical {
                let idx = a.if_index;
                let is_wan = buf.wan_if == Some(idx);
                let is_lan = buf.lan_if == Some(idx);

                // 行内角色互斥：一行只持一个角色。点 WAN → 该行原 LAN 让位；点 LAN → 原 WAN 让位。
                // 这样「内网改外网」一次点击完成，不会出现 WAN/LAN 同卡的中间冲突态。
                if ui.radio(is_wan, "").clicked() {
                    buf.wan_if = Some(idx);
                    if buf.lan_if == Some(idx) {
                        buf.lan_if = None;
                    }
                    *dirty = true;
                }
                if ui.radio(is_lan, "").clicked() {
                    buf.lan_if = Some(idx);
                    if buf.wan_if == Some(idx) {
                        buf.wan_if = None;
                    }
                    *dirty = true;
                }
                // 默认 = 本行不参与；点击即把本行从角色中清除。
                let is_default = !is_wan && !is_lan;
                if ui.radio(is_default, "").clicked() && !is_default {
                    if buf.wan_if == Some(idx) {
                        buf.wan_if = None;
                    }
                    if buf.lan_if == Some(idx) {
                        buf.lan_if = None;
                    }
                    *dirty = true;
                }
                ui.label(adapter_name(a));
                ui.end_row();
            }
        });

    let _ = reachable; // 单选只做选择；内网段由用户「采纳」或「自动识别」填充
}

/// 自动识别（显式一键动作）：按连通探测优先/网关回退设 wan_if/lan_if（只在已标记的物理卡里解析），
/// 并把 LAN 卡的 on-link 网段补进内网清单（只补缺，不删手工条目）。
fn apply_auto_roles(physical_guids: &[String], buf: &mut EditBuf, adapters: &[AdapterView], probe_ok: &[u32], reachable: &[ReachableNet]) {
    let (w, l) = resolved_roles(physical_guids, adapters, probe_ok);
    if let Some(w) = w {
        buf.wan_if = Some(w.if_index);
    }
    if let Some(l) = l {
        buf.lan_if = Some(l.if_index);
        fill_lans_from(buf, reachable, l.if_index, |n| format!("自动：{n}（{}）", adapter_name(l)));
    }
}

// ──────────────────────────── 高级设置主体 ────────────────────────────

/// 高级设置主体。返回值 = 本节产生的「消息」（网段添加结果），由调用方写入底部消息栏。
fn advanced_body(
    ui: &mut egui::Ui,
    buf: &mut EditBuf,
    physical_guids: &[String],
    adapters: &[AdapterView],
    reachable: &[ReachableNet],
    dirty: &mut bool,
) -> Option<Notice> {
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr(msgs::WIZ_PROBE_TARGET, &[]));
        ui.add(egui::TextEdit::singleline(&mut buf.probe_target)
            .hint_text("www.apple.com")
            .desired_width(220.0));
    });
    ui.label(crate::i18n::tr(msgs::WIZ_PROBE_NOTE, &[]));

    ui.separator();
    ui.label(crate::i18n::tr(msgs::WIZ_REACHABLE_HDR, &[]));
    if reachable.is_empty() {
        ui.label(crate::i18n::tr(msgs::WIZ_REACH_EMPTY, &[]));
    }
    let mut add: Option<String> = None;
    for r in reachable {
        let present = buf.lans.iter().any(|(c, _, _)| c == &r.prefix.to_string());
        let name = adapters.iter().find(|a| a.if_index == r.if_index).map(adapter_name)
            .unwrap_or_else(|| format!("if#{}", r.if_index));
        let src = match r.source {
            ReachSource::AdapterPrimary => crate::i18n::tr(msgs::WIZ_SRC_PRIMARY_SHORT, &[]),
            ReachSource::OnLinkRoute => "on-link".to_string(),
        };
        ui.horizontal(|ui| {
            ui.monospace(r.prefix.to_string());
            ui.label(name);
            ui.label(src);
            if present {
                ui.label(crate::i18n::tr(msgs::WIZ_ALREADY_IN_LIST, &[]));
            } else if ui.button(crate::i18n::tr(msgs::WIZ_ADOPT_BTN, &[])).clicked() {
                add = Some(r.prefix.to_string());
            }
        });
    }
    if let Some(c) = add {
        if !buf.lans.iter().any(|(x, _, _)| x == &c) {
            buf.lans.push((c.clone(), "手工采纳".into(), None)); // 采纳的可达网段 on-link，不绑定网卡
            *dirty = true;
        }
    }

    ui.separator();
    ui.strong(crate::i18n::tr(msgs::WIZ_LAN_MANUAL_HDR, &[]));
    let mut remove: Option<usize> = None;
    for (i, (cidr, note, via)) in buf.lans.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.label(format!("{}.", i + 1));
            ui.add(egui::TextEdit::singleline(cidr).hint_text("192.168.0.0/16").desired_width(150.0));
            ui.add(egui::TextEdit::singleline(note).hint_text(&crate::i18n::tr(msgs::WIZ_NOTE_HINT, &[])).desired_width(150.0));
            // 手工网段：绑定网卡 → 下一跳取该网卡 DHCP 服务器。
            egui::ComboBox::from_id_salt(("lan_via", i))
                .selected_text(via_name(adapters, via.as_deref()))
                .show_ui(ui, |ui| {
                    ui.selectable_value(via, None, &crate::i18n::tr(msgs::WIZ_VIA_AUTO, &[]));
                    for a in connected_phys(physical_guids, adapters) {
                        let guid = a.guid.clone().unwrap_or_default();
                        ui.selectable_value(via, Some(guid.clone()), adapter_name(a));
                    }
                });
            if ui.button(crate::i18n::tr(msgs::WIZ_DELETE_BTN, &[])).clicked() {
                remove = Some(i);
            }
        });
        if !cidr.trim().is_empty() && cidr.trim().parse::<Prefix>().is_err() {
            ui.colored_label(egui::Color32::from_rgb(230, 90, 70), crate::i18n::tr(msgs::WIZ_INVALID_CIDR, &[]));
        }
    }
    if let Some(i) = remove {
        buf.lans.remove(i);
        *dirty = true;
    }
    // 绑定网卡下拉依赖「已标记 + 已连接」的物理卡；为空时给出原因提示。
    if connected_phys(physical_guids, adapters).is_empty() {
        ui.colored_label(eframe::egui::Color32::from_rgb(220, 170, 60),
            crate::i18n::tr(msgs::WIZ_VIA_EMPTY_HINT, &[]));
    }
    // 「添加网段」的结果走底部消息栏（消息类）；行内红字仅保留逐行实时校验（状态类）。
    let mut add_notice: Option<Notice> = None;
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut buf.new_cidr).hint_text("10.0.0.0/8").desired_width(150.0));
        if ui.button(crate::i18n::tr(msgs::WIZ_ADD_BTN, &[])).clicked() {
            let c = buf.new_cidr.trim().to_string();
            if c.is_empty() {
                add_notice = Some(Notice::err(crate::i18n::tr(msgs::WIZ_ADD_EMPTY_ERR, &[])));
            } else if let Err(e) = c.parse::<Prefix>() {
                add_notice = Some(Notice::err(crate::i18n::tr(msgs::WIZ_ADD_INVALID_ERR, &[&e])));
            } else if buf.lans.iter().any(|(x, _, _)| *x == c) {
                add_notice = Some(Notice::warn(crate::i18n::tr(msgs::WIZ_ADD_DUP_ERR, &[&c])));
            } else {
                buf.lans.push((c, String::new(), None));
                buf.new_cidr.clear();
                *dirty = true;
                add_notice = Some(Notice::ok(crate::i18n::tr(msgs::WIZ_ADD_OK, &[])));
            }
        }
    });

    ui.separator();
    ui.strong(crate::i18n::tr(msgs::WIZ_METRIC_HDR, &[]));
    ui.horizontal(|ui| {
        ui.label("LAN metric");
        ui.add(egui::DragValue::new(&mut buf.metric_lan).range(1u32..=9999u32));
        ui.label("WAN metric");
        ui.add(egui::DragValue::new(&mut buf.metric_wan).range(1u32..=9999u32));
    });

    ui.separator();
    ui.strong(crate::i18n::tr(msgs::WIZ_RECON_HDR, &[]));
    // 对账开关不再由前台配置（产品语义）：方案启用（接管 WAN）后服务端强制开启双收敛，
    // 未接管 WAN 的方案自动关闭。这里只展示说明，不再提供复选框。
    ui.label(egui::RichText::new(crate::i18n::tr(msgs::WIZ_RECON_NOTE, &[])).weak());

    ui.label(crate::i18n::tr(msgs::WIZ_PROTECTED_HDR, &[]));
    for a in adapters {
        if let Some(g) = &a.guid {
            let mut on = buf.protected.iter().any(|p| normalize_guid(p) == normalize_guid(g));
            if ui.checkbox(&mut on, adapter_name(a)).changed() {
                if on {
                    if !buf.protected.iter().any(|p| normalize_guid(p) == normalize_guid(g)) {
                        buf.protected.push(g.clone());
                    }
                } else {
                    buf.protected.retain(|p| normalize_guid(p) != normalize_guid(g));
                }
                *dirty = true;
            }
        }
    }
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr(msgs::WIZ_PROTECTED_ADD_LABEL, &[]));
        ui.add(egui::TextEdit::singleline(&mut buf.add_protected).desired_width(260.0));
        if ui.button(crate::i18n::tr(msgs::WIZ_ADD_SHORT_BTN, &[])).clicked() {
            let g = buf.add_protected.trim().to_string();
            if !g.is_empty() && !buf.protected.iter().any(|p| normalize_guid(p) == normalize_guid(&g)) {
                buf.protected.push(g);
                buf.add_protected.clear();
                *dirty = true;
            }
        }
    });
    if !buf.protected.is_empty() {
        ui.label(crate::i18n::tr(msgs::WIZ_PROTECTED_COUNT, &[&buf.protected.len()]));
    }
    add_notice
}

// ──────────────────────────── 网卡分组 / 自动角色 / 填网段 ────────────────────────────

/// 分组：physical=用户标记的物理卡（**不论是否连接**——未连接也可编辑/选角色，界面标注状态）；
/// virtual_assignable=未标记但 up+IPv4 非 APIPA 非回环的虚拟/隧道卡（easetun/vEthernet，可手动采纳为角色）；
/// rest=未连接的未标记卡/无 IP/APIPA/回环/隧道等仅展示。
fn split_adapters<'a>(
    physical_guids: &[String],
    adapters: &'a [AdapterView],
) -> (Vec<&'a AdapterView>, Vec<&'a AdapterView>, Vec<&'a AdapterView>) {
    let mut physical = Vec::new();
    let mut vcand = Vec::new();
    let mut rest = Vec::new();
    for a in adapters {
        let marked = dualnic_core::env::is_physical_adapter(physical_guids, a);
        let loopback = a.if_type == Some(24)
            || [a.alias.as_deref(), a.description.as_deref()]
                .iter()
                .flatten()
                .any(|s| s.to_lowercase().contains("loopback"));
        if marked && !loopback {
            // 已标记的物理卡：无论连接与否都可编辑/选角色
            physical.push(a);
        } else {
            let ip_ok = a
                .primary_ipv4
                .is_some_and(|ip| !(ip.octets()[0] == 169 && ip.octets()[1] == 254));
            if a.oper_up && ip_ok && !loopback {
                vcand.push(a);
            } else {
                rest.push(a);
            }
        }
    }
    (physical, vcand, rest)
}

/// 已连接物理候选（被标记为物理 + oper_up + 有 IPv4、非 APIPA）。
fn connected_phys<'a>(physical_guids: &[String], adapters: &'a [AdapterView]) -> Vec<&'a AdapterView> {
    adapters.iter().filter(|a| {
        dualnic_core::env::is_physical_adapter(physical_guids, a)
            && a.oper_up
            && a.primary_ipv4.is_some_and(|ip| !(ip.octets()[0] == 169 && ip.octets()[1] == 254))
    }).collect()
}

/// 角色解析：WAN = 连通可达（多个则优先持默认网关）；探测为空回退网关；LAN = 另一块已连接物理卡。
fn resolved_roles<'a>(physical_guids: &[String], adapters: &'a [AdapterView], probe_ok: &[u32]) -> (Option<&'a AdapterView>, Option<&'a AdapterView>) {
    let connected = connected_phys(physical_guids, adapters);
    let reachable: Vec<&AdapterView> = connected.iter().copied()
        .filter(|a| probe_ok.contains(&a.if_index)).collect();
    let wan = if reachable.is_empty() {
        connected.iter().copied().find(|a| a.gateway_ipv4.is_some())
    } else {
        reachable.iter().copied().find(|a| a.gateway_ipv4.is_some())
            .or_else(|| reachable.first().copied())
    };
    let lan = connected.iter().copied().find(|a| Some(a.if_index) != wan.map(|w| w.if_index));
    (wan, lan)
}

fn fill_lans_from(buf: &mut EditBuf, reachable: &[ReachableNet], lan_if: u32, note: impl Fn(&str) -> String) -> usize {
    let mut added = 0;
    let mut picked = reachable.iter().filter(|r| r.if_index == lan_if).collect::<Vec<_>>();
    picked.sort_by_key(|r| std::cmp::Reverse(r.prefix.len));
    for r in picked {
        let c = r.prefix.to_string();
        if !buf.lans.iter().any(|(x, _, _)| x == &c) {
            buf.lans.push((c.clone(), note(&c), None)); // 自动填充的内网段 on-link，不绑定网卡
            added += 1;
        }
    }
    added
}

// ──────────────────────────── 后台取数/探测/保存 ────────────────────────────

fn ensure_data(ctx: egui::Context, shared: Arc<Mutex<WizardShared>>) {
    let (need_cfg, need_adapters, need_profiles, editing) = {
        let mut s = shared.lock().unwrap();
        // 编辑目标未定时等 profiles 加载后再定（默认=激活方案），不盲目拉基线。
        let need_cfg = !s.editing_profile.is_empty() && s.baseline.is_none() && !s.loading_baseline;
        if need_cfg { s.loading_baseline = true; }
        // 进页签强制刷新（refresh_adapters_wanted）或列表为空 → 拉网卡（不空也拉，覆盖 USB 插拔）。
        let force_adapters = s.refresh_adapters_wanted && !s.loading_adapters;
        if force_adapters { s.refresh_adapters_wanted = false; }
        let need_adapters = force_adapters || (s.adapters.is_empty() && !s.loading_adapters);
        if need_adapters { s.loading_adapters = true; }
        // 仅当「列表为空且当前没有在拉取」时才拉，避免 loading 状态导致每帧重复 spawn 线程卡死。
        let need_profiles = s.profiles.is_empty() && !s.loading_profiles;
        if need_profiles { s.loading_profiles = true; }
        (need_cfg, need_adapters, need_profiles, s.editing_profile.clone())
    };
    if need_cfg {
        let ctx_a = ctx.clone();
        let sa = shared.clone();
        std::thread::spawn(move || {
            let r = client::get_profile_config(&editing).map_err(|f| f.message);
            let mut s = sa.lock().unwrap();
            s.baseline = Some(r);
            s.loading_baseline = false;
            ctx_a.request_repaint();
        });
    }
    if need_adapters {
        let ctx_b = ctx.clone();
        let sb = shared.clone();
        std::thread::spawn(move || {
            let r = client::get_snapshot();
            let (adapters, reachable) = match r {
                Ok(snap) => (snap.report.adapters, snap.report.reachable),
                Err(_) => (Vec::new(), Vec::new()),
            };
            let physical = client::get_physical_nics().unwrap_or_default();
            let mut s = sb.lock().unwrap();
            s.adapters = adapters;
            s.reachable = reachable;
            s.physical_guids = physical;
            s.loading_adapters = false;
            // 网卡列表已就绪 → 允许跑连通性探测（进页签时被推迟到此刻，保证基于新列表）。
            s.probe_wanted = true;
            ctx_b.request_repaint();
        });
    }
    if need_profiles {
        let ctx_p = ctx.clone();
        let sp = shared.clone();
        std::thread::spawn(move || {
            let r = client::list_profiles();
            let mut s = sp.lock().unwrap();
            match r {
                Ok(d) => {
                    s.profiles = d.profiles;
                    s.active_profile = d.active;
                    // 编辑目标未定（首启/重载）→ 默认编辑激活方案；清基线触发按该方案拉取。
                    if s.editing_profile.is_empty() && !s.active_profile.is_empty() {
                        s.editing_profile = s.active_profile.clone();
                        s.baseline = None;
                        s.loading_baseline = false;
                    }
                    s.loading_profiles = false;
                }
                Err(_) => {
                    s.loading_profiles = false;
                }
            }
            ctx_p.request_repaint();
        });
    }
    // 环境检测（只读，方案栏环境状态行用）
    {
        let need_env = {
            let mut s = shared.lock().unwrap();
            if !s.loading_env && s.env_report.is_none() {
                s.loading_env = true;
                true
            } else {
                false
            }
        };
        if need_env {
            let ctx_e = ctx.clone();
            let se = shared.clone();
            std::thread::spawn(move || {
                let r = client::detect_environment().map_err(|f| f.message);
                let mut s = se.lock().unwrap();
                s.env_report = Some(r);
                s.loading_env = false;
                ctx_e.request_repaint();
            });
        }
    }
}

/// 若有“探测请求”且候选就绪 → 起一轮连通性探测（幂等护栏）。
fn run_probe_if_ready(ctx: egui::Context, shared: Arc<Mutex<WizardShared>>) {
    let (candidates, host) = {
        let mut s = shared.lock().unwrap();
        if !s.probe_wanted || s.probe_running || s.adapters.is_empty() {
            return;
        }
        s.probe_wanted = false;
        s.probe_running = true;
        let mut host = s.probe_target.clone();
        if host.trim().is_empty() {
            host = crate::probe::DEFAULT_TARGET.to_string();
        }
        (s.adapters.clone(), host)
    };
    let ctx2 = ctx.clone();
    let sh = shared.clone();
    std::thread::spawn(move || {
        let res = crate::probe::reachable_wans(&candidates, &host);
        let mut s = sh.lock().unwrap();
        s.probe_running = false;
        // 底部消息栏：探测结果（新消息覆盖旧消息）
        if res.is_empty() {
            s.notice = Some(Notice::warn(crate::i18n::tr(msgs::WIZ_PROBE_FAIL_FALLBACK, &[&host])));
        } else {
            let names: Vec<String> = res
                .iter()
                .filter_map(|idx| candidates.iter().find(|a| a.if_index == *idx).map(adapter_name))
                .collect();
            let names_str = if names.is_empty() {
                crate::i18n::tr(msgs::WIZ_PROBE_NAMES_UNK, &[])
            } else {
                names.join("、")
            };
            s.notice = Some(Notice::ok(crate::i18n::tr(msgs::WIZ_PROBE_OK, &[&host, &names_str])));
        }
        s.probe_outcome = Some(Ok(res));
        ctx2.request_repaint();
    });
}

/// 后台保存：握手 → set_profile_config（只持久化编辑目标方案，**不触发对账**；生效走「启用」）。
/// 失败(未授权)清 token。
fn spawn_save(ctx: egui::Context, shared: Arc<Mutex<WizardShared>>, status_force: Arc<Mutex<PollState>>, cfg: PolicyConfig) {
    let editing = shared.lock().unwrap().editing_profile.clone();
    {
        let mut s = shared.lock().unwrap();
        if s.saving { return; }
        s.saving = true;
    }
    std::thread::spawn(move || {
        // 每次保存都取新一次性令牌（服务端消费一次即失效，不能缓存复用）
        let token = match client::handshake() {
            Ok(t) => { shared.lock().unwrap().token = Some(t.clone()); t }
            Err(e) => { finish_save(&shared, Err(crate::i18n::tr(msgs::WIZ_HANDSHAKE_FAIL, &[&e]))); ctx.request_repaint(); return; }
        };
        let result = client::set_profile_config(&token, &editing, &cfg);
        match result {
            Ok(()) => {
                finish_save(&shared, Ok(()));
                status_force.lock().unwrap().force = true;
                ctx.request_repaint();
            }
            Err(f) => {
                let mut clear_token = false;
                let msg = if f.code == dualnic_core::ipc::error_code::UNAUTHORIZED {
                    clear_token = true;
                    crate::i18n::tr(msgs::WIZ_REHANDSHAKE, &[&f.message])
                } else if f.code == "bad_request" {
                    crate::i18n::tr(msgs::WIZ_VERSION_OLD, &[])
                } else {
                    f.message
                };
                if clear_token { shared.lock().unwrap().token = None; }
                finish_save(&shared, Err(msg));
                ctx.request_repaint();
            }
        }
    });
}

fn finish_save(shared: &Arc<Mutex<WizardShared>>, r: Result<(), String>) {
    let mut s = shared.lock().unwrap();
    s.saving = false;
    // 底部消息栏：保存结果（新消息覆盖旧消息）
    s.notice = Some(match &r {
        Ok(()) => Notice::ok(crate::i18n::tr(msgs::WIZ_SAVED_OK, &[])),
        Err(msg) => Notice::err(crate::i18n::tr(msgs::WIZ_SAVE_FAIL, &[&msg])),
    });
}

// ──────────────────────────── 编辑缓冲 ↔ PolicyConfig ────────────────────────────

fn buf_from_config(cfg: &PolicyConfig, adapters: &[AdapterView]) -> EditBuf {
    EditBuf {
        lans: cfg.lan_networks.iter()
            .map(|n| (n.cidr.to_string(), n.note.clone().unwrap_or_default(), n.via_iface_guid.clone()))
            .collect(),
        // 从配置的 matcher 反推选中的网卡（否则切方案后显示“默认”，且保存会误清空角色）
        wan_if: resolve_matcher_if(&cfg.wan_adapter, adapters),
        lan_if: resolve_matcher_if(&cfg.lan_adapter, adapters),
        metric_lan: cfg.interface_metric.lan,
        metric_wan: cfg.interface_metric.wan,
        protected: cfg.reconciliation.protected_interfaces.clone(),
        add_protected: String::new(),
        new_cidr: String::new(),
        probe_target: cfg.probe_target.clone(),
    }
}

/// 用某个角色的匹配规则在真实网卡里反推 if_index（命中即返回）。
fn resolve_matcher_if(rm: &RoleMatcher, adapters: &[AdapterView]) -> Option<u32> {
    adapters.iter().find(|a| {
        let probe = AdapterProbe {
            guid: a.guid.clone(),
            description: a.description.clone(),
            name: a.alias.clone(),
        };
        rm.matches(&probe)
    }).map(|a| a.if_index)
}

fn build_config(buf: &EditBuf, adapters: &[AdapterView]) -> Result<PolicyConfig, Vec<String>> {
    let mut errs: Vec<String> = Vec::new();
    let mut cfg = PolicyConfig::default();
    cfg.schema_version = SCHEMA_VERSION;

    let mut seen = std::collections::HashSet::new();
    for (i, (cidr, note, via)) in buf.lans.iter().enumerate() {
        let c = cidr.trim();
        if c.is_empty() { continue; }
        match c.parse::<Prefix>() {
            Ok(p) if p.len == 0 => errs.push(crate::i18n::tr(msgs::WIZ_ERR_ZERO_CIDR, &[&(i + 1)])),
            Ok(_) if !seen.insert(c.to_string()) => errs.push(crate::i18n::tr(msgs::WIZ_ERR_DUP, &[&(i + 1), &c])),
            Ok(p) => {
                // 校验：选了网卡则必须能在当前枚举列表里匹配到（否则对账无从取 DHCP 服务器）。
                if let Some(g) = via {
                    if !adapters.iter().any(|a| a.guid.as_deref().is_some_and(|x| {
                        normalize_guid(x) == normalize_guid(g)
                    })) {
                        errs.push(crate::i18n::tr(msgs::WIZ_ERR_NIC_NOT_FOUND, &[&(i + 1)]));
                        continue;
                    }
                }
                let note_owned = note.trim().to_string();
                let via_owned = via.clone();
                cfg.lan_networks.push(LanNetwork::with_via(
                    p,
                    if note_owned.is_empty() { None } else { Some(&note_owned) },
                    via_owned,
                ));
            }
            Err(e) => errs.push(crate::i18n::tr(msgs::WIZ_ERR_LINE, &[&(i + 1), &e])),
        }
    }

    // 角色：None=该侧不管理（可全默认保存=纯观测；也可只配其中一侧）
    match buf.wan_if {
        None => {} // 不管理外网默认路由
        Some(i) => match role_matcher(adapters, Some(i)) {
            Some(m) => cfg.wan_adapter = RoleMatcher::new(AdapterRole::Wan).with_matcher(m),
            None => errs.push(crate::i18n::tr(msgs::WIZ_ERR_WAN_DESC, &[])),
        },
    }
    match buf.lan_if {
        // 内网卡未选 = 该侧不管理；内网网段照常保存（提示“暂不生效”，等勾了内网卡再用）。
        None => {}
        Some(i) => match role_matcher(adapters, Some(i)) {
            Some(m) => cfg.lan_adapter = RoleMatcher::new(AdapterRole::Lan).with_matcher(m),
            None => errs.push(crate::i18n::tr(msgs::WIZ_ERR_LAN_DESC, &[])),
        },
    }

    cfg.interface_metric.lan = buf.metric_lan;
    cfg.interface_metric.wan = buf.metric_wan;
    let t = buf.probe_target.trim();
    cfg.probe_target = if t.is_empty() { crate::probe::DEFAULT_TARGET.to_string() } else { t.to_string() };
    cfg.reconciliation.protected_interfaces = buf.protected.clone();
    // 对账开关前台不可配：按「是否接管 WAN」归一（服务端写入口还会再兜底一次）。
    dualnic_core::config::apply_reconciliation_policy(&mut cfg);

    if errs.is_empty() {
        if let Err(e) = cfg.validate() {
            errs.push(e.to_string());
        }
    }
    if errs.is_empty() { Ok(cfg) } else { Err(errs) }
}

fn role_matcher(adapters: &[AdapterView], if_index: Option<u32>) -> Option<AdapterMatcher> {
    let a = adapters.iter().find(|a| Some(a.if_index) == if_index)?;
    if let Some(g) = &a.guid {
        return Some(AdapterMatcher::Guid(g.clone()));
    }
    if let Some(d) = &a.description {
        if !d.is_empty() {
            return Some(AdapterMatcher::DescContains(d.clone()));
        }
    }
    a.alias.clone().map(AdapterMatcher::NameEq)
}

fn matcher_preview(adapters: &[AdapterView], sel: Option<u32>) -> String {
    match role_matcher(adapters, sel) {
        Some(AdapterMatcher::Guid(g)) => format!("guid={g}"),
        Some(AdapterMatcher::DescContains(d)) => format!("desc~{d}"),
        Some(AdapterMatcher::NameEq(n)) => format!("name={n}"),
        None => crate::i18n::tr(msgs::WIZ_PV_UNSET, &[]),
    }
}

fn adapter_name(a: &AdapterView) -> String {
    let base = a.alias.clone().unwrap_or_else(|| "—".to_string());
    match &a.description {
        Some(d) if !d.is_empty() => format!("{base}（{d}）"),
        _ => base,
    }
}

/// 手工网段「指定网卡」下拉的当前文案。
fn via_name(adapters: &[AdapterView], via: Option<&str>) -> String {
    match via {
        None => crate::i18n::tr(msgs::WIZ_VIA_AUTO, &[]),
        Some(g) => adapters
            .iter()
            .find(|a| a.guid.as_deref().is_some_and(|x| normalize_guid(x) == normalize_guid(g)))
            .map(adapter_name)
            .unwrap_or_else(|| crate::i18n::tr(msgs::WIZ_VIA_UNENUMERATED, &[])),
    }
}
