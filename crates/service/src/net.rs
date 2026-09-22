//! 只读网络视图（iphlpapi / netioapi / 适配器枚举）。
//!
//! **只读**：不写路由、不改系统；以下函数普通权限均可调用。
//! - `risk_summary()`：GetStatus 用——IPv4 默认路由 + 接口名（风险页）。
//! - `adapters_views()` / `all_ipv4_routes()`：GetSnapshot 用——网卡元数据(含 GUID/网关/IP) + 全路由表。
//!
//! windows-0.58 feature：GetIpForwardTable 在 Win32_Networking_WinSock、GetIfTable2 在
//! Win32_NetworkManagement_Ndis、GetAdaptersAddresses 需两者，FreeMibTable 在 IpHelper（service Cargo 已开）。

use std::collections::HashMap;
use std::ffi::CStr;
use std::net::Ipv4Addr;

use windows::core::PCWSTR;
use windows::Win32::Foundation::BOOL;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetAdaptersAddresses, GetIfTable2, GetIpForwardTable, GAA_FLAG_INCLUDE_GATEWAYS,
    IP_ADAPTER_ADDRESSES_LH, IP_ADAPTER_GATEWAY_ADDRESS_LH, IP_ADAPTER_UNICAST_ADDRESS_LH,
    MIB_IF_TABLE2, MIB_IPFORWARDTABLE,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Networking::WinSock::SOCKADDR;
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ,
};

use dualnic_core::diff::{AdapterView, RouteView};
use dualnic_core::ip::{netmask_len};
use dualnic_core::ipc::{DefaultRouteRow, RiskSummary};

const NO_ERROR: u32 = 0;
/// ERROR_INSUFFICIENT_BUFFER（legacy 双趟）
const ERR_INSUF_BUF: u32 = 122;
/// ERROR_NO_DATA
const ERR_NO_DATA: u32 = 232;
/// ERROR_BUFFER_OVERFLOW（GetAdaptersAddresses 双趟）
const ERR_BUF_OVERFLOW: u32 = 111;
/// AF_INET
const AF_INET: u16 = 2;

// ──────────────────────────── 风险（GetStatus 用） ────────────────────────────

/// 构建风险概要：枚举默认路由 + 接口名。
pub fn risk_summary() -> RiskSummary {
    let names = match if_index_names() {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 30;)));
            HashMap::new()
        }
    };

    match all_ipv4_routes() {
        Ok(routes) => {
            let rows: Vec<DefaultRouteRow> = routes
                .iter()
                .filter(|r| r.is_default())
                .map(|r| {
                    let (interface_desc, interface_alias) =
                        names.get(&r.if_index).cloned().unwrap_or_default();
                    DefaultRouteRow {
                        if_index: r.if_index,
                        interface_desc,
                        interface_alias,
                        gateway: r.gateway.to_string(),
                        metric: r.metric,
                        source_proto: r.source_proto,
                    }
                })
                .collect();
            RiskSummary::ok(rows)
        }
        Err(e) => RiskSummary::read_failed(e),
    }
}

// ──────────────────────────── 网卡视图 + 全路由（GetSnapshot 用） ────────────────────────────

/// 网卡元数据：GUID/网关/单播 IP/Up 状态（GetAdaptersAddresses, family=AF_INET）。
/// desc/alias 用 GetIfTable2 补齐（同一 IPv4 IfIndex 域 join）。
pub fn adapters_views() -> Result<Vec<AdapterView>, dualnic_core::msg::MessageRef> {
    let names = if_index_names().unwrap_or_default();

    let mut size: u32 = 16 * 1024;
    let mut buf: Vec<u64> = vec![0; (size as usize + 7) / 8];
    unsafe {
        loop {
            let rc = GetAdaptersAddresses(
                AF_INET as u32,
                GAA_FLAG_INCLUDE_GATEWAYS,
                None,
                Some(buf.as_mut_ptr().cast()),
                &mut size,
            );
            match rc {
                NO_ERROR => break,
                ERR_BUF_OVERFLOW | ERR_INSUF_BUF => {
                    buf.resize((size as usize + 7) / 8, 0);
                    continue;
                }
                ERR_NO_DATA => return Ok(Vec::new()),
                other => {
                    return Err(dualnic_core::msgref!("DNERR", 70; &other.to_string()))
                }
            }
        }

        let mut out: Vec<AdapterView> = Vec::new();
        let mut cur = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !cur.is_null() {
            let e = &*cur;
            let if_index = e.Anonymous1.Anonymous.IfIndex;
            let (desc, alias) = names.get(&if_index).cloned().unwrap_or_default();
            let guid = pstr_to_string(e.AdapterName);
            let (primary_ipv4, prefix_len) = match first_unicast_ipv4(e.FirstUnicastAddress) {
                Some((ip, len)) => (Some(ip), Some(len)),
                None => (None, None),
            };
            let dhcp_server = adapter_dhcp_server(guid.as_deref());
            if let (Some(_g), None) = (&guid, dhcp_server) {
                tracing::debug!(if_index, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 31;)));
            }
            out.push(AdapterView {
                if_index,
                guid,
                description: if desc.is_empty() { None } else { Some(desc) },
                alias: if alias.is_empty() { None } else { Some(alias) },
                primary_ipv4,
                gateway_ipv4: first_gateway_ipv4(e.FirstGatewayAddress),
                dhcp_server,
                prefix_len,
                if_type: Some(e.IfType),
                oper_up: e.OperStatus.0 == IfOperStatusUp.0,
            });
            cur = e.Next;
        }
        Ok(out)
    }
}

