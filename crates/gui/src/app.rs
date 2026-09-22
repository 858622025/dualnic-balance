//! egui 主窗口：顶部分段切换「状态 / 风险」两视图。
//! 数据都来自同一后台轮询线程（见 `state`），每 2s 刷新、天然热重连。
//!
//! 注：eframe 0.36 的 `App` trait 以 `fn ui(&mut self, ui: &mut Ui, …)` 为必选入口。

use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dualnic_core::diff::{DiffAction, DiffKind, DiffReport, ResolvedCard};
use dualnic_core::ipc::{DiagnoseData, EventsData, SnapshotData, StatusData};
use dualnic_core::lpm::{decide_lpm, LpmDecision};

use crate::client;
use crate::msgs;
use crate::state::{
    unix_now_secs, DiagnoseState, EventsState, PollState, SettingsState, SnapshotState,
};
use crate::wizard::{ConfigEditor, Notice, WizardShared};

/// 顶部视图。
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Status,
    Risk,
    Snapshot,
    Config,
    PhysicalNics,
    Diagnose,
    Events,
    Settings,
}

/// 物理网卡标记页的共享状态。
#[derive(Default)]
struct PhysicalNicsState {
    /// 网卡列表（GetSnapshot）。
    adapters: Vec<dualnic_core::diff::AdapterView>,
    /// 服务端已保存的物理卡 GUID 集合（归一化）。
    saved_guids: Vec<String>,
    /// 用户在 UI 上的勾选（编辑中的 GUID 集合）。
    checked: std::collections::HashSet<String>,
    /// 是否已从服务加载过（防止重复弹首启引导）。
    loaded: bool,
    /// 是否正在拉取。
    loading: bool,
    /// 是否正在保存。
    saving: bool,
    /// 保存结果。
    outcome: Option<Result<(), String>>,
}

pub struct DualNicApp {
    shared: Arc<Mutex<PollState>>,
    snap: Arc<Mutex<SnapshotState>>,
    editor: ConfigEditor,
    view: View,
    /// 记录上一帧是否在快照/策略预览页，用于“切回该页即刷新”。
    snap_was_active: bool,
    /// 记录上一帧是否在配置向导页，用于“切进该页即跑一轮连通性探测”。
    config_was_active: bool,
    /// 「网卡标记」页签是否处于激活态（进页签瞬间强制刷新网卡列表）。
    physical_was_active: bool,
    /// 「诊断」页签是否处于激活态（每次进页签自动「立即诊断」一次）。
    diag_was_active: bool,
    /// 待确认的切换目标（ComboBox 选中 ≠ active 时置位，确认框弹出）。
    pending_switch: Option<String>,
    /// 有未保存修改时，待切换的编辑目标方案（确认丢弃后才真正切换编辑目标）。
    pending_edit: Option<String>,
    /// 配置向导有未保存修改时，被拦截的目标页签（确认丢弃后才真正切走）。
    pending_view: Option<View>,
    /// 待重命名的方案（旧名）。
    rename_target: Option<String>,
    /// 重命名的新名输入缓冲（内联编辑，持久化）。
    rename_input: String,
    /// 待确认删除的方案名（编辑目标；确认后立即从服务端移除）。
    pending_delete: Option<String>,
    /// 对账/回滚结果展示（后台线程写、UI 读）。
    reconcile_outcome: Arc<Mutex<Option<Result<String, String>>>>,
    /// 规则预览：输入 IP 查询的缓冲（LPM 可视化）。
    lpm_query: String,
    /// 诊断页数据（独立单发，不进轮询）。
    diag: Arc<Mutex<DiagnoseState>>,
    /// 事件日志页数据（独立单发，不进轮询）。
    events: Arc<Mutex<EventsState>>,
    /// 待确认的「清空事件日志」操作（确认框置位）。
    pending_clear_events: bool,
    /// 设置页数据（服务注册/自启动）。
    settings: Arc<Mutex<SettingsState>>,
    /// 待确认的「卸载服务」操作（确认框置位）。
    pending_uninstall_svc: bool,
    /// 首启后台引擎横幅（None = 无事发生）。
    backend_banner: Arc<Mutex<Option<Result<String, String>>>>,
    /// 设置页轮询节流（进入页面后每 5 秒自动刷新一次状态）。
    last_settings_poll: Option<std::time::Instant>,
    /// 配置导入/导出的结果消息（展示在按钮旁）。
    config_io_msg: Option<String>,
    /// 物理网卡标记页：适配器列表（GetSnapshot 拉取）+ 用户勾选的物理卡 GUID 集合。
    physical_state: Arc<Mutex<PhysicalNicsState>>,
}

impl DualNicApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_fonts(&cc.egui_ctx);
        // 锁死深色模式（egui 默认 System 跟随系统）：Windows 切浅色时软件只有部分元素变浅，观感割裂。
        // 显式 Dark 后系统 ThemeChanged 事件不再影响配色。
        cc.egui_ctx.set_theme(eframe::egui::ThemePreference::Dark);
        crate::tray::set_egui_ctx(cc.egui_ctx.clone());
        let shared = Arc::new(Mutex::new(PollState::default()));
        crate::state::spawn_poll_thread(
            cc.egui_ctx.clone(),
            shared.clone(),
            Duration::from_secs(2),
        );
        let app_state = DualNicApp {
            shared,
            snap: Arc::new(Mutex::new(SnapshotState::default())),
            editor: ConfigEditor::new(),
            view: View::Status,
            snap_was_active: false,
            config_was_active: false,
            physical_was_active: false,
            diag_was_active: false,
            pending_switch: None,
            pending_edit: None,
            pending_view: None,
            rename_target: None,
            rename_input: String::new(),
            pending_delete: None,
            reconcile_outcome: Arc::new(Mutex::new(None)),
            lpm_query: String::new(),
            diag: Arc::new(Mutex::new(DiagnoseState::default())),
            events: Arc::new(Mutex::new(EventsState::default())),
            pending_clear_events: false,
            settings: Arc::new(Mutex::new(SettingsState::default())),
            pending_uninstall_svc: false,
            backend_banner: crate::state::ensure_backend_async(cc.egui_ctx.clone()),
            last_settings_poll: None,
            config_io_msg: None,
            physical_state: Arc::new(Mutex::new(PhysicalNicsState::default())),
        };
        // 启动即预加载物理标记数据：首启引导判定不依赖用户先点进「网卡标记」页。
        gui_spawn_physical_load(cc.egui_ctx.clone(), app_state.physical_state.clone());
        app_state
    }
}

impl eframe::App for DualNicApp {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        // 拦截关闭：点 X 时若用户未确认退出（托盘菜单未点「退出」），只隐藏到托盘、不销毁进程。
        let close_req = ui.ctx().input(|i| i.viewport().close_requested());
        if close_req {
            if crate::tray::quit_wanted() {
                // 用户确认退出（托盘「退出」）：放行，直接结束进程。
                std::process::exit(0);
            } else {
                // 只是点 X：取消关闭，隐藏到托盘。
                ui.ctx().send_viewport_cmd(eframe::egui::ViewportCommand::CancelClose);
                ui.ctx().send_viewport_cmd(eframe::egui::ViewportCommand::Visible(false));
            }
        }

        // 快照克隆后释放锁再绘制，避免阻塞轮询线程。
        let snapshot = self.shared.lock().unwrap().clone();

        // 设置页轮询：停留期间每 5 秒自动刷新服务/自启动状态（动作执行中跳过）。
        if self.view == View::Settings {
            let due = self
                .last_settings_poll
                .map(|t| t.elapsed() >= std::time::Duration::from_secs(5))
                .unwrap_or(true);
            let busy = self.settings.lock().unwrap().busy.is_some();
            if due && !busy {
                self.last_settings_poll = Some(std::time::Instant::now());
                crate::state::load_settings_async(ui.ctx().clone(), self.settings.clone());
            }
        }

        let prev_view = self.view;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.view, View::Status, crate::i18n::tr(msgs::CMN_TAB_STATUS, &[]));
            ui.selectable_value(&mut self.view, View::Risk, crate::i18n::tr(msgs::CMN_TAB_RISK, &[]));
            ui.selectable_value(&mut self.view, View::Snapshot, crate::i18n::tr(msgs::CMN_TAB_SNAPSHOT, &[]));
            ui.selectable_value(&mut self.view, View::Config, crate::i18n::tr(msgs::CMN_TAB_CONFIG, &[]));
            ui.selectable_value(&mut self.view, View::PhysicalNics, crate::i18n::tr(msgs::CMN_TAB_PHY, &[]));
            ui.selectable_value(&mut self.view, View::Diagnose, crate::i18n::tr(msgs::CMN_TAB_DIAG, &[]));
            ui.selectable_value(&mut self.view, View::Events, crate::i18n::tr(msgs::CMN_TAB_EVENTS, &[]));
            ui.selectable_value(&mut self.view, View::Settings, crate::i18n::tr(msgs::CMN_TAB_SETTINGS, &[]));
        });
        // 配置向导有未保存修改时拦截切页签：回退到配置页，弹确认（丢弃/留下）。
        if self.view != prev_view && prev_view == View::Config && self.editor.is_dirty() {
            self.pending_view = Some(self.view);
            self.view = prev_view;
        }
        ui.separator();

        // —— 常驻底栏（所有页签可见）：「正在编辑 / 当前生效」是全局状态，风险/策略预览页
        //    也需要知道当前生效方案。配置页从上到下 = 内容 → 消息栏 → 本栏（在「消息」之下）。
        //    实现：把剩余空间显式切成「页面区 + 底栏区」两个子 Ui（egui 0.36 的 Panel 在
        //    中央面板嵌套 Ui 里不生效，故不用 Panel）。 ——
        {
            let wizard_shared = self.editor.shared();
            let (editing_wiz, active_wiz) = {
                let s = wizard_shared.lock().unwrap();
                (s.editing_profile.clone(), s.active_profile.clone())
            };
            // 兜底：方案数据只在进过「配置向导」后才加载；未进过时用全局状态轮询里的
            // active_profile（每 2s 刷新，跨页可用）。编辑目标默认=当前生效（与向导逻辑一致）。
            let status_active = snapshot
                .last
                .as_ref()
                .and_then(|o| o.as_ref().ok())
                .map(|st| st.active_profile.clone())
                .unwrap_or_default();
            let active = if active_wiz.is_empty() { status_active } else { active_wiz };
            let editing = if editing_wiz.is_empty() { active.clone() } else { editing_wiz };
            const FOOTER_H: f32 = 30.0;
            let avail = ui.available_rect_before_wrap();
            let footer_top = (avail.max.y - FOOTER_H).max(avail.min.y);
            let page_rect =
                eframe::egui::Rect::from_min_max(avail.min, eframe::egui::pos2(avail.max.x, footer_top));
            let footer_rect =
                eframe::egui::Rect::from_min_max(eframe::egui::pos2(avail.min.x, footer_top), avail.max);

            // —— 页面内容区（底部让出底栏高度） ——
            let mut page_ui = ui
                .new_child(eframe::egui::UiBuilder::new().max_rect(page_rect).id_salt("page_area"));

            let now_active = self.view == View::Snapshot;
            let entering_snapshot = now_active && !self.snap_was_active;
            if entering_snapshot {
                // 切回本页即刷新一次快照（热重载后立刻看到新策略）。
                crate::state::refresh_snapshot_async(page_ui.ctx().clone(), self.snap.clone());
            }
            let config_now = self.view == View::Config;
            if config_now && !self.config_was_active {
                // 切进配置向导：强制刷新网卡列表（USB 网卡随时插拔）+ 重灌编辑缓冲，
                // 刷新完成后再跑一轮“外网网卡”连通性探测（见 wizard::on_entered）。
                self.editor.on_entered();
            }
            let physical_now = self.view == View::PhysicalNics;
            if physical_now && !self.physical_was_active {
                // 切进网卡标记：强制重新枚举网卡（USB 网卡随时插拔，缓存不可信）。
                gui_spawn_physical_load(page_ui.ctx().clone(), self.physical_state.clone());
            }
            let diag_now = self.view == View::Diagnose;
            if diag_now && !self.diag_was_active {
                // 每次进诊断页自动「立即诊断」一次（等同点按钮；in_flight 幂等，连切不并发）。
                crate::state::refresh_diagnose_async(page_ui.ctx().clone(), self.diag.clone());
            }
            match self.view {
                View::Status => self.render_status(&mut page_ui, &snapshot),
                View::Risk => self.render_risk(&mut page_ui, &snapshot),
                View::Snapshot => self.render_snapshot(&mut page_ui, &snapshot),
                View::Config => {
                    // 顶部「配置方案(Profile)」栏（跨页可用；此处只对配置页展示操作）
                    self.render_profile_bar(&mut page_ui, &snapshot);
                    self.render_config_io(&mut page_ui);
                    self.editor.ui(&mut page_ui, self.shared.clone());
                }
                View::PhysicalNics => self.render_physical_nics(&mut page_ui),
                View::Diagnose => self.render_diagnose(&mut page_ui, &snapshot),
                View::Settings => self.render_settings(&mut page_ui),
                View::Events => self.render_events(&mut page_ui),
            }
            self.snap_was_active = now_active;
            self.config_was_active = config_now;
            self.physical_was_active = physical_now;
            self.diag_was_active = diag_now;

            // —— 底栏 ——
            let mut footer_ui = ui.new_child(
                eframe::egui::UiBuilder::new().max_rect(footer_rect).id_salt("edit_active_footer"),
            );
            footer_ui.separator();
            footer_ui.horizontal(|ui| {
                // 编辑 / 生效 双状态：编辑目标可不同于激活方案；改动保存后要点「启用」才作用路由。
                // 首批接入消息目录的文案（DNCMN-001/002）；空名 = 未载入，直接原样显示。
                let edit_disp = if editing.is_empty() { crate::i18n::tr(msgs::CMN_LOADING_SHORT, &[]) } else { editing.clone() };
                let active_disp = if active.is_empty() { crate::i18n::tr(msgs::CMN_LOADING_SHORT, &[]) } else { active.clone() };
                ui.label(crate::i18n::tr(msgs::CMN_FOOTER_EDITING, &[&edit_disp]));
                ui.add_space(10.0);
                ui.label(crate::i18n::tr(msgs::CMN_FOOTER_ACTIVE, &[&active_disp]));
                if !editing.is_empty() && editing != active {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(220, 170, 60),
                        crate::i18n::tr(msgs::CMN_FOOTER_WARN, &[]),
                    );
                }
            });
        }
        self.render_onboarding(ui);
        self.render_pending_view_confirm(ui);
    }
}

