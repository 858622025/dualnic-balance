//! GUI 消息常量登记表（msgid/msgno 集中在此，代码不写裸数字）。
//!
//! - 词条：`lang/SC.ini`（简体）+ `lang/en.ini`（英文），**两文件必须同步**；
//! - 参数含义：`docs/消息目录.md`（新增消息先在该登记表领号）；
//! - 调用：`crate::i18n::tr(msgs::STA_XXX, &[&a, &b])`——args 与模板 `&1..&n` 一一对应。

/// (msgid, msgno) 二元组。
pub type Mid = (&'static str, u32);

// ── DNCMN 跨页公共 ──
pub const CMN_FOOTER_EDITING: Mid = ("DNCMN", 1); // &1=方案名
pub const CMN_FOOTER_ACTIVE: Mid = ("DNCMN", 2); // &1=方案名
pub const CMN_TAB_STATUS: Mid = ("DNCMN", 3);
pub const CMN_TAB_RISK: Mid = ("DNCMN", 4);
pub const CMN_TAB_SNAPSHOT: Mid = ("DNCMN", 5);
pub const CMN_TAB_CONFIG: Mid = ("DNCMN", 6);
pub const CMN_TAB_PHY: Mid = ("DNCMN", 7);
pub const CMN_TAB_DIAG: Mid = ("DNCMN", 8);
pub const CMN_TAB_EVENTS: Mid = ("DNCMN", 9);
pub const CMN_TAB_SETTINGS: Mid = ("DNCMN", 10);
pub const CMN_LOADING_SHORT: Mid = ("DNCMN", 11);
pub const CMN_FOOTER_WARN: Mid = ("DNCMN", 12);
pub const CMN_AGO_S: Mid = ("DNCMN", 13); // &1=秒
pub const CMN_AGO_M: Mid = ("DNCMN", 14); // &1=分
pub const CMN_AGO_H: Mid = ("DNCMN", 15); // &1=时

// ── DNSTA 状态页 ──
pub const STA_HINT: Mid = ("DNSTA", 1);
pub const STA_FIRST_POLL: Mid = ("DNSTA", 2);
pub const STA_UNREACHABLE: Mid = ("DNSTA", 3); // &1=错误
pub const STA_REFRESH_BTN: Mid = ("DNSTA", 4);
pub const STA_LAST_OK: Mid = ("DNSTA", 5); // &1=相对时间
pub const STA_NEVER: Mid = ("DNSTA", 6);
pub const STA_PAUSE: Mid = ("DNSTA", 7);
pub const STA_RESUME: Mid = ("DNSTA", 8);
pub const STA_PAUSED_NOTE: Mid = ("DNSTA", 9);
pub const STA_RUNNING: Mid = ("DNSTA", 10);
pub const STA_KV_LISTEN: Mid = ("DNSTA", 11);
pub const STA_KV_PROTO: Mid = ("DNSTA", 12);
pub const STA_KV_CONFIG: Mid = ("DNSTA", 13);
pub const STA_CFG_LOADED: Mid = ("DNSTA", 14);
pub const STA_CFG_NOT_LOADED: Mid = ("DNSTA", 15); // &1=原因
pub const STA_CFG_LOADERR_UNKNOWN: Mid = ("DNSTA", 16);
pub const STA_KV_LAN_COUNT: Mid = ("DNSTA", 17);
pub const STA_KV_ROLES: Mid = ("DNSTA", 18);
pub const STA_KV_RECON: Mid = ("DNSTA", 19);

// ── DNRSK 风险页 ──
pub const RSK_READ_ERR_TITLE: Mid = ("DNRSK", 1);
pub const RSK_READ_ERR_DETAIL: Mid = ("DNRSK", 2); // &1=错误
pub const RSK_MULTI_TITLE: Mid = ("DNRSK", 3); // &1=默认路由条数
pub const RSK_MULTI_DETAIL: Mid = ("DNRSK", 4);
pub const RSK_NONE_TITLE: Mid = ("DNRSK", 5);
pub const RSK_NONE_DETAIL: Mid = ("DNRSK", 6);
pub const RSK_SINGLE_TITLE: Mid = ("DNRSK", 7);
pub const RSK_UNREACHABLE: Mid = ("DNRSK", 8); // &1=错误
pub const RSK_H_IF: Mid = ("DNRSK", 18);
pub const RSK_H_ALIAS: Mid = ("DNRSK", 19);
pub const RSK_H_DESC: Mid = ("DNRSK", 20);
pub const RSK_H_GW: Mid = ("DNRSK", 21);
pub const RSK_H_PROTO: Mid = ("DNRSK", 22);
pub const RSK_FIRST_POLL: Mid = ("DNRSK", 9);
pub const RSK_VERSION_OLD: Mid = ("DNRSK", 10);
pub const RSK_RECONCILE_BTN: Mid = ("DNRSK", 11);
pub const RSK_BTN_TIP_OK: Mid = ("DNRSK", 12);
pub const RSK_BTN_TIP_OFF: Mid = ("DNRSK", 13);
pub const RSK_RESULT_FAIL: Mid = ("DNRSK", 14); // &1=错误
pub const RSK_RESULT_UNCHANGED: Mid = ("DNRSK", 15);
pub const RSK_RESULT_DONE: Mid = ("DNRSK", 16); // &1..&4=删默认/删前缀/加前缀/固定 metric
pub const RSK_RESULT_ERR_UNKNOWN: Mid = ("DNRSK", 17);

// ── DNSSN 策略预览页 ──
pub const SSN_REFRESH_BTN: Mid = ("DNSSN", 1);
pub const SSN_FETCHING: Mid = ("DNSSN", 2);
pub const SSN_NO_SNAPSHOT: Mid = ("DNSSN", 3);
pub const SSN_UNAVAILABLE: Mid = ("DNSSN", 4); // &1=错误
pub const SSN_LPM_HEADING: Mid = ("DNSSN", 5);
pub const SSN_LPM_LABEL: Mid = ("DNSSN", 6);
pub const SSN_LPM_HINT: Mid = ("DNSSN", 7);
pub const SSN_LAN_LIST: Mid = ("DNSSN", 8); // &1=条数 &2=网段列表
pub const SSN_LPM_LAN: Mid = ("DNSSN", 9); // &1=IP &2=前缀
pub const SSN_LPM_WAN: Mid = ("DNSSN", 10); // &1=IP
pub const SSN_LPM_INVALID: Mid = ("DNSSN", 11); // &1=输入
pub const SSN_READ_PARTIAL: Mid = ("DNSSN", 12); // &1=错误
pub const SSN_ROLE_WAN_TITLE: Mid = ("DNSSN", 13);
pub const SSN_ROLE_LAN_TITLE: Mid = ("DNSSN", 14);
pub const SSN_RESOLVE_ERROR: Mid = ("DNSSN", 15); // &1=错误
pub const SSN_DIFF_HEADING: Mid = ("DNSSN", 16);
pub const SSN_NO_DIFF: Mid = ("DNSSN", 17);
pub const SSN_H_STATE: Mid = ("DNSSN", 18);
pub const SSN_H_DEST: Mid = ("DNSSN", 19);
pub const SSN_H_IF: Mid = ("DNSSN", 20);
pub const SSN_H_GW: Mid = ("DNSSN", 21);
pub const SSN_H_NOTE: Mid = ("DNSSN", 22);
pub const SSN_MARK_DONE: Mid = ("DNSSN", 23);
pub const SSN_MARK_FIX: Mid = ("DNSSN", 24);
pub const SSN_MARK_KEEP: Mid = ("DNSSN", 25);
pub const SSN_MARK_WATCH: Mid = ("DNSSN", 26);
pub const SSN_AD_UNKNOWN: Mid = ("DNSSN", 27);
pub const SSN_REACHABLE_TITLE: Mid = ("DNSSN", 28); // &1=条数
pub const SSN_H_ROLE: Mid = ("DNSSN", 29);
pub const SSN_H_NIC: Mid = ("DNSSN", 30);
pub const SSN_H_SUBNET: Mid = ("DNSSN", 31);
pub const SSN_H_SOURCE: Mid = ("DNSSN", 32);
pub const SSN_ROLE_OTHER: Mid = ("DNSSN", 33);
pub const SSN_SRC_PRIMARY: Mid = ("DNSSN", 34);
pub const SSN_SRC_ONLINK: Mid = ("DNSSN", 35);
pub const SSN_ADAPTERS_TITLE: Mid = ("DNSSN", 36); // &1=网卡数
pub const SSN_H_ALIAS: Mid = ("DNSSN", 37);
pub const SSN_H_DESC: Mid = ("DNSSN", 38);
pub const SSN_NO_GUID: Mid = ("DNSSN", 39);
pub const SSN_NO_NIC: Mid = ("DNSSN", 40);
pub const SSN_FC_ALIAS: Mid = ("DNSSN", 41); // &1=值
pub const SSN_FC_DESC: Mid = ("DNSSN", 42); // &1=值
pub const SSN_FC_IP: Mid = ("DNSSN", 43); // &1=值
pub const SSN_FC_GW: Mid = ("DNSSN", 44); // &1=值
pub const SSN_FC_IFINDEX: Mid = ("DNSSN", 45); // &1=值
pub const SSN_FC_GUID: Mid = ("DNSSN", 46); // &1=值

// ── DNDIA 诊断页 ──
pub const DIA_HINT: Mid = ("DNDIA", 1);
pub const DIA_RUN_BTN: Mid = ("DNDIA", 2);
pub const DIA_RUNNING_BTN: Mid = ("DNDIA", 3);
pub const DIA_RUNNING_PAGE: Mid = ("DNDIA", 4);
pub const DIA_NOT_YET: Mid = ("DNDIA", 5);
pub const DIA_FAILED: Mid = ("DNDIA", 6); // &1=错误
pub const DIA_CMD_ERROR: Mid = ("DNDIA", 7); // &1=错误
pub const DIA_UNRESOLVED: Mid = ("DNDIA", 8);
pub const DIA_WAN_LABEL: Mid = ("DNDIA", 9); // &1=网卡
pub const DIA_LAN_LABEL: Mid = ("DNDIA", 10); // &1=网卡
pub const DIA_DIFF_COUNT: Mid = ("DNDIA", 11); // &1=条数
pub const DIA_RESOLVE_ERROR: Mid = ("DNDIA", 12); // &1=错误
pub const DIA_ROUTE_PRINT_HDR: Mid = ("DNDIA", 13); // &1=行数
pub const DIA_NETIF_HDR: Mid = ("DNDIA", 14); // &1=行数
pub const DIA_NETCFG_HDR: Mid = ("DNDIA", 15); // &1=行数
pub const DIA_EXPORT_BTN: Mid = ("DNDIA", 16);
pub const DIA_EXPORTED: Mid = ("DNDIA", 17); // &1=路径
pub const DIA_EXPORT_FAILED: Mid = ("DNDIA", 18); // &1=错误

// ── DNEVT 事件页 ──
pub const EVT_HINT: Mid = ("DNEVT", 1);
pub const EVT_REFRESH_BTN: Mid = ("DNEVT", 2);
pub const EVT_FETCHING: Mid = ("DNEVT", 3);
pub const EVT_DB_SIZE: Mid = ("DNEVT", 4); // &1=大小
pub const EVT_DB_UNKNOWN: Mid = ("DNEVT", 5);
pub const EVT_CLEAR_BTN: Mid = ("DNEVT", 6);
pub const EVT_CLEARING: Mid = ("DNEVT", 7);
pub const EVT_CLEARED: Mid = ("DNEVT", 8);
pub const EVT_CLEAR_FAILED: Mid = ("DNEVT", 9); // &1=错误
pub const EVT_NONE_YET: Mid = ("DNEVT", 10);
pub const EVT_UNAVAILABLE: Mid = ("DNEVT", 11); // &1=错误
pub const EVT_EMPTY: Mid = ("DNEVT", 12);
pub const EVT_H_TIME: Mid = ("DNEVT", 13);
pub const EVT_H_LEVEL: Mid = ("DNEVT", 14);
pub const EVT_H_SOURCE: Mid = ("DNEVT", 15);
pub const EVT_H_MESSAGE: Mid = ("DNEVT", 16);
pub const EVT_CLEAR_WIN_TITLE: Mid = ("DNEVT", 17);
pub const EVT_CLEAR_CONFIRM: Mid = ("DNEVT", 18); // &1=库大小
pub const EVT_CLEAR_CONFIRM_BTN: Mid = ("DNEVT", 19);
pub const EVT_CANCEL: Mid = ("DNEVT", 20);

// ── DNWIZ 配置向导页（方案栏/弹窗/导入导出） ──
pub const WIZ_PROFILE_LABEL: Mid = ("DNWIZ", 1);
pub const WIZ_LOADING_PROFILES: Mid = ("DNWIZ", 2);
pub const WIZ_NO_PROFILE: Mid = ("DNWIZ", 3);
pub const WIZ_ENABLE_BTN: Mid = ("DNWIZ", 4);
pub const WIZ_DUPLICATE_BTN: Mid = ("DNWIZ", 5);
pub const WIZ_NEW_BTN: Mid = ("DNWIZ", 6);
pub const WIZ_DELETE_BTN: Mid = ("DNWIZ", 7);
pub const WIZ_RENAME_BTN: Mid = ("DNWIZ", 8);
pub const WIZ_AUTOMATCH_BTN: Mid = ("DNWIZ", 9);
pub const WIZ_SWITCH_TITLE: Mid = ("DNWIZ", 10);
pub const WIZ_SWITCH_CONFIRM: Mid = ("DNWIZ", 11); // &1=方案名
pub const WIZ_SWITCH_UNSAVED: Mid = ("DNWIZ", 12);
pub const WIZ_SWITCH_KEEP: Mid = ("DNWIZ", 13); // &1=当前生效方案
pub const WIZ_CONFIRM_ENABLE: Mid = ("DNWIZ", 14);
pub const WIZ_CANCEL: Mid = ("DNWIZ", 15);
pub const WIZ_UNSAVED_TITLE: Mid = ("DNWIZ", 16);
pub const WIZ_SWITCH_DISCARD_WARN: Mid = ("DNWIZ", 17); // &1=方案名
pub const WIZ_DISCARD_BTN: Mid = ("DNWIZ", 18);
pub const WIZ_STAY_BTN: Mid = ("DNWIZ", 19);
pub const WIZ_DELETE_TITLE: Mid = ("DNWIZ", 20);
pub const WIZ_DELETE_CONFIRM: Mid = ("DNWIZ", 21); // &1=方案名
pub const WIZ_DELETE_UNSAVED: Mid = ("DNWIZ", 22);
pub const WIZ_CONFIRM_DELETE: Mid = ("DNWIZ", 23);
pub const WIZ_ENV_FAIL: Mid = ("DNWIZ", 24); // &1=错误
pub const WIZ_ENV_MATCHED: Mid = ("DNWIZ", 25); // &1=方案名
pub const WIZ_ENV_NO_MATCH: Mid = ("DNWIZ", 26);
pub const WIZ_EXPORT_BTN: Mid = ("DNWIZ", 27);
pub const WIZ_IMPORT_BTN: Mid = ("DNWIZ", 28);
pub const WIZ_EXPORTED: Mid = ("DNWIZ", 29); // &1=路径
pub const WIZ_EXPORT_FAIL: Mid = ("DNWIZ", 30); // &1=错误
pub const WIZ_EXPORT_CANCELLED: Mid = ("DNWIZ", 31);
pub const WIZ_EXPORT_EMPTY: Mid = ("DNWIZ", 32);
pub const WIZ_IMPORT_CANCELLED: Mid = ("DNWIZ", 33);
pub const WIZ_IMPORT_READ_FAIL: Mid = ("DNWIZ", 34); // &1=错误
pub const WIZ_IMPORTED: Mid = ("DNWIZ", 35);
pub const WIZ_IMPORT_FAIL: Mid = ("DNWIZ", 36); // &1=错误
pub const WIZ_LEAVE_CONFIRM: Mid = ("DNWIZ", 37);
pub const WIZ_LEAVE_DETAIL: Mid = ("DNWIZ", 38);
pub const WIZ_LEAVE_DISCARD: Mid = ("DNWIZ", 39);
pub const WIZ_LEAVE_STAY: Mid = ("DNWIZ", 40);
pub const WIZ_RENAME_PROMPT: Mid = ("DNWIZ", 41); // &1=旧名
pub const WIZ_RENAME_SAVE: Mid = ("DNWIZ", 42);

// ── DNPHY 网卡标记页 ──
pub const PHY_TITLE: Mid = ("DNPHY", 1);
pub const PHY_HINT1: Mid = ("DNPHY", 2);
pub const PHY_HINT2: Mid = ("DNPHY", 3);
pub const PHY_ENUMING: Mid = ("DNPHY", 4);
pub const PHY_NONE: Mid = ("DNPHY", 5);
pub const PHY_MAY_VIRTUAL: Mid = ("DNPHY", 6);
pub const PHY_SAVE_BTN: Mid = ("DNPHY", 7);
pub const PHY_SAVING: Mid = ("DNPHY", 8);
pub const PHY_RELOAD_BTN: Mid = ("DNPHY", 9);
pub const PHY_SAVED: Mid = ("DNPHY", 10);
pub const PHY_SAVE_FAILED: Mid = ("DNPHY", 11); // &1=错误

// ── DNSET 设置页正文 ──
pub const SET_LANG_HEADING: Mid = ("DNSET", 1);
pub const SET_LANG_SYSTEM: Mid = ("DNSET", 2); // &1=探测到的代码
pub const SET_DEPLOY_TITLE: Mid = ("DNSET", 3);
pub const SET_DEPLOY_HINT: Mid = ("DNSET", 4);
pub const SET_SVC_TITLE: Mid = ("DNSET", 5);
pub const SET_PROBING: Mid = ("DNSET", 6);
pub const SET_NOT_PROBED: Mid = ("DNSET", 7);
pub const SET_SVC_MISSING: Mid = ("DNSET", 8);
pub const SET_INSTALL_BTN: Mid = ("DNSET", 9);
pub const SET_UNINSTALL_BTN: Mid = ("DNSET", 10);
pub const SET_SVC_NOTE: Mid = ("DNSET", 11);
pub const SET_AUTO_TITLE: Mid = ("DNSET", 12);
pub const SET_AUTO_ON: Mid = ("DNSET", 13);
pub const SET_AUTO_OFF: Mid = ("DNSET", 14);
pub const SET_AUTO_REG_BTN: Mid = ("DNSET", 15);
pub const SET_AUTO_UNREG_BTN: Mid = ("DNSET", 16);
pub const SET_AUTO_NOTE: Mid = ("DNSET", 17);
pub const SET_BUSY: Mid = ("DNSET", 18); // &1=动作名
pub const SET_FAILED: Mid = ("DNSET", 19); // &1=错误
pub const SET_UNINSTALL_WIN: Mid = ("DNSET", 20);
pub const SET_UNINSTALL_WARN: Mid = ("DNSET", 21);
pub const SET_UNINSTALL_CONFIRM: Mid = ("DNSET", 22);
pub const SET_UNINSTALL_CONFIRM_BTN: Mid = ("DNSET", 23);
pub const SET_SVC_EXE_MISSING: Mid = ("DNSET", 24);
pub const SET_INSTALL_FAIL_EXIT: Mid = ("DNSET", 25); // &1=退出码
pub const SET_INSTALL_FAIL_TAIL: Mid = ("DNSET", 26); // &1=退出码 &2=日志尾
pub const SET_INSTALL_RUNNING_OK: Mid = ("DNSET", 27);
pub const SET_INSTALL_STOPPED_OK: Mid = ("DNSET", 28);
pub const SET_INSTALL_NOT_APPLIED: Mid = ("DNSET", 29);
pub const SET_UNINSTALL_OK: Mid = ("DNSET", 30);
pub const SET_UNINSTALL_STILL_THERE: Mid = ("DNSET", 31);
pub const SET_UNINSTALL_NOT_DONE: Mid = ("DNSET", 32);
pub const SET_UNINSTALL_FAIL_EXIT: Mid = ("DNSET", 33); // &1=退出码
pub const SET_UNINSTALL_FAIL_TAIL: Mid = ("DNSET", 34); // &1=退出码 &2=日志尾
pub const SET_EXE_PATH_FAIL: Mid = ("DNSET", 35); // &1=错误
pub const SET_AUTOREG_FAIL_EXIT: Mid = ("DNSET", 36); // &1=退出码
pub const SET_AUTOREG_OK: Mid = ("DNSET", 37);
pub const SET_AUTOREG_NOT_APPLIED: Mid = ("DNSET", 38);
pub const SET_AUTOUNREG_FAIL_EXIT: Mid = ("DNSET", 39); // &1=退出码
pub const SET_AUTOUNREG_OK: Mid = ("DNSET", 40);
pub const SET_AUTOUNREG_NOT_APPLIED: Mid = ("DNSET", 41);
pub const SET_BANNER_SVC_IPC_WAIT: Mid = ("DNSET", 42);
pub const SET_BANNER_SVC_STOPPED: Mid = ("DNSET", 43);
pub const SET_BANNER_OPTOUT: Mid = ("DNSET", 44);
pub const SET_BANNER_TEMP_ENGINE: Mid = ("DNSET", 45);
pub const SET_BANNER_ENGINE_IPC_FAIL: Mid = ("DNSET", 46);
pub const SET_BANNER_OPTOUT_SAVED: Mid = ("DNSET", 47); // &1=原消息
pub const SET_UNINSTALL_NOT_DONE_TAIL: Mid = ("DNSET", 48); // &1=日志尾
pub const SET_SVC_LBL_NOT_INSTALLED: Mid = ("DNSET", 49);
pub const SET_SVC_LBL_RUNNING: Mid = ("DNSET", 50);
pub const SET_SVC_LBL_STOPPED: Mid = ("DNSET", 51);
pub const SET_TEMP_DIR_FAIL: Mid = ("DNSET", 52); // &1=错误
pub const SET_WRITE_SCRIPT_FAIL: Mid = ("DNSET", 53); // &1=错误
pub const SET_PS_START_FAIL: Mid = ("DNSET", 54); // &1=错误
pub const SET_SCRIPT_EXIT_CODE: Mid = ("DNSET", 55); // &1=码
pub const SET_UAC_DENIED: Mid = ("DNSET", 56);
pub const SET_ELEVATE_FAIL: Mid = ("DNSET", 57); // &1=错误码
pub const SET_ELEVATE_NO_HANDLE: Mid = ("DNSET", 58);
pub const SET_SCRIPT_TIMEOUT: Mid = ("DNSET", 59); // &1=秒
pub const SET_ENGINE_START_FAIL: Mid = ("DNSET", 60); // &1=错误
pub const SET_RESTART_SCHED_FAIL: Mid = ("DNSET", 61); // &1=错误

// ── DNTRAY 托盘/公共窗口 ──
pub const TRAY_ONBOARD_TITLE: Mid = ("DNTRAY", 1);
pub const TRAY_ONBOARD_L1: Mid = ("DNTRAY", 2);
pub const TRAY_ONBOARD_L2: Mid = ("DNTRAY", 3);
pub const TRAY_ONBOARD_L3: Mid = ("DNTRAY", 4);
pub const TRAY_ONBOARD_SAVING: Mid = ("DNTRAY", 5);
pub const TRAY_ONBOARD_GO: Mid = ("DNTRAY", 6);
pub const TRAY_ALREADY_RUNNING: Mid = ("DNTRAY", 7);
pub const TRAY_SHOW: Mid = ("DNTRAY", 8);
pub const TRAY_PAUSE_RESUME: Mid = ("DNTRAY", 9);
pub const TRAY_QUIT: Mid = ("DNTRAY", 10);

// ── DNWIZ 向导主体（续：43 起） ──
pub const WIZ_RELOAD_BTN: Mid = ("DNWIZ", 43);
pub const WIZ_RELOADING: Mid = ("DNWIZ", 44);
pub const WIZ_BASELINE_FAIL: Mid = ("DNWIZ", 45); // &1=错误
pub const WIZ_BASELINE_READING: Mid = ("DNWIZ", 46);
pub const WIZ_NO_BASELINE: Mid = ("DNWIZ", 47);
pub const WIZ_ENUMING: Mid = ("DNWIZ", 48);
pub const WIZ_PREPARING: Mid = ("DNWIZ", 49);
pub const WIZ_NO_ADAPTERS: Mid = ("DNWIZ", 50);
pub const WIZ_SECTION_QUICK: Mid = ("DNWIZ", 51);
pub const WIZ_AUTOMATCH_PROBING: Mid = ("DNWIZ", 52);
pub const WIZ_AUTODETECT_BTN: Mid = ("DNWIZ", 53);
pub const WIZ_RESET_ALL_BTN: Mid = ("DNWIZ", 54);
pub const WIZ_NO_ROLE_NOTE: Mid = ("DNWIZ", 55);
pub const WIZ_WAN_ONLY_NOTE: Mid = ("DNWIZ", 56);
pub const WIZ_NO_PHYSICAL: Mid = ("DNWIZ", 57);
pub const WIZ_VIRTUAL_HDR: Mid = ("DNWIZ", 58); // &1=数量
pub const WIZ_LAN_WILL_SAVE: Mid = ("DNWIZ", 59); // &1=条数 &2=明细
pub const WIZ_ROLE_NONE: Mid = ("DNWIZ", 60);
pub const WIZ_WILL_SAVE_AS: Mid = ("DNWIZ", 61); // &1=WAN &2=LAN
pub const WIZ_OTHERS_HDR: Mid = ("DNWIZ", 62); // &1=数量
pub const WIZ_DISCONNECTED: Mid = ("DNWIZ", 63);
pub const WIZ_SECTION_ADV: Mid = ("DNWIZ", 64);
pub const WIZ_SAVE_BTN: Mid = ("DNWIZ", 65);
pub const WIZ_VALID_OK: Mid = ("DNWIZ", 66);
pub const WIZ_VALID_WARN: Mid = ("DNWIZ", 67); // &1=首条错误
pub const WIZ_SAVING: Mid = ("DNWIZ", 68);
pub const WIZ_SAVE_NOTE: Mid = ("DNWIZ", 69);
pub const WIZ_NOTICE_EMPTY: Mid = ("DNWIZ", 70);
pub const WIZ_ROLE_WAN_HDR: Mid = ("DNWIZ", 71);
pub const WIZ_ROLE_LAN_HDR: Mid = ("DNWIZ", 72);
pub const WIZ_ROLE_DEFAULT_HDR: Mid = ("DNWIZ", 73);
pub const WIZ_ROLE_NIC_HDR: Mid = ("DNWIZ", 74);
pub const WIZ_PROBE_TARGET: Mid = ("DNWIZ", 75);
pub const WIZ_PROBE_NOTE: Mid = ("DNWIZ", 76);
pub const WIZ_REACHABLE_HDR: Mid = ("DNWIZ", 77);
pub const WIZ_REACH_EMPTY: Mid = ("DNWIZ", 78);
pub const WIZ_SRC_PRIMARY_SHORT: Mid = ("DNWIZ", 79);
pub const WIZ_ALREADY_IN_LIST: Mid = ("DNWIZ", 80);
pub const WIZ_ADOPT_BTN: Mid = ("DNWIZ", 81);
pub const WIZ_LAN_MANUAL_HDR: Mid = ("DNWIZ", 82);
pub const WIZ_NOTE_HINT: Mid = ("DNWIZ", 83);
pub const WIZ_VIA_AUTO: Mid = ("DNWIZ", 84);
pub const WIZ_INVALID_CIDR: Mid = ("DNWIZ", 85);
pub const WIZ_VIA_EMPTY_HINT: Mid = ("DNWIZ", 86);
pub const WIZ_ADD_BTN: Mid = ("DNWIZ", 87);
pub const WIZ_ADD_EMPTY_ERR: Mid = ("DNWIZ", 88);
pub const WIZ_ADD_INVALID_ERR: Mid = ("DNWIZ", 89); // &1=错误
pub const WIZ_ADD_DUP_ERR: Mid = ("DNWIZ", 90); // &1=网段
pub const WIZ_ADD_OK: Mid = ("DNWIZ", 91);
pub const WIZ_METRIC_HDR: Mid = ("DNWIZ", 92);
pub const WIZ_RECON_HDR: Mid = ("DNWIZ", 93);
pub const WIZ_RECON_NOTE: Mid = ("DNWIZ", 94);
pub const WIZ_PROTECTED_HDR: Mid = ("DNWIZ", 95);
pub const WIZ_PROTECTED_ADD_LABEL: Mid = ("DNWIZ", 96);
pub const WIZ_ADD_SHORT_BTN: Mid = ("DNWIZ", 97);
pub const WIZ_PROTECTED_COUNT: Mid = ("DNWIZ", 98); // &1=数量
pub const WIZ_PV_UNSET: Mid = ("DNWIZ", 99);
pub const WIZ_VIA_UNENUMERATED: Mid = ("DNWIZ", 100);
pub const WIZ_PROBE_FAIL_FALLBACK: Mid = ("DNWIZ", 101); // &1=探测目标
pub const WIZ_PROBE_OK: Mid = ("DNWIZ", 102); // &1=探测目标 &2=可达网卡
pub const WIZ_HANDSHAKE_FAIL: Mid = ("DNWIZ", 103); // &1=错误
pub const WIZ_REHANDSHAKE: Mid = ("DNWIZ", 104); // &1=错误
pub const WIZ_VERSION_OLD: Mid = ("DNWIZ", 105);
pub const WIZ_SAVED_OK: Mid = ("DNWIZ", 106);
pub const WIZ_SAVE_FAIL: Mid = ("DNWIZ", 107); // &1=错误
pub const WIZ_ERR_ZERO_CIDR: Mid = ("DNWIZ", 108); // &1=行号
pub const WIZ_ERR_DUP: Mid = ("DNWIZ", 109); // &1=行号 &2=网段
pub const WIZ_ERR_NIC_NOT_FOUND: Mid = ("DNWIZ", 110); // &1=行号
pub const WIZ_ERR_LINE: Mid = ("DNWIZ", 111); // &1=行号 &2=错误
pub const WIZ_ERR_WAN_DESC: Mid = ("DNWIZ", 112);
pub const WIZ_ERR_LAN_DESC: Mid = ("DNWIZ", 113);
pub const WIZ_DEFAULT_HINT: Mid = ("DNWIZ", 114);
pub const WIZ_DEFAULT_HINT2: Mid = ("DNWIZ", 115);
pub const WIZ_PAGE_HINT: Mid = ("DNWIZ", 116);
pub const WIZ_PROFILE_EDITING_FMT: Mid = ("DNWIZ", 117); // ✏ &1
pub const WIZ_PAGE_TITLE: Mid = ("DNWIZ", 118);
pub const WIZ_NOTICE_HDR: Mid = ("DNWIZ", 119);
pub const WIZ_PROBE_NAMES_UNK: Mid = ("DNWIZ", 120);
pub const WIZ_SWITCHED_OK: Mid = ("DNWIZ", 121); // &1=方案名
pub const WIZ_CREATED_OK: Mid = ("DNWIZ", 122); // &1=方案名
pub const WIZ_COPIED_OK: Mid = ("DNWIZ", 123); // &1=方案名
pub const WIZ_DELETED_OK: Mid = ("DNWIZ", 124); // &1=方案名
pub const WIZ_AUTOMATCH_SWITCHED: Mid = ("DNWIZ", 125); // &1=方案名
pub const WIZ_AUTOMATCH_CREATED: Mid = ("DNWIZ", 126); // &1=方案名
pub const WIZ_AUTOMATCH_ADOPTED: Mid = ("DNWIZ", 127); // &1=方案名
pub const WIZ_AUTOMATCH_LATEST: Mid = ("DNWIZ", 128);
pub const WIZ_RENAMED_OK: Mid = ("DNWIZ", 129); // &1=旧名 &2=新名
pub const WIZ_ACTION_FAIL: Mid = ("DNWIZ", 130); // &1=错误
