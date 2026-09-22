# 部署与分发

> 本文档对应 **版本 1.0（2026-09-22）**。

## 1. 构建

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release
# 产物：target/release/dualnic-service.exe、dualnic-gui.exe
```

## 2. 安装 / 卸载服务（需管理员）

**推荐：直接在 GUI「设置」页点按钮**（安装/卸载服务、注册/取消登录自启动）。
脚本已嵌入 GUI，无需 scripts 目录；安装服务会弹一次 UAC 授权。
前提：`dualnic-service.exe` 与 `dualnic-gui.exe` 放在**同一目录**。

**首次运行的自动行为**：
- GUI 是**单实例**的（重复双击只弹「已在运行」提示）。
- 服务未安装时，GUI 启动会自动（弹 UAC）拉起一个**临时后台引擎**
  （`dualnic-service.exe --console`，仅本次开机有效，重启后不再自动运行）。
  点一次「取消」即记录跳过，之后不再自动弹；正式常驻请在设置页安装服务。
- 注册登录自启动：根目录登录触发任务需要管理员授权，点击后若系统要求会弹 UAC。

命令行方式（等价，IT 批量部署用）：

```powershell
# 安装为 LocalSystem 服务（开机自启）；-Exe 可指定服务 exe 路径
powershell -ExecutionPolicy Bypass -File scripts\install-service.ps1

# 卸载
powershell -ExecutionPolicy Bypass -File scripts\uninstall-service.ps1
```

- 服务名：`DualNICBalance`，`sc.exe query DualNICBalance` 查状态。
- 日志：`%ProgramData%\DualNIC Balance\logs\dualnic-service.log`（按天滚动）。
- 前台调试（不装服务）：`dualnic-service.exe --console`（需管理员才能写路由；先 `sc stop DualNICBalance` 释放 IPC 端口）。

## 3. 配置

- **配置与事件日志统一存 SQLite**：`%ProgramData%\DualNIC Balance\dualnic.db`。
- 打开 GUI（普通权限，免 UAC），在「配置向导」页：
  1. 选内网卡 / 外网卡（按 GUID/描述自动识别，不依赖会变的接口索引）；
  2. 填内网网段；
  3. 设受保护接口（把 vEthernet / VPN / TUN 等虚拟卡 GUID 加进去）。
- **对账无需手动开启**：方案启用（接管 WAN）后自动开启双收敛
  （默认路由收敛为 WAN 独占唯一 + 清除非受管多余默认路由）；未接管 WAN 的方案自动关闭。
  受保护接口始终只读豁免。
- **配置导入/导出**：配置向导页有「导出配置 JSON」「导入配置 JSON」按钮，方便备份/迁移/IT 排障。

### 3.1 GUI 登录自启动（计划任务）

推荐直接用 `setup.bat`（见第 2 节）一键完成服务 + 自启动；命令行等价方式：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\install-gui-autostart.ps1
```

注册计划任务 `DualNICBalanceGUI`：用户登录后 30 秒自启动 GUI（已取消计划任务默认的
72 小时强制结束限制）。注意：根目录登录触发任务需要管理员授权，若系统要求会弹 UAC。
卸载：`scripts\uninstall-gui-autostart.ps1`。

## 4. 与代理工具共存（重要）

分流在**路由层**（决定走哪块物理网卡），代理在**应用层**（决定要不要过代理），两者可共存：

1. **代理工具用「系统代理模式」，不要开 TUN/虚拟网卡模式** —— TUN 会接管所有流量、侵入路由层，破坏「单默认 + 内网前缀」的分流结构，并触发多默认网关故障。
2. **代理规则里把内网段设为直连（DIRECT）**，例如 Clash：
   ```yaml
   rules:
     - IP-CIDR,192.168.0.0/16,DIRECT,no-resolve
     - GEOIP,LAN,DIRECT
     - MATCH,你的代理节点
   ```
3. **修改 hosts 完全兼容**：hosts 只影响域名→IP 解析，分流基于 IP，互不干扰。

## 5. 数据面（暂不需要）

已真机验证：WinDivert 改写源地址不能可靠钦定出口（重注入行为黑盒 + 改源后断网），故未采用数据面方案。
纯前缀分流场景用不到数据面；仅当将来需要「按域名/按应用/else 分支」分流时，才考虑
「本地终结（NAT + 绑定源 IP socket）」或「TUN」两路。

## 6. 回滚 / 自愈

- 风险页「一键收敛」= 手动把路由收敛到期望态；切到未接管 WAN 的方案（如「默认」）即停用接管，
  对账自动关闭并清理本工具添加的前缀路由。
- 服务自带事件驱动自愈（`NotifyIpInterfaceChange` + 定时兜底），网络环境变化时自动对账。
- 「暂停分流」可随时冻结自动对账（托盘右键或状态页按钮）。