impl DualNicApp {
    /// 未保存修改时被拦截的切页签确认：丢弃并切换 / 留在本页保存。
    fn render_pending_view_confirm(&mut self, ui: &mut eframe::egui::Ui) {
        let Some(target) = self.pending_view else { return };
        let mut discard = false;
        eframe::egui::Window::new(crate::i18n::tr(msgs::WIZ_UNSAVED_TITLE, &[]))
            .collapsible(false)
            .resizable(false)
            .anchor(eframe::egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.label(crate::i18n::tr(msgs::WIZ_LEAVE_CONFIRM, &[]));
                ui.colored_label(
                    eframe::egui::Color32::from_rgb(230, 90, 70),
                    crate::i18n::tr(msgs::WIZ_LEAVE_DETAIL, &[]),
                );
                ui.separator();
                if ui.button(crate::i18n::tr(msgs::WIZ_LEAVE_DISCARD, &[])).clicked() {
                    discard = true;
                }
                if ui.button(crate::i18n::tr(msgs::WIZ_LEAVE_STAY, &[])).clicked() {
                    self.pending_view = None;
                }
            });
        if discard {
            self.editor.discard_edits();
            self.view = target;
            self.pending_view = None;
        }
    }
}

// ──────────────────────────── 配置方案(Profile)栏 ────────────────────────────

impl DualNicApp {
    fn render_profile_bar(&mut self, ui: &mut eframe::egui::Ui, _status: &PollState) {
        let editor_shared = self.editor.shared();
        let (profiles, active, loading) = {
            let guard = editor_shared.lock().unwrap();
            (guard.profiles.clone(), guard.active_profile.clone(), guard.loading_profiles)
        };
        // 方案栏：始终渲染下拉 + 按钮，不因加载状态吞掉控件。
        let (editing, active2) = {
            let shared_arc = self.editor.shared();
            let g = shared_arc.lock().unwrap();
            (g.editing_profile.clone(), g.active_profile.clone())
        };
        let selected_text = if editing.is_empty() {
            if loading {
                crate::i18n::tr(msgs::WIZ_LOADING_PROFILES, &[])
            } else {
                crate::i18n::tr(msgs::WIZ_NO_PROFILE, &[])
            }
        } else {
            crate::i18n::tr(msgs::WIZ_PROFILE_EDITING_FMT, &[&editing])
        };
        ui.horizontal(|ui| {
            ui.strong(crate::i18n::tr(msgs::WIZ_PROFILE_LABEL, &[]));
            let mut chosen = editing.clone();
            eframe::egui::ComboBox::from_id_salt("profile_sel")
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    for p in &profiles {
                        ui.selectable_value(&mut chosen, p.name.clone(), &p.name);
                    }
                });
            // 选中目标 ≠ 正在编辑 → 切换编辑目标（预览；有未保存修改则确认丢弃）
            if chosen != editing && !chosen.is_empty() {
                if self.editor.is_dirty() {
                    self.pending_edit = Some(chosen.clone());
                } else {
                    self.editor.set_editing_target(chosen);
                }
            }
            ui.add_space(6.0);
            // 启用 = 把编辑中的方案设为激活并立即对账路由（生效）
            let enable_target = if editing.is_empty() { active2.clone() } else { editing.clone() };
            if ui.add_enabled(
                !enable_target.is_empty(),
                eframe::egui::Button::new(crate::i18n::tr(msgs::WIZ_ENABLE_BTN, &[])),
            )
            .clicked()
            {
                self.pending_switch = Some(enable_target.clone());
            }
            ui.add_space(6.0);
            if ui.button(crate::i18n::tr(msgs::WIZ_DUPLICATE_BTN, &[])).clicked() {
                self.profile_act(ActionKind::Duplicate, None);
            }
            if ui.button(crate::i18n::tr(msgs::WIZ_NEW_BTN, &[])).clicked() {
                self.profile_act(ActionKind::New, None);
            }
            // 删除/重命名都作用于「编辑目标」（下拉选中的方案），不是当前生效方案。
            let builtin = editing.is_empty() || editing == dualnic_core::config::DEFAULT_PROFILE_NAME;
            if ui
                .add_enabled(!builtin, eframe::egui::Button::new(crate::i18n::tr(msgs::WIZ_DELETE_BTN, &[])))
                .clicked()
            {
                self.pending_delete = Some(editing.clone());
            }
            if ui
                .add_enabled(!builtin, eframe::egui::Button::new(crate::i18n::tr(msgs::WIZ_RENAME_BTN, &[])))
                .clicked()
            {
                self.rename_target = Some(editing.clone());
                self.rename_input = editing.clone();
            }
            if ui.button(crate::i18n::tr(msgs::WIZ_AUTOMATCH_BTN, &[])).clicked() {
                self.profile_act(ActionKind::AutoMatch, None);
            }
        });
        // 「启用」确认弹窗（有未保存修改时额外警告：启用用的是已保存版本）
        let pending = self.pending_switch.clone();
        if let Some(target) = pending {
            let unsaved = self.editor.is_dirty();
            let mut open = true;
            let ctx = ui.ctx().clone();
            eframe::egui::Window::new(crate::i18n::tr(msgs::WIZ_SWITCH_TITLE, &[]))
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    ui.label(crate::i18n::tr(msgs::WIZ_SWITCH_CONFIRM, &[&target]));
                    if unsaved {
                        ui.colored_label(
                            eframe::egui::Color32::from_rgb(230, 90, 70),
                            crate::i18n::tr(msgs::WIZ_SWITCH_UNSAVED, &[]),
                        );
                    }
                    if target != active {
                        ui.label(crate::i18n::tr(msgs::WIZ_SWITCH_KEEP, &[&active]));
                    }
                    ui.separator();
                    if ui.button(crate::i18n::tr(msgs::WIZ_CONFIRM_ENABLE, &[])).clicked() {
                        self.profile_act(ActionKind::Switch, Some(target.clone()));
                        self.pending_switch = None;
                    }
                    if ui.button(crate::i18n::tr(msgs::WIZ_CANCEL, &[])).clicked() {
                        self.pending_switch = None;
                    }
                });
            if !open {
                self.pending_switch = None;
            }
            let _ = ctx;
        }
        // 编辑目标切换确认弹窗（有未保存修改时）
        let pending_e = self.pending_edit.clone();
        if let Some(target) = pending_e {            let mut open = true;
            eframe::egui::Window::new(crate::i18n::tr(msgs::WIZ_UNSAVED_TITLE, &[]))
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    ui.label(crate::i18n::tr(msgs::WIZ_SWITCH_DISCARD_WARN, &[&target]));
                    ui.separator();
                    if ui.button(crate::i18n::tr(msgs::WIZ_DISCARD_BTN, &[])).clicked() {
                        self.editor.set_editing_target(target.clone());
                        self.pending_edit = None;
                    }
                    if ui.button(crate::i18n::tr(msgs::WIZ_STAY_BTN, &[])).clicked() {
                        self.pending_edit = None;
                    }
                });
            if !open {
                self.pending_edit = None;
            }
        }
        // 删除方案确认弹窗（作用于编辑目标；删除立即从服务端移除，不可撤销）
        let pending_d = self.pending_delete.clone();
        if let Some(target) = pending_d {
            let unsaved = self.editor.is_dirty();
            let mut open = true;
            eframe::egui::Window::new(crate::i18n::tr(msgs::WIZ_DELETE_TITLE, &[]))
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    ui.label(crate::i18n::tr(msgs::WIZ_DELETE_CONFIRM, &[&target]));
                    if unsaved {
                        ui.colored_label(
                            eframe::egui::Color32::from_rgb(230, 90, 70),
                            crate::i18n::tr(msgs::WIZ_DELETE_UNSAVED, &[]),
                        );
                    }
                    ui.separator();
                    if ui.button(crate::i18n::tr(msgs::WIZ_CONFIRM_DELETE, &[])).clicked() {
                        self.pending_delete = None;
                        self.profile_act(ActionKind::Delete, Some(target));
                    }
                    if ui.button(crate::i18n::tr(msgs::WIZ_CANCEL, &[])).clicked() {
                        self.pending_delete = None;
                    }
                });
            if !open {
                self.pending_delete = None;
            }
        }

        // 环境状态行（来自 DetectEnvironment，只读）
        {
            let env = { self.editor.shared().lock().unwrap().env_report.clone() };
            match env {
                None => {}
                Some(Err(e)) => {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(200, 160, 60),
                        crate::i18n::tr(msgs::WIZ_ENV_FAIL, &[&e]),
                    );
                }
                Some(Ok(r)) => {
                    if let Some(name) = &r.matched_profile {
                        ui.colored_label(
                            eframe::egui::Color32::from_rgb(60, 160, 90),
                            crate::i18n::tr(msgs::WIZ_ENV_MATCHED, &[name]),
                        );
                    } else {
                        ui.colored_label(
                            eframe::egui::Color32::from_rgb(220, 150, 60),
                            crate::i18n::tr(msgs::WIZ_ENV_NO_MATCH, &[]),
                        );
                    }
                }
            }
        }

        // 重命名内联输入（直接编辑 self.rename_input，持久化；不再用弹窗导致输入丢失）
        if let Some(old) = self.rename_target.clone() {
            ui.horizontal(|ui| {
                ui.label(crate::i18n::tr(msgs::WIZ_RENAME_PROMPT, &[&old]));
                ui.text_edit_singleline(&mut self.rename_input);
                let can = !self.rename_input.trim().is_empty();
                if ui
                    .add_enabled(
                        can,
                        eframe::egui::Button::new(crate::i18n::tr(msgs::WIZ_RENAME_SAVE, &[])),
                    )
                    .clicked()
                {
                    let target = self.rename_input.trim().to_string();
                    self.rename_target = None;
                    self.profile_act_rename(old, target);
                }
                if ui.button(crate::i18n::tr(msgs::WIZ_CANCEL, &[])).clicked() {
                    self.rename_target = None;
                }
            });
        }

        // 方案操作结果提示已统一走底部消息栏（wizard::ui 末尾渲染 notice）。

    }

    /// 重命名（旧名 / 新名独立传递）。
    fn profile_act_rename(&mut self, old: String, new_name: String) {
        let sh = self.editor.shared().clone();
        std::thread::spawn(move || {
            let result = do_profile_rename(sh.clone(), old.clone(), new_name.clone());
            finish_profile_outcome(&sh, result);
        });
    }

    /// 执行一次性方案操作（切换前先 preview）。
    fn profile_act(&mut self, kind: ActionKind, target: Option<String>) {
        let active = { self.editor.shared().lock().unwrap().active_profile.clone() };
        // switch/delete/restore 用目标或当前方案名；new/duplicate 传 None，由 do_* 生成名字。
        let name = match kind {
            ActionKind::New | ActionKind::Duplicate => target.unwrap_or_default(),
            _ => target.clone().or(Some(active.clone())).unwrap_or_default(),
        };
        let sh = self.editor.shared().clone();
        let status_force = self.shared.clone();
        match kind {
            ActionKind::Switch => {
                let name2 = name.clone();
                std::thread::spawn(move || {
                    let result = do_profile_switch(sh.clone(), status_force, name2);
                    finish_profile_outcome(&sh, result);
                });
            }
            _ => {
                let name2 = name.clone();
                std::thread::spawn(move || {
                    let result = do_profile_action(sh.clone(), kind, name2);
                    finish_profile_outcome(&sh, result);
                });
            }
        }
    }
}

