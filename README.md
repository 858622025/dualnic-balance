# DualNIC Balance —— 双网卡内外网分流稳定化工具

> **文档版本：1.0（2026-09-22）** · Win11 绿色便携分发
> 目标：有线 + 无线（+ Hyper-V / easetun 等虚拟网卡）同时在线时，内网流量走指定物理卡、
> 外网走另一指定物理卡；根治「线路稳定却突然失效」，交付形态为可静默分发、有 GUI 的常驻服务。

## 关键结论（设计依据）

- 根因 = Windows **多默认网关 + 死网关自动切换**（微软 KB159168 / Learn）。
- 仅按目的网段分流时，稳定 = **全系统一条默认路由 + 内网前缀静态路由（LPM）+ 固定接口 metric**。
- Windows 没有 Linux `ip rule` 式策略路由；数据面（WinDivert/Wintun）仅当规则需要
  else / 域名 / 应用级时才值得上（已真机验证：改写源地址不能可靠钦定出口）。
- 权威设计文档（OneDrive 同步盘，**不在仓库内**，勿在代码里复制其内容）

## 角色映射约定（重要）

物理网卡角色**必须靠「永久 GUID / 描述关键字」识别，绝不依赖会变化的接口索引**。
角色是**配置项**，不是写死的代码常量。默认策略：

| 角色 | 含义 | 默认识别关键字 |
|---|---|---|
| `wan`（外网） | 持有全系统唯一默认路由 0.0.0.0/0 | 见配置文件 |
| `lan`（内网） | 承载内网前缀路由，不带默认网关 | 见配置文件 |

> 任何按「当前谁持默认路由」反推角色的假设都不能写进代码逻辑——开发机与目标公司环境
> 的角色分配恰好相反，正说明角色必须可配置、按 GUID/描述识别。

## 配置模型（crates/core，可配置化要点）

除 `lan_networks`（内网网段，本就该配）外，一切可变语义都收敛进 `PolicyConfig`，不做代码内写死：

```jsonc
{
  "schema_version": 1,                      // 结构版本；高于程序支持版本时拒绝加载（默认值出口唯一）
  "lan_networks": [
    { "cidr": "10.0.0.0/8", "note": "公司内网" }
  ],
  "wan_adapter": {                          // 角色识别规则：命中其一即算（精确 > 模糊仲裁）
    "role": "wan",
    "matchers": [
      { "by": "guid", "value": "{xxxx-...}" },          // 精确，优先级最高（推荐用 GUID 消除多同名歧义）
      { "by": "desc-contains", "value": "Intel Wi-Fi" } // 模糊兜底（描述关键字为连续子串，注意 "(R)" 等干扰）
    ]
  },
  "lan_adapter": { "role": "lan", "matchers": [ { "by": "desc-contains", "value": "Realtek" } ] },
  "interface_metric": { "lan": 10, "wan": 50 },         // 占位默认；勿假设 lan 必须 < wan
  "reconciliation": {
    "converge_default_route": true,   // 由程序按「是否接管 WAN」自动归一，前台不可配
    "remove_stale_defaults": true,    // 同上；非 WAN、未豁免接口上的过期默认路由 → 清除
    "protected_interfaces": []        // 受保护接口 GUID：对账只读，其上默认路由「保留 + 审计告警」，绝不擅删
  }
}
```

- **角色仲裁 `resolve_roles`**：精确（GUID/名称）命中压过模糊 desc；同一强度命中多块 →
  报 `AmbiguousRole` 提示补 GUID，**绝不「取枚举第一块」**。`config.validate()` 静态拦截
  「单角色多条不同精确规则」「两角色同 GUID」。
- **对账开关前台不可配**：GUI 无对账复选框。`apply_reconciliation_policy` 按「是否接管 WAN」
  归一——接管 WAN = 双收敛全开（生效即收敛）；未接管 WAN = 全关（无 WAN 时收敛 = 删光
  全系统默认路由）；服务唯一写入口 `write_profiles_locked` 落盘前对全容器统一（「默认」除外），
  旧存档在下次写盘时自动纠正。受保护接口是独立维度，不受影响。
- **默认配置是惰性的**：`PolicyConfig::default()`（无配置文件首启）`reconciliation` 全关，
  服务不会据此动任何路由。
- **部署必填**：真实环境把 Hyper-V `vEthernet`、easetun/WireGuard 等虚拟卡 GUID 列入
  `protected_interfaces`，否则对账的 `remove_stale_defaults` 可能清掉其默认路由。

## 功能总览

**路由内核（service，无驱动、可公司分发）**

- 对账自愈真写路由：删非 WAN 多余默认、为 WAN 补默认（DHCP 服务器作下一跳）、
  写内网前缀（on-link 或经 LAN 卡 DHCP 服务器）、固定接口 metric。
- 事件驱动自愈（`NotifyIpInterfaceChange` + 定时兜底 + 防抖 + 指纹幂等）；断线卡只观察不写
  （oper_up 闸）；未识别接口与受保护接口绝不增删改；「暂停分流」随时冻结。