/// IPv4 路由全表。
pub fn all_ipv4_routes() -> Result<Vec<RouteView>, dualnic_core::msg::MessageRef> {
    unsafe {
        let mut size: u32 = 16 * 1024;
        let mut buf: Vec<u64> = vec![0; (size as usize + 7) / 8];
        loop {
            let rc = GetIpForwardTable(
                Some(buf.as_mut_ptr().cast()),
                &mut size,
                BOOL(0), // bOrder=0，顺序无所谓
            );
            match rc {
                NO_ERROR => break,
                ERR_INSUF_BUF => {
                    buf.resize((size as usize + 7) / 8, 0);
                    continue;
                }
                ERR_NO_DATA => return Ok(Vec::new()),
                other => {
                    return Err(dualnic_core::msgref!("DNERR", 71; &other.to_string()))
                }
            }
        }

        let table = buf.as_ptr() as *const MIB_IPFORWARDTABLE;
        let num = (*table).dwNumEntries as usize;
        let mut out: Vec<RouteView> = Vec::with_capacity(num);
        if num == 0 {
            return Ok(out);
        }
        let rows = std::slice::from_raw_parts((*table).table.as_ptr(), num);
        for r in rows {
            // mask 网络序 → Ipv4Addr → 连续掩码长度
            let mask_addr = Ipv4Addr::from(u32::from_be(r.dwForwardMask));
            let Ok(mask_len) = netmask_len(mask_addr) else {
                tracing::warn!(mask = %mask_addr, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 32;)));
                continue;
            };
            out.push(RouteView {
                if_index: r.dwForwardIfIndex,
                dest: Ipv4Addr::from(u32::from_be(r.dwForwardDest)),
                mask_len,
                gateway: Ipv4Addr::from(u32::from_be(r.dwForwardNextHop)),
                metric: r.dwForwardMetric1,
                source_proto: r.Anonymous2.dwForwardProto,
                forward_type: r.Anonymous1.dwForwardType,
            });
        }
        out.sort_by(|a, b| (a.metric, a.if_index).cmp(&(b.metric, b.if_index)));
        Ok(out)
    }
}

// ──────────────────────────── 接口名（GetIfTable2） ────────────────────────────

/// if_index → (Description, Alias)。缓冲由系统分配，FreeMibTable 释放。
fn if_index_names() -> Result<HashMap<u32, (String, String)>, String> {
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        let rc = GetIfTable2(&mut table);
        if rc.0 != NO_ERROR {
            return Err(dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 33; &rc.0.to_string())));
        }
        if table.is_null() {
            return Err(dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 34;)));
        }
        let num = (*table).NumEntries as usize;
        let mut map = HashMap::with_capacity(num);
        if num > 0 {
            let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), num);
            for r in rows {
                map.insert(r.InterfaceIndex, (wchar_trim(&r.Description), wchar_trim(&r.Alias)));
            }
        }
        FreeMibTable(table.cast());
        Ok(map)
    }
}

// ──────────────────────────── 底层解析 ────────────────────────────

/// PSTR（ANSI）→ String。
unsafe fn pstr_to_string(p: windows::core::PSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    Some(String::from_utf8_lossy(CStr::from_ptr(p.as_ptr().cast()).to_bytes()).into_owned())
}