// 方案操作后台执行
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ActionKind {
    New,
    Duplicate,
    Delete,
    Switch,
    AutoMatch,
}

fn do_profile_switch(
    shared: Arc<Mutex<WizardShared>>,
    status_force: Arc<Mutex<PollState>>,
    name: String,
) -> Result<String, String> {
    let token = ensure_token(&shared)?;
    client::switch_profile(&token, &name).map_err(|f| f.message)?;
    status_force.lock().unwrap().force = true;
    shared.lock().unwrap().profile_switch_done = true;
    Ok(crate::i18n::tr(msgs::WIZ_SWITCHED_OK, &[&name]))
}

fn do_profile_action(
    shared: Arc<Mutex<WizardShared>>,
    kind: ActionKind,
    name: String,
) -> Result<String, String> {
    let token = ensure_token(&shared)?;
    let existing: Vec<String> = shared.lock().unwrap().profiles.iter().map(|p| p.name.clone()).collect();
    match kind {
        ActionKind::New => {
            // 默认用「新方案」；被占则加序号
            let n = unique_name(&existing, if name.is_empty() { "新方案".to_string() } else { name }); // 新方案名是存入配置的数据，保持原文
            client::create_profile(&token, &n, None).map_err(|f| f.message)?;
            Ok(crate::i18n::tr(msgs::WIZ_CREATED_OK, &[&n]))
        }
        ActionKind::Duplicate => {
            // 复制源 = 当前激活方案；副本名 = 「{current} (副本)」+ 去重
            let current = shared.lock().unwrap().active_profile.clone();
            let n = unique_name(&existing, format!("{current} (副本)")); // 副本名是存入配置的数据，保持原文
            client::create_profile(&token, &n, Some(&current)).map_err(|f| f.message)?;
            Ok(crate::i18n::tr(msgs::WIZ_COPIED_OK, &[&n]))
        }
        ActionKind::Delete => {
            client::delete_profile(&token, &name).map_err(|f| f.message)?;
            // 编辑目标已被删除：清空编辑目标与基线，profiles 重拉后自动回落到激活方案。
            {
                let mut s = shared.lock().unwrap();
                s.profile_switch_done = true;
                s.editing_profile.clear();
                s.baseline = None;
                s.loading_baseline = false;
            }
            Ok(crate::i18n::tr(msgs::WIZ_DELETED_OK, &[&name]))
        }
        ActionKind::AutoMatch => {
            let data = client::auto_match(&token).map_err(|f| f.message)?;
            // 可能 switch/create 改变了 active，重置编辑缓冲 + 刷新列表
            shared.lock().unwrap().profile_switch_done = true;
            Ok(match data.action.as_str() {
                "switched" => crate::i18n::tr(msgs::WIZ_AUTOMATCH_SWITCHED, &[&data.profile.unwrap_or_default()]),
                "created" => crate::i18n::tr(msgs::WIZ_AUTOMATCH_CREATED, &[&data.profile.unwrap_or_default()]),
                "adopted" => crate::i18n::tr(msgs::WIZ_AUTOMATCH_ADOPTED, &[&data.profile.unwrap_or_default()]),
                _ => crate::i18n::tr(msgs::WIZ_AUTOMATCH_LATEST, &[]),
            })
        }
        ActionKind::Switch => unreachable!("switch is handled by do_profile_switch"),
    }
}

/// 重命名（旧名 / 新名）。若重命名的正是编辑目标，同步更新编辑目标并重灌基线。
fn do_profile_rename(shared: Arc<Mutex<WizardShared>>, old: String, new_name: String) -> Result<String, String> {
    let token = ensure_token(&shared)?;
    client::rename_profile(&token, &old, &new_name).map_err(|f| f.message)?;
    {
        let mut s = shared.lock().unwrap();
        s.profile_switch_done = true; // 刷新方案列表（新名）
        if s.editing_profile == old {
            s.editing_profile = new_name.clone();
            s.baseline = None;
            s.loading_baseline = false;
        }
    }
    Ok(crate::i18n::tr(msgs::WIZ_RENAMED_OK, &[&old, &new_name]))
}

/// 生成不与 `existing` 冲突的名字：若 base 已存在则追加 " 2"," 3"…
fn unique_name(existing: &[String], base: String) -> String {
    if !existing.iter().any(|e| *e == base) {
        return base;
    }
    for i in 2.. {
        let cand = format!("{base} {i}");
        if !existing.iter().any(|e| *e == cand) {
            return cand;
        }
    }
    unreachable!()
}

/// 每次写操作都取**新**一次性令牌（服务端消费一次即失效，不能缓存复用）。
fn ensure_token(shared: &Arc<Mutex<WizardShared>>) -> Result<String, String> {
    let t = client::handshake().map_err(|f| f.message)?;
    shared.lock().unwrap().token = Some(t.clone()); // 仅提示用（不清空无碍）
    Ok(t)
}

fn finish_profile_outcome(
    shared: &Arc<Mutex<WizardShared>>,
    r: Result<String, String>,
) {
    let mut s = shared.lock().unwrap();
    // 成功 → 置空列表，让 ensure_data 下一帧因「空」而重拉（避免直接置 loading 导致死循环）
    if r.is_ok() {
        s.profiles = Vec::new();
        s.loading_profiles = false;
        // 环境匹配状态属于方案操作语境（新建/删除/启用/自动匹配都可能改变它）→ 一并重拉刷新
        s.env_report = None;
        s.loading_env = false;
    }
    // 底部消息栏：方案操作结果（切换/新建/删除/复制/重命名/自动匹配），新消息覆盖旧消息
    s.notice = Some(match r {
        Ok(msg) => Notice::ok(msg),
        Err(msg) => Notice::err(crate::i18n::tr(msgs::WIZ_ACTION_FAIL, &[&msg])),
    });
}

// ──────────────────────────── 状态视图 ────────────────────────────