- 手动「一键收敛」；「意图 vs 实际」差异持续可见。
- 事件日志结构化存储（`msgid+msgno+&n 填空` 消息目录，见 `docs/消息目录.md`）：
  SQLite 存消息引用，显示时按界面语言渲染，历史事件原样保留。

**方案体系（core/service）**

- 多方案 Profile（SQLite 容器 schema=2，v1 自动迁移）；**编辑/生效解耦**：保存 ≠ 启用；
  「默认」= 只读不接管占位。
- 环境绑定键 = 物理卡（GUID + DHCP 服务器），网段不参与匹配；自动匹配 → 唯一命中切换、
  0 命中走「认领 → 掉卡抑制 → 自动新建」三级收敛；回公司自动恢复接管。

**GUI（gui，eframe/egui + glow，免 UAC）**

- 八页签：状态 / 风险 / 网卡标记 / 策略预览 / 配置向导 / 诊断 / 事件 / 设置。
- 网卡标记：首启强制引导；疑似物理卡置顶展示；进页签强制刷新（USB 网卡随时插拔）。
- 配置向导：快速设置 + 高级项折叠；规则预览（LPM 可视化）；一键诊断导出；事件日志；
  进页签强制刷新网卡；锁死深色模式（不随系统深浅色切换）。
- 设置页：一键安装/卸载服务、注册/取消登录自启动（脚本嵌入 exe）；托盘暂停/恢复 + 退出；单实例。
- **多语言界面**：简体中文(SC) / 繁體中文(TC) / English(en) / 日本語(ja)，设置页热切换，
  缺省跟随系统语言；全部词条外置 `lang/<代码>.ini`（2 字符代码），`lang/languages.ini`
  为唯一语言清单——新增语言 = 加一行 + 放一个同名 ini，无需改代码；
  缺文件时回退出厂内嵌包（SC/en），再回退兜底语言，最后落到消息代码原文。
- 常驻底栏显示「正在编辑 / 当前生效」方案（跨页签）；诊断页进页自动诊断；
  一键诊断报告导出时跟随界面语言。

**分发形态**

- 服务 `DualNICBalance`：SCM 托管（LocalSystem + 开机自启 + 崩溃自恢复）；
  计划任务 `DualNICBalanceGUI` 登录自启；`setup.bat` 一键装/卸（IT 可静默部署）。
- 绿色便携包 `dist/dualnic-balance-portable.zip`（gui + service exe + setup.bat +
  使用说明 + `lang/` 语言文件五件套）。
- 未装服务时首启自动（弹 UAC）拉起临时后台引擎，仅本次开机有效；常驻请装服务。

## 目录结构

```
dualnic-balance/
├─ Cargo.toml          # workspace 根（默认只构建产品三件套）
├─ crates/
│  ├─ core/            # 纯逻辑：配置模型、路由意图计算、LPM、角色识别，可单测，无 Windows 依赖
│  ├─ service/         # 常驻服务：LocalSystem，路由对账/自愈、网络事件监听、SQLite 配置/日志、IPC 服务端
│  ├─ gui/             # 桌面 GUI（免 UAC），本地 IPC 读状态/下发配置、托盘、事件日志
├─ lang/               # 语言文件：languages.ini 语言清单 + 各语言词条（随包分发）
├─ docs/               # 部署与分发文档
└─ scripts/            # 服务安装/卸载脚本（GUI 设置页也内嵌同等功能）
```

## 开发环境

- Rust stable，target `x86_64-pc-windows-msvc`（依赖 VS 的 MSVC/Windows SDK，本机已具备）。
- Windows API crate：`windows`（IP Helper / NLM / 路由 API）。
- GUI 渲染后端用 **glow**（OpenGL）而非 eframe 默认 wgpu——wgpu-hal 30 在 windows crate
  多版本下存在编译冲突，且本工具对渲染无性能要求，glow 更轻。
- **仓库目录不要放在 OneDrive 等同步盘内**：避免对 `target/` 的同步锁与文件冲突。

## 运行与验证

- `dualnic-service`：无参 = SCM 服务模式；`--console` = 前台调试
  （默认监听 127.0.0.1:44175；写路由需管理员；先 `sc stop DualNICBalance` 释放端口）。
- `dualnic-gui`：窗口模式（每 2s 轮询）；`--headless` = 单发 GetStatus 打印 JSON 退出（exit 0/1）。
- 配置与事件日志：SQLite `%ProgramData%\DualNIC Balance\dualnic.db`
  （config 表 key=main 存 JSON 容器 + events 表）；环境变量 `DUALNIC_CONFIG_PATH` 仅用于
  旧单文件路径的迁移/联调覆盖。
- IPC 协议契约见 `crates/core/src/ipc.rs`（回环 TCP + JSON 行；读不鉴权，写鉴权一次性令牌）。
- 免管理员联调：`dualnic-service --console` + `dualnic-gui --headless`；
  部署/分发见 `docs/DEPLOYMENT.md`；运维脚本在 `scripts/`（装/卸服务、自启动、
  `rebuild-svc.ps1` 提权换服务 exe、`reset-system.ps1` 一键复位开发机）。