/// 读网卡 DHCP 服务器：`HKLM\SYSTEM\CurrentControlSet\Services\Tcpip\Parameters\Interfaces\{guid}\DhcpServer`。
/// REG_SZ，多 DHCP 时逗号分隔——取首个可解析的 IPv4。任一步失败 → None（不致命，供路由判定降级）。
/// AdapterName 是 `{...}` 形式；部分环境下注册表子键可能存成裸 GUID，做一次去花括号回退。
fn adapter_dhcp_server(guid: Option<&str>) -> Option<Ipv4Addr> {
    let guid = guid?;
    let bare = guid.trim_matches(&['{', '}'] as &[char]);
    for key in [guid.to_string(), bare.to_string()] {
        let path = to_wide(&format!(
            r"SYSTEM\CurrentControlSet\Services\Tcpip\Parameters\Interfaces\{key}"
        ));
        let mut hkey: HKEY = HKEY::default();
        let rc = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(path.as_ptr()), 0, KEY_READ, &mut hkey) };
        if rc != ERROR_SUCCESS {
            continue;
        }
        let value = unsafe { read_reg_sz(hkey, "DhcpServer") };
        let _ = unsafe { RegCloseKey(hkey) };
        if let Some(s) = value {
            if let Some(ip) = s.split(',').map(str::trim).find_map(|p| p.parse().ok()) {
                return Some(ip);
            }
        }
    }
    None
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 读一个 REG_SZ 值（宽字符）。两次调用 RegQueryValueExW：先取长度、再取内容。
unsafe fn read_reg_sz(key: HKEY, name: &str) -> Option<String> {
    use windows::Win32::System::Registry::REG_VALUE_TYPE;
    let namew = to_wide(name);
    let mut kind: REG_VALUE_TYPE = REG_SZ;
    let mut len: u32 = 0;
    let mut rc = RegQueryValueExW(
        key,
        PCWSTR(namew.as_ptr()),
        None,
        Some(&mut kind),
        None,
        Some(&mut len),
    );
    if rc != ERROR_SUCCESS || len == 0 {
        return None;
    }
    // len 含结尾 NUL（REG_SZ 是 UTF-16，每字符 2 字节，NUL 占 2 字节）。
    let mut buf = vec![0u8; len as usize];
    let mut bytes_len = buf.len() as u32;
    rc = RegQueryValueExW(
        key,
        PCWSTR(namew.as_ptr()),
        None,
        Some(&mut kind),
        Some(buf.as_mut_ptr()),
        Some(&mut bytes_len),
    );
    if rc != ERROR_SUCCESS {
        return None;
    }
    // bytes_len 为实际写入字节数（含结尾 NUL）。截到实际长度，再按 UTF-16LE 解码并去掉尾随 NUL。
    buf.truncate(bytes_len as usize);
    let chars: Vec<u16> = buf.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let mut s = String::from_utf16_lossy(&chars);
    if s.ends_with('\0') {
        s.pop();
    }
    Some(s)
}

/// 取单播地址链里第一个 IPv4（含其前缀长度 OnLinkPrefixLength）。
fn first_unicast_ipv4(mut node: *mut IP_ADAPTER_UNICAST_ADDRESS_LH) -> Option<(Ipv4Addr, u8)> {
    unsafe {
        while !node.is_null() {
            let a = &*node;
            if let Some(ip) = sockaddr_ipv4(a.Address.lpSockaddr) {
                return Some((ip, a.OnLinkPrefixLength));
            }
            node = a.Next;
        }
    }
    None
}

/// 取网关地址链里第一个 IPv4。
fn first_gateway_ipv4(mut node: *mut IP_ADAPTER_GATEWAY_ADDRESS_LH) -> Option<Ipv4Addr> {
    unsafe {
        while !node.is_null() {
            let a = &*node;
            if let Some(ip) = sockaddr_ipv4(a.Address.lpSockaddr) {
                return Some(ip);
            }
            node = a.Next;
        }
    }
    None
}

/// 从 SOCKADDR 解析 IPv4：family(0..2)==AF_INET，地址在 offset 4..8（网络序）。
unsafe fn sockaddr_ipv4(sa: *mut SOCKADDR) -> Option<Ipv4Addr> {
    if sa.is_null() {
        return None;
    }
    let p = sa.cast::<u8>();
    let family = u16::from_le_bytes([*p, *p.add(1)]);
    if family != AF_INET {
        return None;
    }
    let b = [*p.add(4), *p.add(5), *p.add(6), *p.add(7)];
    Some(Ipv4Addr::new(b[0], b[1], b[2], b[3]))
}

/// 定长 WCHAR 数组 → String（截到首个 NUL）。
fn wchar_trim(a: &[u16]) -> String {
    let end = a.iter().position(|&c| c == 0).unwrap_or(a.len());
    String::from_utf16_lossy(&a[..end])
}