impl DualNicApp {
    /// 配置导入/导出（JSON 文件，整个配置容器）。
    fn render_config_io(&mut self, ui: &mut eframe::egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button(crate::i18n::tr(msgs::WIZ_EXPORT_BTN, &[])).clicked() {
                self.config_io_msg = Some(self.do_export_config());
            }
            if ui.button(crate::i18n::tr(msgs::WIZ_IMPORT_BTN, &[])).clicked() {
                self.config_io_msg = Some(self.do_import_config());
            }
        });
        if let Some(msg) = &self.config_io_msg {
            ui.label(msg);
        }
    }

    fn do_export_config(&self) -> String {
        match crate::client::export_config() {
            Ok(Some(json)) => {
                let default_name = format!("dualnic-config-{}.json", unix_now_secs());
                match rfd::FileDialog::new().set_file_name(default_name).save_file() {
                    Some(path) => match std::fs::write(&path, json) {
                        Ok(()) => {
                            let p = path.display().to_string();
                            crate::i18n::tr(msgs::WIZ_EXPORTED, &[&p])
                        }
                        Err(e) => crate::i18n::tr(msgs::WIZ_EXPORT_FAIL, &[&e]),
                    },
                    None => crate::i18n::tr(msgs::WIZ_EXPORT_CANCELLED, &[]),
                }
            }
            Ok(None) => crate::i18n::tr(msgs::WIZ_EXPORT_EMPTY, &[]),
            Err(e) => crate::i18n::tr(msgs::WIZ_EXPORT_FAIL, &[&e]),
        }
    }

    fn do_import_config(&self) -> String {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON", &["json"])
            .pick_file()
        else {
            return crate::i18n::tr(msgs::WIZ_IMPORT_CANCELLED, &[]);
        };
        let json = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => return crate::i18n::tr(msgs::WIZ_IMPORT_READ_FAIL, &[&e]),
        };
        match crate::client::handshake()
            .and_then(|token| crate::client::import_config(&token, &json))
        {
            Ok(()) => crate::i18n::tr(msgs::WIZ_IMPORTED, &[]),
            Err(e) => crate::i18n::tr(msgs::WIZ_IMPORT_FAIL, &[&e]),
        }
    }

    fn render_status(&mut self, ui: &mut eframe::egui::Ui, snapshot: &PollState) {
        eframe::egui::ScrollArea::vertical().show(ui, |ui| {
            ui.label(crate::i18n::tr(msgs::STA_HINT, &[]));
            // 首启后台引擎横幅（临时引擎已启动 / 需要授权 / 服务停止等）
            if let Some(b) = self.backend_banner.lock().unwrap().clone() {
                ui.add_space(2.0);
                match b {
                    Ok(msg) => {
                        ui.colored_label(eframe::egui::Color32::from_rgb(60, 160, 90), format!("ℹ {msg}"));
                    }
                    Err(msg) => {
                        ui.colored_label(eframe::egui::Color32::from_rgb(220, 150, 60), format!("⚠ {msg}"));
                    }
                }
                ui.add_space(2.0);
            }

            match &snapshot.last {
                None => {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(180, 180, 180),
                        crate::i18n::tr(msgs::STA_FIRST_POLL, &[]),
                    );
                }
                Some(Ok(st)) => render_online(ui, st),
                Some(Err(msg)) => {
                    ui.colored_label(
                        eframe::egui::Color32::RED,
                        crate::i18n::tr(msgs::STA_UNREACHABLE, &[&msg]),
                    );
                }
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button(crate::i18n::tr(msgs::STA_REFRESH_BTN, &[])).clicked() {
                    self.shared.lock().unwrap().force = true;
                    ui.ctx().request_repaint();
                }
                let ago = if snapshot.last_ok_unix > 0 {
                    format_ago(unix_now_secs().saturating_sub(snapshot.last_ok_unix))
                } else {
                    crate::i18n::tr(msgs::STA_NEVER, &[])
                };
                ui.label(crate::i18n::tr(msgs::STA_LAST_OK, &[&ago]));
            });

            // 暂停/恢复对账（托盘「暂停分流」的等价入口）
            if let Some(Ok(st)) = &snapshot.last {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let label = if st.paused {
                        crate::i18n::tr(msgs::STA_RESUME, &[])
                    } else {
                        crate::i18n::tr(msgs::STA_PAUSE, &[])
                    };
                    if ui.button(label).clicked() {
                        self.toggle_paused(!st.paused, ui.ctx().clone());
                    }
                    if st.paused {
                        ui.colored_label(
                            eframe::egui::Color32::from_rgb(220, 150, 60),
                            crate::i18n::tr(msgs::STA_PAUSED_NOTE, &[]),
                        );
                    }
                });
            }
        });
    }

    /// 后台线程：握手 + 暂停/恢复对账 + 触发状态刷新。
    fn toggle_paused(&self, paused: bool, ctx: eframe::egui::Context) {
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let r = crate::client::handshake()
                .and_then(|token| crate::client::set_paused(&token, paused));
            if let Err(e) = r {
                eprintln!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 100; &e.to_string())));
            }
            shared.lock().unwrap().force = true;
            ctx.request_repaint();
        });
    }
}

fn render_online(ui: &mut eframe::egui::Ui, st: &StatusData) {
    ui.colored_label(
        eframe::egui::Color32::from_rgb(60, 160, 90),
        crate::i18n::tr(msgs::STA_RUNNING, &[]),
    );
    ui.add_space(4.0);
    eframe::egui::Grid::new("status_grid")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            kv(ui, "PID", &st.pid.to_string());
            kv(ui, &crate::i18n::tr(msgs::STA_KV_LISTEN, &[]), &st.listen_addr);
            kv(
                ui,
                &crate::i18n::tr(msgs::STA_KV_PROTO, &[]),
                &st.protocol_version.to_string(),
            );

            let c = &st.config;
            let load = if c.loaded {
                crate::i18n::tr(msgs::STA_CFG_LOADED, &[])
            } else {
                let why = c.load_error
                    .as_ref()
                    .map(dualnic_core::msg::t)
                    .unwrap_or_else(|| crate::i18n::tr(msgs::STA_CFG_LOADERR_UNKNOWN, &[]));
                crate::i18n::tr(msgs::STA_CFG_NOT_LOADED, &[&why])
            };
            kv(ui, &crate::i18n::tr(msgs::STA_KV_CONFIG, &[]), &load);
            kv(ui, "schema_version", &c.schema_version.to_string());
            kv(
                ui,
                &crate::i18n::tr(msgs::STA_KV_LAN_COUNT, &[]),
                &c.lan_networks_count.to_string(),
            );
            kv(
                ui,
                &crate::i18n::tr(msgs::STA_KV_ROLES, &[]),
                &format!("WAN:{} / LAN:{}", onoff(c.wan_rule_present), onoff(c.lan_rule_present)),
            );
            let r = &c.reconciliation;
            kv(
                ui,
                &crate::i18n::tr(msgs::STA_KV_RECON, &[]),
                &format!(
                    "enabled:{} / converge:{} / remove_stale:{} / protected:{}",
                    onoff(r.enabled),
                    onoff(r.converge_default_route),
                    onoff(r.remove_stale_defaults),
                    r.protected_count
                ),
            );
        });
}

fn kv(ui: &mut eframe::egui::Ui, k: &str, v: &str) {
    ui.label(k);
    ui.monospace(v);
    ui.end_row();
}

fn onoff(b: bool) -> &'static str {
    if b {
        "ON"
    } else {
        "off"
    }
}

fn format_ago(secs: u64) -> String {
    if secs < 60 {
        crate::i18n::tr(msgs::CMN_AGO_S, &[&secs])
    } else if secs < 3600 {
        crate::i18n::tr(msgs::CMN_AGO_M, &[&(secs / 60)])
    } else {
        crate::i18n::tr(msgs::CMN_AGO_H, &[&(secs / 3600)])
    }
}

// ──────────────────────────── 风险视图 ────────────────────────────

impl DualNicApp {
    fn render_risk(&mut self, ui: &mut eframe::egui::Ui, snapshot: &PollState) {
        eframe::egui::ScrollArea::vertical().show(ui, |ui| {
            let st: Option<&StatusData> = match &snapshot.last {
                Some(Ok(st)) => Some(st),
                Some(Err(msg)) => {
                    ui.colored_label(
                        eframe::egui::Color32::RED,
                        crate::i18n::tr(msgs::RSK_UNREACHABLE, &[&msg]),
                    );
                    return;
                }
                None => {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(180, 180, 180),
                        crate::i18n::tr(msgs::RSK_FIRST_POLL, &[]),
                    );
                    return;
                }
            };
            let st = st.unwrap();

            let summary = match &st.risk {
                Some(s) => s,
                None => {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(200, 160, 60),
                        crate::i18n::tr(msgs::RSK_VERSION_OLD, &[]),
                    );
                    return;
                }
            };

            let count = summary.default_routes.len();
            if let Some(err) = &summary.read_error {
                banner(
                    ui,
                    eframe::egui::Color32::from_rgb(200, 160, 60),
                    &crate::i18n::tr(msgs::RSK_READ_ERR_TITLE, &[]),
                    &crate::i18n::tr(msgs::RSK_READ_ERR_DETAIL, &[&dualnic_core::msg::t(err)]),
                );
            } else if count >= 2 {
                banner(
                    ui,
                    eframe::egui::Color32::from_rgb(230, 60, 60),
                    &crate::i18n::tr(msgs::RSK_MULTI_TITLE, &[&count]),
                    &crate::i18n::tr(msgs::RSK_MULTI_DETAIL, &[]),
                );
            } else if count == 0 {
                banner(
                    ui,
                    eframe::egui::Color32::from_rgb(200, 160, 60),
                    &crate::i18n::tr(msgs::RSK_NONE_TITLE, &[]),
                    &crate::i18n::tr(msgs::RSK_NONE_DETAIL, &[]),
                );
            } else {
                banner(
                    ui,
                    eframe::egui::Color32::from_rgb(60, 160, 90),
                    &crate::i18n::tr(msgs::RSK_SINGLE_TITLE, &[]),
                    "",
                );
            }

            if !summary.default_routes.is_empty() {
                ui.add_space(6.0);
                render_default_routes(ui, summary);
            }

            ui.add_space(10.0);
            let recon_on = st.config.reconciliation.enabled;
            ui.horizontal(|ui| {
                // 一键收敛（需对账开启 + 管理员）
                if ui
                    .add_enabled(
                        recon_on,
                        eframe::egui::Button::new(crate::i18n::tr(msgs::RSK_RECONCILE_BTN, &[])),
                    )
                    .on_disabled_hover_text(if recon_on {
                        crate::i18n::tr(msgs::RSK_BTN_TIP_OK, &[])
                    } else {
                        crate::i18n::tr(msgs::RSK_BTN_TIP_OFF, &[])
                    })
                    .clicked()
                {
                    self.run_reconcile_action(ui.ctx().clone(), ReconcileAction::ReconcileNow);
                }
            });
            // 结果展示
            let outcome = { self.reconcile_outcome.lock().unwrap().clone() };
            if let Some(res) = outcome {
                match res {
                    Ok(msg) => {
                        ui.colored_label(eframe::egui::Color32::from_rgb(60, 160, 90), msg);
                    }
                    Err(msg) => {
                        ui.colored_label(eframe::egui::Color32::from_rgb(200, 160, 60), msg);
                    }
                }
            }
        });
    }

    /// 后台执行一键收敛（每次新握手令牌），结果写共享态 + 请求重绘。
    fn run_reconcile_action(&self, ctx: eframe::egui::Context, kind: ReconcileAction) {
        let shared = self.reconcile_outcome.clone();
        *shared.lock().unwrap() = None;
        std::thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                let token = client::handshake().map_err(|f| f.message)?;
                match kind {
                    ReconcileAction::ReconcileNow => {
                        client::reconcile_now(&token).map(|r| format_reconcile(&r)).map_err(|f| f.message)
                    }
                }
            })();
            *shared.lock().unwrap() = Some(result);
            ctx.request_repaint();
        });
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReconcileAction {
    ReconcileNow,
}

fn format_reconcile(r: &dualnic_core::ipc::ReconcileResult) -> String {
    if !r.ok {
        let unknown = crate::i18n::tr(msgs::RSK_RESULT_ERR_UNKNOWN, &[]);
        let text = match &r.error {
            Some(m) => dualnic_core::msg::t(m),
            None => unknown,
        };
        return crate::i18n::tr(msgs::RSK_RESULT_FAIL, &[&text]);
    }
    if r.removed_defaults == 0 && r.removed_static_prefixes == 0 && r.added_prefixes == 0 && r.metrics_fixed == 0 {
        return crate::i18n::tr(msgs::RSK_RESULT_UNCHANGED, &[]);
    }
    crate::i18n::tr(
        msgs::RSK_RESULT_DONE,
        &[
            &r.removed_defaults,
            &r.removed_static_prefixes,
            &r.added_prefixes,
            &r.metrics_fixed,
        ],
    )
}

fn banner(ui: &mut eframe::egui::Ui, color: eframe::egui::Color32, title: &str, detail: &str) {
    ui.colored_label(
        color,
        eframe::egui::RichText::new(title).size(18.0).strong(),
    );
    if !detail.is_empty() {
        ui.label(detail);
    }
}

fn render_default_routes(ui: &mut eframe::egui::Ui, summary: &dualnic_core::ipc::RiskSummary) {
    eframe::egui::Grid::new("risk_grid")
        .num_columns(6)
        .striped(true)
        .spacing([12.0, 3.0])
        .show(ui, |ui| {
            for h in [
                    crate::i18n::tr(msgs::RSK_H_IF, &[]),
                    crate::i18n::tr(msgs::RSK_H_ALIAS, &[]),
                    crate::i18n::tr(msgs::RSK_H_DESC, &[]),
                    crate::i18n::tr(msgs::RSK_H_GW, &[]),
                    "metric".to_string(),
                    crate::i18n::tr(msgs::RSK_H_PROTO, &[]),
                ] {
                ui.strong(h);
            }
            ui.end_row();
            for r in &summary.default_routes {
                ui.monospace(r.if_index.to_string());
                ui.label(&r.interface_alias);
                ui.label(&r.interface_desc);
                ui.monospace(&r.gateway);
                ui.monospace(r.metric.to_string());
                ui.monospace(r.source_proto.to_string());
                ui.end_row();
            }
        });
}

// ──────────────────────────── 快照/策略视图 ────────────────────────────

impl DualNicApp {
    fn render_snapshot(&mut self, ui: &mut eframe::egui::Ui, _status: &PollState) {
        let ctx = ui.ctx().clone();
        // 首次进入且无缓存 → 自动取一次；之后靠「立即刷新」。
        let need_fetch = {
            let s = self.snap.lock().unwrap();
            s.last.is_none() && !s.in_flight
        };
        if need_fetch {
            crate::state::fetch_snapshot_async(ctx, self.snap.clone());
        }
        let (last, in_flight) = {
            let s = self.snap.lock().unwrap();
            (s.last.clone(), s.in_flight)
        };

        eframe::egui::ScrollArea::vertical().show(ui, |ui| {
            ui.horizontal(|ui| {
                let clicked = ui
                    .add_enabled(
                        !in_flight,
                        eframe::egui::Button::new(crate::i18n::tr(msgs::SSN_REFRESH_BTN, &[])),
                    )
                    .clicked();
                if clicked {
                    crate::state::refresh_snapshot_async(ui.ctx().clone(), self.snap.clone());
                }
                if in_flight {
                    ui.spinner();
                    ui.label(crate::i18n::tr(msgs::SSN_FETCHING, &[]));
                }
            });
            ui.add_space(4.0);

            match &last {
                None => {
                    ui.label(crate::i18n::tr(msgs::SSN_NO_SNAPSHOT, &[]));
                }
                Some(Err(msg)) => {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(200, 160, 60),
                        crate::i18n::tr(msgs::SSN_UNAVAILABLE, &[&msg]),
                    );
                }
                Some(Ok(data)) => {
                    render_snapshot_body(ui, data);
                    self.render_lpm_query(ui, &data.report);
                }
            }
        });
    }

    /// 规则预览：输入任意 IPv4，用 LPM（与 Windows 路由决策一致）显示它走哪块卡。
    fn render_lpm_query(&mut self, ui: &mut eframe::egui::Ui, report: &DiffReport) {
        ui.add_space(8.0);
        ui.separator();
        ui.heading(crate::i18n::tr(msgs::SSN_LPM_HEADING, &[]));
        ui.horizontal(|ui| {
            ui.label(crate::i18n::tr(msgs::SSN_LPM_LABEL, &[]));
            ui.add(
                eframe::egui::TextEdit::singleline(&mut self.lpm_query)
                    .desired_width(140.0)
                    .hint_text(crate::i18n::tr(msgs::SSN_LPM_HINT, &[])),
            );
        });

        let trimmed = self.lpm_query.trim();
        if trimmed.is_empty() {
            let list = report
                .lan_networks
                .iter()
                .map(|n| n.cidr.to_string())
                .collect::<Vec<_>>()
                .join("  ");
            ui.label(crate::i18n::tr(
                msgs::SSN_LAN_LIST,
                &[&report.lan_networks.len(), &list],
            ));
            return;
        }

        match trimmed.parse::<Ipv4Addr>() {
            Ok(ip) => {
                let (color, text) = match decide_lpm(&report.lan_networks, ip) {
                    LpmDecision::Lan(p) => (
                        eframe::egui::Color32::from_rgb(60, 160, 90),
                        crate::i18n::tr(msgs::SSN_LPM_LAN, &[&ip, &p]),
                    ),
                    LpmDecision::Wan => (
                        eframe::egui::Color32::from_rgb(80, 150, 230),
                        crate::i18n::tr(msgs::SSN_LPM_WAN, &[&ip]),
                    ),
                };
                ui.colored_label(color, text);
            }
            Err(_) => {
                ui.colored_label(
                    eframe::egui::Color32::from_rgb(230, 90, 70),
                    crate::i18n::tr(msgs::SSN_LPM_INVALID, &[&trimmed]),
                );
            }
        }
    }

    /// 一键诊断页：取系统路由/网卡原始输出 + diff，支持一键导出。
    /// 进页自动「立即诊断」由 ui() 的 diag_was_active 触发，这里只负责渲染与手动按钮。
    fn render_diagnose(&mut self, ui: &mut eframe::egui::Ui, _status: &PollState) {
        let (last, in_flight) = {
            let d = self.diag.lock().unwrap();
            (d.last.clone(), d.in_flight)
        };

        eframe::egui::ScrollArea::vertical().show(ui, |ui| {
            ui.label(crate::i18n::tr(msgs::DIA_HINT, &[]));
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !in_flight,
                        eframe::egui::Button::new(crate::i18n::tr(msgs::DIA_RUN_BTN, &[])),
                    )
                    .clicked()
                {
                    crate::state::refresh_diagnose_async(ui.ctx().clone(), self.diag.clone());
                }
                if in_flight {
                    ui.spinner();
                    ui.label(crate::i18n::tr(msgs::DIA_RUNNING_BTN, &[]));
                }
            });
            ui.add_space(4.0);

            match &last {
                None => {
                    // 进页自动刷新会先清 last：此时是「正在诊断」而非「尚未诊断」。
                    if in_flight {
                        ui.label(crate::i18n::tr(msgs::DIA_RUNNING_PAGE, &[]));
                    } else {
                        ui.label(crate::i18n::tr(msgs::DIA_NOT_YET, &[]));
                    }
                }
                Some(Err(msg)) => {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(230, 90, 70),
                        crate::i18n::tr(msgs::DIA_FAILED, &[&msg]),
                    );
                }
                Some(Ok(d)) => render_diagnose_body(ui, d),
            }
        });
    }

    /// 事件日志页：从 SQLite 拉取最近事件展示。
    fn render_events(&mut self, ui: &mut eframe::egui::Ui) {
        let need_fetch = {
            let e = self.events.lock().unwrap();
            e.last.is_none() && !e.in_flight
        };
        if need_fetch {
            crate::state::fetch_events_async(ui.ctx().clone(), self.events.clone());
        }
        let (last, in_flight, clearing, clear_result) = {
            let e = self.events.lock().unwrap();
            (e.last.clone(), e.in_flight, e.clearing, e.clear_result.clone())
        };
        let db_size = if let Some(Ok(d)) = &last { d.db_size_bytes } else { None };

        eframe::egui::ScrollArea::vertical().show(ui, |ui| {
            ui.label(crate::i18n::tr(msgs::EVT_HINT, &[]));
            ui.add_space(2.0);
            let unknown_db = crate::i18n::tr(msgs::EVT_DB_UNKNOWN, &[]);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !in_flight,
                        eframe::egui::Button::new(crate::i18n::tr(msgs::EVT_REFRESH_BTN, &[])),
                    )
                    .clicked()
                {
                    crate::state::refresh_events_async(ui.ctx().clone(), self.events.clone());
                }
                if in_flight {
                    ui.spinner();
                    ui.label(crate::i18n::tr(msgs::EVT_FETCHING, &[]));
                }
                ui.separator();
                ui.label(crate::i18n::tr(msgs::EVT_DB_SIZE, &[&fmt_db_size(db_size, &unknown_db)]));
                ui.separator();
                if ui
                    .add_enabled(
                        !clearing,
                        eframe::egui::Button::new(crate::i18n::tr(msgs::EVT_CLEAR_BTN, &[])),
                    )
                    .clicked()
                {
                    self.pending_clear_events = true;
                }
                if clearing {
                    ui.spinner();
                    ui.label(crate::i18n::tr(msgs::EVT_CLEARING, &[]));
                }
                match &clear_result {
                    Some(Ok(())) => {
                        ui.colored_label(
                            eframe::egui::Color32::from_rgb(60, 160, 90),
                            crate::i18n::tr(msgs::EVT_CLEARED, &[]),
                        );
                    }
                    Some(Err(msg)) => {
                        ui.colored_label(
                            eframe::egui::Color32::from_rgb(230, 90, 70),
                            crate::i18n::tr(msgs::EVT_CLEAR_FAILED, &[&msg]),
                        );
                    }
                    None => {}
                }
            });
            ui.add_space(4.0);

            match &last {
                None => {
                    ui.label(crate::i18n::tr(msgs::EVT_NONE_YET, &[]));
                }
                Some(Err(msg)) => {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(230, 90, 70),
                        crate::i18n::tr(msgs::EVT_UNAVAILABLE, &[&msg]),
                    );
                }
                Some(Ok(d)) => render_events_body(ui, d),
            }
        });

        // 清空日志确认弹窗
        if self.pending_clear_events {
            let mut open = true;
            eframe::egui::Window::new(crate::i18n::tr(msgs::EVT_CLEAR_WIN_TITLE, &[]))
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    let unknown_db = crate::i18n::tr(msgs::EVT_DB_UNKNOWN, &[]);
                    ui.label(crate::i18n::tr(
                        msgs::EVT_CLEAR_CONFIRM,
                        &[&fmt_db_size(db_size, &unknown_db)],
                    ));
                    ui.separator();
                    if ui
                        .add_enabled(
                            !clearing,
                            eframe::egui::Button::new(crate::i18n::tr(msgs::EVT_CLEAR_CONFIRM_BTN, &[])),
                        )
                        .clicked()
                    {
                        self.pending_clear_events = false;
                        crate::state::clear_events_async(ui.ctx().clone(), self.events.clone());
                    }
                    if ui.button(crate::i18n::tr(msgs::EVT_CANCEL, &[])).clicked() {
                        self.pending_clear_events = false;
                    }
                });
            if !open {
                self.pending_clear_events = false;
            }
        }
    }

    /// 设置页：Windows 服务注册/卸载（弹 UAC）+ GUI 登录自启动（普通权限）。
    fn render_settings(&mut self, ui: &mut eframe::egui::Ui) {
        // 首次进入探测一次状态。
        let need_load = {
            let s = self.settings.lock().unwrap();
            !s.loaded && !s.loading && s.busy.is_none()
        };
        if need_load {
            crate::state::load_settings_async(ui.ctx().clone(), self.settings.clone());
        }
        let (loading, busy, outcome, svc, autostart, svc_exe) = {
            let s = self.settings.lock().unwrap();
            (s.loading, s.busy.clone(), s.outcome.clone(), s.svc, s.autostart, s.svc_exe_exists)
        };
        let working = busy.is_some();

        eframe::egui::ScrollArea::vertical().show(ui, |ui| {
            // ── 语言 / Language：切换后各页文案随之切换 ──
            ui.group(|ui| {
                ui.strong(crate::i18n::tr(msgs::SET_LANG_HEADING, &[]));
                let choice = crate::i18n::current_choice();
                let mut picked = choice.clone();
                let detected = crate::i18n::detected_system_language();
                ui.radio_value(
                    &mut picked,
                    "system".to_string(),
                    crate::i18n::tr(msgs::SET_LANG_SYSTEM, &[&detected]),
                );
                for (code, name) in crate::i18n::available() {
                    ui.radio_value(&mut picked, code.clone(), format!("{name} ({code})"));
                }
                if picked != choice {
                    if let Err(e) = crate::i18n::set_choice(&picked) {
                        // 切换失败极罕见（ini 写不进），原样提示；不影响当前语言
                        eprintln!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 101; &e.to_string())));
                    }
                    ui.ctx().request_repaint();
                }
            });
            ui.add_space(6.0);

            ui.strong(crate::i18n::tr(msgs::SET_DEPLOY_TITLE, &[]));
            ui.label(crate::i18n::tr(msgs::SET_DEPLOY_HINT, &[]));
            ui.add_space(6.0);

            // ── 后台服务 ──
            ui.group(|ui| {
                ui.strong(crate::i18n::tr(msgs::SET_SVC_TITLE, &[]));
                match svc {
                    None if loading => {
                        ui.horizontal(|ui| { ui.spinner(); ui.label(crate::i18n::tr(msgs::SET_PROBING, &[])); });
                    }
                    None => { ui.label(crate::i18n::tr(msgs::SET_NOT_PROBED, &[])); }
                    Some(st) => {
                        let color = match st {
                            crate::setup::ServiceStatus::Running => eframe::egui::Color32::from_rgb(60, 160, 90),
                            crate::setup::ServiceStatus::Stopped => eframe::egui::Color32::from_rgb(220, 150, 60),
                            crate::setup::ServiceStatus::NotInstalled => eframe::egui::Color32::from_rgb(230, 90, 70),
                        };
                        ui.colored_label(color, format!("● {}", st.label()));
                    }
                }
                if svc_exe == Some(false) {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(230, 90, 70),
                        crate::i18n::tr(msgs::SET_SVC_MISSING, &[]),
                    );
                }
                ui.horizontal(|ui| {
                    let can_install = !working && svc != Some(crate::setup::ServiceStatus::Running);
                    if ui.add_enabled(can_install, eframe::egui::Button::new(crate::i18n::tr(msgs::SET_INSTALL_BTN, &[]))).clicked() {
                        crate::state::install_service_async(ui.ctx().clone(), self.settings.clone());
                    }
                    let can_uninstall = !working && svc.is_some_and(|s| s != crate::setup::ServiceStatus::NotInstalled);
                    if ui.add_enabled(can_uninstall, eframe::egui::Button::new(crate::i18n::tr(msgs::SET_UNINSTALL_BTN, &[]))).clicked() {
                        self.pending_uninstall_svc = true;
                    }
                });
                ui.label(crate::i18n::tr(msgs::SET_SVC_NOTE, &[]));
            });
            ui.add_space(6.0);

            // ── 登录自启动 ──
            ui.group(|ui| {
                ui.strong(crate::i18n::tr(msgs::SET_AUTO_TITLE, &[]));
                match autostart {
                    None if loading => {
                        ui.horizontal(|ui| { ui.spinner(); ui.label(crate::i18n::tr(msgs::SET_PROBING, &[])); });
                    }
                    None => { ui.label(crate::i18n::tr(msgs::SET_NOT_PROBED, &[])); }
                    Some(true) => {
                        ui.colored_label(
                            eframe::egui::Color32::from_rgb(60, 160, 90),
                            crate::i18n::tr(msgs::SET_AUTO_ON, &[]),
                        );
                    }
                    Some(false) => { ui.label(crate::i18n::tr(msgs::SET_AUTO_OFF, &[])); }
                }
                ui.horizontal(|ui| {
                    if ui.add_enabled(!working && autostart != Some(true), eframe::egui::Button::new(crate::i18n::tr(msgs::SET_AUTO_REG_BTN, &[]))).clicked() {
                        crate::state::install_autostart_async(ui.ctx().clone(), self.settings.clone());
                    }
                    if ui.add_enabled(!working && autostart == Some(true), eframe::egui::Button::new(crate::i18n::tr(msgs::SET_AUTO_UNREG_BTN, &[]))).clicked() {
                        crate::state::uninstall_autostart_async(ui.ctx().clone(), self.settings.clone());
                    }
                });
                ui.label(crate::i18n::tr(msgs::SET_AUTO_NOTE, &[]));
            });
            ui.add_space(6.0);

            // ── 动作进行中 / 结果 ──
            if let Some(b) = busy {
                ui.horizontal(|ui| { ui.spinner(); ui.label(crate::i18n::tr(msgs::SET_BUSY, &[&b])); });
            }
            match outcome {
                Some(Ok(msg)) => {
                    ui.colored_label(eframe::egui::Color32::from_rgb(60, 160, 90), msg);
                }
                Some(Err(msg)) => {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(230, 90, 70),
                        crate::i18n::tr(msgs::SET_FAILED, &[&msg]),
                    );
                }
                None => {}
            }
        });

        // 卸载服务确认弹窗（会停掉路由自愈，值得拦一下）
        if self.pending_uninstall_svc {
            let mut open = true;
            eframe::egui::Window::new(crate::i18n::tr(msgs::SET_UNINSTALL_WIN, &[]))
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ui.ctx(), |ui| {
                    ui.colored_label(
                        eframe::egui::Color32::from_rgb(220, 150, 60),
                        crate::i18n::tr(msgs::SET_UNINSTALL_WARN, &[]),
                    );
                    ui.label(crate::i18n::tr(msgs::SET_UNINSTALL_CONFIRM, &[]));
                    ui.separator();
                    if ui
                        .add_enabled(
                            !working,
                            eframe::egui::Button::new(crate::i18n::tr(msgs::SET_UNINSTALL_CONFIRM_BTN, &[])),
                        )
                        .clicked()
                    {
                        self.pending_uninstall_svc = false;
                        crate::state::uninstall_service_async(ui.ctx().clone(), self.settings.clone());
                    }
                    if ui.button(crate::i18n::tr(msgs::WIZ_CANCEL, &[])).clicked() {
                        self.pending_uninstall_svc = false;
                    }
                });
            if !open {
                self.pending_uninstall_svc = false;
            }
        }
    }

    /// 物理网卡标记页：列出所有探测到的网卡，勾选「物理」，保存到全局标记集合。
    fn render_physical_nics(&mut self, ui: &mut eframe::egui::Ui) {
        // 首次进入加载适配器列表 + 已保存的标记。
        let need_load = {
            let s = self.physical_state.lock().unwrap();
            !s.loaded && !s.loading
        };
        if need_load {
            gui_spawn_physical_load(ui.ctx().clone(), self.physical_state.clone());
        }
        let (adapters, checked, loading, saving, outcome) = {
            let s = self.physical_state.lock().unwrap();
            (s.adapters.clone(), s.checked.clone(), s.loading, s.saving, s.outcome.clone())
        };

        eframe::egui::ScrollArea::vertical().show(ui, |ui| {
            ui.strong(crate::i18n::tr(msgs::PHY_TITLE, &[]));
            ui.label(crate::i18n::tr(msgs::PHY_HINT1, &[]));
            ui.label(crate::i18n::tr(msgs::PHY_HINT2, &[]));
            ui.add_space(4.0);

            if loading {
                ui.horizontal(|ui| { ui.spinner(); ui.label(crate::i18n::tr(msgs::PHY_ENUMING, &[])); });
                return;
            }
            if adapters.is_empty() {
                ui.colored_label(
                    eframe::egui::Color32::from_rgb(230, 160, 40),
                    crate::i18n::tr(msgs::PHY_NONE, &[]),
                );
                return;
            }

            // 推测是物理卡的置顶、疑似虚拟卡沉底（仅展示排序；判定只看用户标记）。同组保持枚举顺序。
            let mut rows: Vec<&dualnic_core::diff::AdapterView> = adapters
                .iter()
                .filter(|a| a.guid.as_deref().is_some_and(|g| !g.is_empty()))
                .collect();
            rows.sort_by_key(|a| adapter_looks_virtual(a)); // stable：false(物理) 在前
            for a in rows {
                let guid = a.guid.clone().unwrap_or_default();
                let mut on = checked.contains(&guid);
                let looks_virtual = adapter_looks_virtual(a);
                let label = adapter_display_name(a);
                let mut toggled: Option<bool> = None;
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut on, "").changed() {
                        toggled = Some(on);
                    }
                    ui.label(label);
                    if a.oper_up {
                        ui.label(format!("up ip={}", a.primary_ipv4.map(|v| v.to_string()).unwrap_or_else(|| "—".into())));
                    } else {
                        ui.label("down");
                    }
                    if looks_virtual {
                        ui.colored_label(
                            eframe::egui::Color32::from_rgb(230, 180, 40),
                            crate::i18n::tr(msgs::PHY_MAY_VIRTUAL, &[]),
                        );
                    }
                    // GUID 放行尾并弱化缩小：普通用户不关心，排障时仍能看到。
                    ui.add(eframe::egui::Label::new(
                        eframe::egui::RichText::new(guid.as_str()).weak().small().monospace(),
                    ));
                });
                if let Some(on) = toggled {
                    let mut s = self.physical_state.lock().unwrap();
                    if on {
                        s.checked.insert(guid.clone());
                    } else {
                        s.checked.remove(&guid);
                    }
                }
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let can_save = !saving;
                if ui.add_enabled(can_save, eframe::egui::Button::new(crate::i18n::tr(msgs::PHY_SAVE_BTN, &[]))).clicked() {
                    let guids: Vec<String> = {
                        let s = self.physical_state.lock().unwrap();
                        let mut v: Vec<String> = s.checked.iter().cloned().collect();
                        v.sort();
                        v
                    };
                    gui_spawn_physical_save(ui.ctx().clone(), self.physical_state.clone(), guids);
                }
                if saving {
                    ui.spinner();
                    ui.label(crate::i18n::tr(msgs::PHY_SAVING, &[]));
                }
                if ui.button(crate::i18n::tr(msgs::PHY_RELOAD_BTN, &[])).clicked() {
                    let mut s = self.physical_state.lock().unwrap();
                    s.loaded = false;
                }
            });
            if let Some(out) = &outcome {
                match out {
                    Ok(()) => {
                        ui.colored_label(
                            eframe::egui::Color32::from_rgb(80, 200, 100),
                            crate::i18n::tr(msgs::PHY_SAVED, &[]),
                        );
                    }
                    Err(e) => {
                        ui.colored_label(
                            eframe::egui::Color32::from_rgb(230, 90, 70),
                            crate::i18n::tr(msgs::PHY_SAVE_FAILED, &[&e]),
                        );
                    }
                }
            }
        });
    }

    /// 首启引导：物理标记集合为空（且已加载到网卡）时，弹强制引导。
    /// 行为（用户定稿）：未配置标记时，非标记页全屏遮罩+弹窗，禁止操作弹窗以外内容；
    /// 「网卡标记」页本身不弹（用户正在配置）；逃到其他页会再弹；配置保存后永不再弹。
    fn render_onboarding(&mut self, ui: &mut eframe::egui::Ui) {
        let (loaded, saved_empty, has_adapters, saving) = {
            let s = self.physical_state.lock().unwrap();
            (s.loaded, s.saved_guids.is_empty(), !s.adapters.is_empty(), s.saving)
        };
        if !loaded || !saved_empty || !has_adapters {
            return;
        }
        // 标记页本身不弹（用户正在配置）；其他页弹强制引导。
        if self.view == View::PhysicalNics {
            return;
        }
        let ctx = ui.ctx().clone();
        // 全屏遮罩：压暗并吞掉一切点击 —— 真模态，弹窗以外禁止操作。
        let sr = ctx.input(|i| i.viewport_rect());
        eframe::egui::Area::new(eframe::egui::Id::new("onboarding_shield"))
            .order(eframe::egui::Order::Middle)
            .show(&ctx, |ui| {
                ui.allocate_rect(sr, eframe::egui::Sense::click_and_drag());
                ui.painter().rect_filled(sr, 0.0, eframe::egui::Color32::from_black_alpha(140));
            });
        let mut go = false;
        eframe::egui::Window::new(crate::i18n::tr(msgs::TRAY_ONBOARD_TITLE, &[]))
            .order(eframe::egui::Order::Foreground) // 恒高于 Middle 遮罩：点遮罩不会把弹窗压下去
            .collapsible(false)
            .resizable(false)
            .anchor(eframe::egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(&ctx, |ui| {
                ui.label(crate::i18n::tr(msgs::TRAY_ONBOARD_L1, &[]));
                ui.label(crate::i18n::tr(msgs::TRAY_ONBOARD_L2, &[]));
                ui.label(crate::i18n::tr(msgs::TRAY_ONBOARD_L3, &[]));
                ui.add_space(6.0);
                if saving {
                    ui.horizontal(|ui| { ui.spinner(); ui.label(crate::i18n::tr(msgs::TRAY_ONBOARD_SAVING, &[])); });
                } else if ui.button(crate::i18n::tr(msgs::TRAY_ONBOARD_GO, &[])).clicked() {
                    go = true;
                }
            });
        if go {
            self.view = View::PhysicalNics;
        }
    }
}

fn render_diagnose_body(ui: &mut eframe::egui::Ui, d: &DiagnoseData) {
    if let Some(e) = &d.command_error {
        ui.colored_label(
            eframe::egui::Color32::from_rgb(230, 90, 70),
            crate::i18n::tr(msgs::DIA_CMD_ERROR, &[&dualnic_core::msg::t(e)]),
        );
    }

    let report = &d.report;
    let card = |c: &Option<ResolvedCard>| {
        c.as_ref()
            .map(|c| c.alias.clone().unwrap_or_else(|| format!("if#{}", c.if_index)))
            .unwrap_or_else(|| crate::i18n::tr(msgs::DIA_UNRESOLVED, &[]))
    };
    let wan_card = card(&report.wan);
    let lan_card = card(&report.lan);
    ui.horizontal(|ui| {
        ui.label(crate::i18n::tr(msgs::DIA_WAN_LABEL, &[&wan_card]));
        ui.label(crate::i18n::tr(msgs::DIA_LAN_LABEL, &[&lan_card]));
        ui.label(crate::i18n::tr(msgs::DIA_DIFF_COUNT, &[&report.actions.len()]));
        if let Some(e) = &report.resolve_error {
            ui.colored_label(
                eframe::egui::Color32::from_rgb(200, 160, 60),
                crate::i18n::tr(msgs::DIA_RESOLVE_ERROR, &[&e]),
            );
        }
    });

    let rp_hdr = crate::i18n::tr(msgs::DIA_ROUTE_PRINT_HDR, &[&d.route_print.len()]);
    eframe::egui::CollapsingHeader::new(rp_hdr)
        .default_open(false)
        .show(ui, |ui| {
            for l in &d.route_print {
                ui.monospace(l);
            }
        });
    let ni_hdr = crate::i18n::tr(msgs::DIA_NETIF_HDR, &[&d.net_interfaces.len()]);
    eframe::egui::CollapsingHeader::new(ni_hdr)
        .default_open(false)
        .show(ui, |ui| {
            for l in &d.net_interfaces {
                ui.monospace(l);
            }
        });
    let nc_hdr = crate::i18n::tr(msgs::DIA_NETCFG_HDR, &[&d.net_config.len()]);
    eframe::egui::CollapsingHeader::new(nc_hdr)
        .default_open(false)
        .show(ui, |ui| {
            for l in &d.net_config {
                ui.monospace(l);
            }
        });

    ui.add_space(6.0);
    if ui.button(crate::i18n::tr(msgs::DIA_EXPORT_BTN, &[])).clicked() {
        match save_diagnose(&d.to_text()) {
            Ok(path) => {
                let p = path.display().to_string();
                ui.colored_label(
                    eframe::egui::Color32::from_rgb(60, 160, 90),
                    crate::i18n::tr(msgs::DIA_EXPORTED, &[&p]),
                );
            }
            Err(e) => {
                ui.colored_label(
                    eframe::egui::Color32::from_rgb(230, 90, 70),
                    crate::i18n::tr(msgs::DIA_EXPORT_FAILED, &[&e]),
                );
            }
        }
    }
}

/// 导出诊断报告到 exe 同级目录，文件名带时间戳。
fn save_diagnose(text: &str) -> Result<std::path::PathBuf, String> {
    let mut path = std::env::current_exe().map_err(|e| e.to_string())?;
    path.pop(); // 去 exe 文件名，留目录
    path.push(format!("diagnose-{}.txt", unix_now_secs()));
    std::fs::write(&path, text).map_err(|e| e.to_string())?;
    Ok(path)
}

/// 数据库大小的人类可读格式（None = 内存降级库，无文件可量）。
fn fmt_db_size(bytes: Option<u64>, unknown: &str) -> String {
    const KIB: f64 = 1024.0;
    match bytes {
        None => unknown.to_string(),
        Some(b) if (b as f64) < KIB => format!("{b} B"),
        Some(b) if (b as f64) < KIB * KIB => format!("{:.1} KB", b as f64 / KIB),
        Some(b) => format!("{:.2} MB", b as f64 / (KIB * KIB)),
    }
}

fn render_events_body(ui: &mut eframe::egui::Ui, d: &EventsData) {
    if d.entries.is_empty() {
        ui.label(crate::i18n::tr(msgs::EVT_EMPTY, &[]));
        return;
    }
    let headers = [
        crate::i18n::tr(msgs::EVT_H_TIME, &[]),
        crate::i18n::tr(msgs::EVT_H_LEVEL, &[]),
        crate::i18n::tr(msgs::EVT_H_SOURCE, &[]),
        crate::i18n::tr(msgs::EVT_H_MESSAGE, &[]),
    ];
    eframe::egui::Grid::new("events_grid")
        .num_columns(4)
        .striped(true)
        .spacing([12.0, 3.0])
        .show(ui, |ui| {
            for h in &headers {
                ui.strong(h);
            }
            ui.end_row();
            for e in &d.entries {
                ui.monospace(format_ts(e.ts_unix_secs));
                ui.colored_label(level_color(&e.level), &e.level);
                ui.label(&e.source);
                // v2 结构化消息按当前语言渲染；老事件（无 msg 字段）显示服务端原文
                let text = match &e.msg {
                    Some(m) => dualnic_core::msg::t(m),
                    None => e.message.clone(),
                };
                ui.label(text);
                ui.end_row();
            }
        });
}

fn level_color(level: &str) -> eframe::egui::Color32 {
    match level {
        "error" => eframe::egui::Color32::from_rgb(230, 90, 70),
        "warn" => eframe::egui::Color32::from_rgb(220, 150, 60),
        _ => eframe::egui::Color32::from_rgb(160, 160, 160),
    }
}

/// unix 秒 → 本地 `HH:MM:SS`（无 chrono 时退化为 unix 秒）。
fn format_ts(ts: u64) -> String {
    chrono::DateTime::from_timestamp(ts as i64, 0)
        .map(|dt| dt.with_timezone(&chrono::Local).format("%H:%M:%S").to_string())
        .unwrap_or_else(|| ts.to_string())
}

fn render_reachable_table(ui: &mut eframe::egui::Ui, report: &DiffReport) {
    let role_color = |if_index: u32| -> eframe::egui::Color32 {
        if report.wan.as_ref().is_some_and(|c| c.if_index == if_index) {
            eframe::egui::Color32::from_rgb(80, 150, 230) // WAN 蓝
        } else if report.lan.as_ref().is_some_and(|c| c.if_index == if_index) {
            eframe::egui::Color32::from_rgb(60, 160, 90) // LAN 绿
        } else {
            eframe::egui::Color32::from_rgb(150, 150, 150) // 其它灰
        }
    };
    let role_label = |if_index: u32| -> String {
        if report.wan.as_ref().is_some_and(|c| c.if_index == if_index) {
            "WAN".into()
        } else if report.lan.as_ref().is_some_and(|c| c.if_index == if_index) {
            "LAN".into()
        } else {
            crate::i18n::tr(msgs::SSN_ROLE_OTHER, &[])
        }
    };
    let headers = [
        crate::i18n::tr(msgs::SSN_H_ROLE, &[]),
        crate::i18n::tr(msgs::SSN_H_NIC, &[]),
        crate::i18n::tr(msgs::SSN_H_SUBNET, &[]),
        crate::i18n::tr(msgs::SSN_H_SOURCE, &[]),
    ];
    eframe::egui::Grid::new("reachable_grid")
        .num_columns(4)
        .striped(true)
        .spacing([14.0, 3.0])
        .show(ui, |ui| {
            for h in &headers {
                ui.strong(h);
            }
            ui.end_row();
            for r in &report.reachable {
                ui.colored_label(role_color(r.if_index), role_label(r.if_index));
                let name = report
                    .adapters
                    .iter()
                    .find(|a| a.if_index == r.if_index)
                    .map(|a| a.alias.clone().unwrap_or_default())
                    .unwrap_or_else(|| format!("if#{}", r.if_index));
                ui.label(name);
                ui.monospace(r.prefix.to_string());
                ui.label(match r.source {
                    dualnic_core::diff::ReachSource::AdapterPrimary => {
                        crate::i18n::tr(msgs::SSN_SRC_PRIMARY, &[])
                    }
                    dualnic_core::diff::ReachSource::OnLinkRoute => {
                        crate::i18n::tr(msgs::SSN_SRC_ONLINK, &[])
                    }
                });
                ui.end_row();
            }
        });
}

fn render_snapshot_body(ui: &mut eframe::egui::Ui, data: &SnapshotData) {
    if let Some(e) = &data.read_error {
        ui.colored_label(
            eframe::egui::Color32::from_rgb(200, 160, 60),
            crate::i18n::tr(msgs::SSN_READ_PARTIAL, &[&dualnic_core::msg::t(e)]),
        );
    }
    let report = &data.report;

    ui.add_space(6.0);
    // 上下两张全宽表单卡（原左右并排在窄窗下 LAN 卡被截断）。
    role_card(
        ui,
        &crate::i18n::tr(msgs::SSN_ROLE_WAN_TITLE, &[]),
        report.wan.as_ref(),
    );
    ui.add_space(4.0);
    role_card(
        ui,
        &crate::i18n::tr(msgs::SSN_ROLE_LAN_TITLE, &[]),
        report.lan.as_ref(),
    );
    if let Some(e) = &report.resolve_error {
        ui.colored_label(
            eframe::egui::Color32::from_rgb(200, 160, 60),
            crate::i18n::tr(msgs::SSN_RESOLVE_ERROR, &[&e]),
        );
    }

    ui.add_space(8.0);
    ui.heading(crate::i18n::tr(msgs::SSN_DIFF_HEADING, &[]));
    if report.actions.is_empty() {
        ui.label(crate::i18n::tr(msgs::SSN_NO_DIFF, &[]));
    } else {
        let find = |if_index: u32| report.adapters.iter().find(|ad| ad.if_index == if_index);
        let headers = [
            crate::i18n::tr(msgs::SSN_H_STATE, &[]),
            crate::i18n::tr(msgs::SSN_H_DEST, &[]),
            crate::i18n::tr(msgs::SSN_H_IF, &[]),
            crate::i18n::tr(msgs::SSN_H_GW, &[]),
            "metric".to_string(),
            crate::i18n::tr(msgs::SSN_H_NOTE, &[]),
        ];
        eframe::egui::Grid::new("diff_grid")
            .num_columns(6)
            .striped(true)
            .spacing([14.0, 3.0])
            .show(ui, |ui| {
                for h in &headers {
                    ui.strong(h);
                }
                ui.end_row();
                for a in &report.actions {
                    let (color, mark) = diff_mark(a);
                    ui.colored_label(color, mark);
                    ui.monospace(dest_text(a));
                    let label = match find(a.if_index) {
                        Some(ad) => {
                            let name = ad.alias.clone().unwrap_or_default();
                            let desc = ad.description.clone().unwrap_or_default();
                            if desc.is_empty() { name } else { format!("{name}（{desc}）") }
                        }
                        None => crate::i18n::tr(msgs::SSN_AD_UNKNOWN, &[]),
                    };
                    ui.label(label);
                    ui.monospace(a.gateway.map(|g| g.to_string()).unwrap_or_else(|| "—".into()));
                    ui.monospace(a.metric.to_string());
                    let note = a.note.as_ref().map(dualnic_core::msg::t).unwrap_or_default();
                    ui.label(note);
                    ui.end_row();
                }
            });
    }

    // —— 真实可达网段（只读探测） ——
    if !report.reachable.is_empty() {
        ui.add_space(8.0);
        ui.strong(crate::i18n::tr(msgs::SSN_REACHABLE_TITLE, &[&report.reachable.len()]));
        render_reachable_table(ui, report);
    }

    ui.add_space(8.0);
    eframe::egui::CollapsingHeader::new(crate::i18n::tr(
        msgs::SSN_ADAPTERS_TITLE,
        &[&report.adapters.len()],
    ))
    .default_open(false)
    .show(ui, |ui| {
        let headers = [
            "if_idx".to_string(),
            "up".to_string(),
            crate::i18n::tr(msgs::SSN_H_ALIAS, &[]),
            crate::i18n::tr(msgs::SSN_H_DESC, &[]),
            "IP".to_string(),
            crate::i18n::tr(msgs::SSN_H_GW, &[]),
            "GUID".to_string(),
        ];
        eframe::egui::Grid::new("adapter_grid")
            .num_columns(7)
            .striped(true)
            .spacing([10.0, 3.0])
            .show(ui, |ui| {
                for h in &headers {
                    ui.strong(h);
                }
                ui.end_row();
                for a in &report.adapters {
                    ui.monospace(a.if_index.to_string());
                    ui.label(if a.oper_up { "up" } else { "down" });
                    ui.label(a.alias.as_deref().unwrap_or("—"));
                    ui.label(a.description.as_deref().unwrap_or("—"));
                    ui.monospace(a.primary_ipv4.map(|v| v.to_string()).unwrap_or_else(|| "—".into()));
                    ui.monospace(a.gateway_ipv4.map(|v| v.to_string()).unwrap_or_else(|| "—".into()));
                    ui.add(
                        eframe::egui::Label::new(
                            a.guid.as_deref().unwrap_or(&crate::i18n::tr(msgs::SSN_NO_GUID, &[])),
                        )
                        .selectable(true),
                    );
                    ui.end_row();
                }
            });
    });
}

fn role_card(ui: &mut eframe::egui::Ui, title: &str, card: Option<&ResolvedCard>) {
    eframe::egui::Frame::NONE
        .fill(eframe::egui::Color32::from_rgb(40, 44, 52))
        .inner_margin(eframe::egui::Margin::same(8))
        .show(ui, |ui| {
            // 占满整行宽：长字段（描述/GUID）随窗口伸展，不再横向截断。
            ui.set_min_width(ui.available_width());
            ui.strong(title);
            let Some(c) = card else {
                ui.label(crate::i18n::tr(msgs::SSN_NO_NIC, &[]));
                return;
            };
            let none = "—".to_string();
            let no_guid = crate::i18n::tr(msgs::SSN_NO_GUID, &[]);
            let fmt = |m: msgs::Mid, v: &String| crate::i18n::tr(m, &[v]);
            let alias = c.alias.as_deref().unwrap_or(&none);
            ui.label(fmt(msgs::SSN_FC_ALIAS, &alias.to_string()));
            let desc = c.description.as_deref().unwrap_or(&none);
            ui.label(fmt(msgs::SSN_FC_DESC, &desc.to_string()));
            let ip = c.ip.map(|v| v.to_string()).unwrap_or_else(|| none.clone());
            ui.monospace(fmt(msgs::SSN_FC_IP, &ip));
            let gw = c.gateway.map(|v| v.to_string()).unwrap_or_else(|| none.clone());
            ui.monospace(fmt(msgs::SSN_FC_GW, &gw));
            let ifidx = c.if_index.to_string();
            ui.monospace(fmt(msgs::SSN_FC_IFINDEX, &ifidx));
            let guid = c.guid.as_deref().unwrap_or(&no_guid);
            ui.add(eframe::egui::Label::new(fmt(msgs::SSN_FC_GUID, &guid.to_string())).selectable(true));
        });
}

fn dest_text(a: &DiffAction) -> String {
    a.dest.map(|p| p.to_string()).unwrap_or_else(|| "—".into())
}

/// 动作 → (颜色, 标记字)
fn diff_mark(a: &DiffAction) -> (eframe::egui::Color32, String) {
    if a.ok {
        return (eframe::egui::Color32::from_rgb(60, 160, 90), crate::i18n::tr(msgs::SSN_MARK_DONE, &[]));
    }
    if a.recommended {
        return (eframe::egui::Color32::from_rgb(230, 90, 70), crate::i18n::tr(msgs::SSN_MARK_FIX, &[]));
    }
    match a.kind {
        DiffKind::ProtectedDefaultSeen => {
            (eframe::egui::Color32::from_rgb(220, 150, 60), crate::i18n::tr(msgs::SSN_MARK_KEEP, &[]))
        }
        _ => (eframe::egui::Color32::from_rgb(200, 160, 60), crate::i18n::tr(msgs::SSN_MARK_WATCH, &[])),
    }
}

// ──────────────────────────── CJK 字体 ────────────────────────────

/// 加载一个系统 CJK 字体（egui 内置字体不含中文字形），失败静默回退内置字体。
fn install_cjk_fonts(ctx: &eframe::egui::Context) {
    use eframe::egui::{FontData, FontDefinitions, FontFamily};

    let candidates = [
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simsun.ttc",
    ];
    let bytes = candidates.iter().find_map(|p| std::fs::read(p).ok());
    let Some(bytes) = bytes else { return };

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert("cjk".to_owned(), Arc::new(FontData::from_owned(bytes)));
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts.families.entry(family).or_default().insert(0, "cjk".to_owned());
    }
    ctx.set_fonts(fonts);
}

// ──────────────────────────── 物理网卡标记：后台加载/保存 + 提示判定 ────────────────────────────

/// 后台加载物理网卡标记页数据：适配器列表（GetSnapshot）+ 已保存标记（GetPhysicalNics）。
fn gui_spawn_physical_load(ctx: eframe::egui::Context, state: Arc<Mutex<PhysicalNicsState>>) {
    {
        let mut s = state.lock().unwrap();
        if s.loading {
            return;
        }
        s.loading = true;
    }
    std::thread::spawn(move || {
        let adapters = crate::client::get_snapshot().map(|d| d.report.adapters).unwrap_or_default();
        let saved = crate::client::get_physical_nics().unwrap_or_default();
        let mut s = state.lock().unwrap();
        s.adapters = adapters;
        s.saved_guids = saved.clone();
        s.checked = saved.into_iter().collect();
        s.loading = false;
        s.loaded = true;
        ctx.request_repaint();
    });
}

/// 后台保存物理网卡标记集合。
fn gui_spawn_physical_save(ctx: eframe::egui::Context, state: Arc<Mutex<PhysicalNicsState>>, guids: Vec<String>) {
    {
        let mut s = state.lock().unwrap();
        if s.saving {
            return;
        }
        s.saving = true;
        s.outcome = None;
    }
    std::thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let token = crate::client::handshake().map_err(|e| e.to_string())?;
            crate::client::set_physical_nics(&token, &guids).map_err(|e| e.to_string())?;
            Ok(())
        })();
        let mut s = state.lock().unwrap();
        s.saving = false;
        s.outcome = Some(result);
        s.saved_guids = guids;
        ctx.request_repaint();
    });
}

/// 该网卡是否「看起来像虚拟卡」——仅用于**告警提示**，不参与物理卡判定（判定只看用户标记）。
fn adapter_looks_virtual(a: &dualnic_core::diff::AdapterView) -> bool {
    // 明确的虚拟类型直接判虚拟。
    if let Some(t) = a.if_type {
        if matches!(t, 24 | 53 | 131 | 268 | 243) {
            return true;
        }
    }
    let name = [a.alias.as_deref(), a.description.as_deref()]
        .iter().flatten().copied().collect::<Vec<_>>().join(" ").to_lowercase();
    const VIRTUAL: [&str; 13] = [
        "vehernet", "vether", "virtual", "tunnel", "wireguard", "easetun", "bluetooth",
        "loopback", "local area connection", "wifi direct", "bluetooth device", "vpn", "hyper-v",
    ];
    VIRTUAL.iter().any(|k| name.contains(k))
}

/// 网卡显示名（连接名 + 描述）。
fn adapter_display_name(a: &dualnic_core::diff::AdapterView) -> String {
    let base = a.alias.clone().unwrap_or_else(|| "—".to_string());
    match &a.description {
        Some(d) if !d.is_empty() => format!("{base}（{d}）"),
        _ => base,
    }
}
