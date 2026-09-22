//! 连通性探测：用候选网卡的源 IP 去连一个外网地址，判定“哪块能出外网”。
//!
//! 纯 GUI 侧、只读辅助：不写配置/路由、不进服务/协议、不需要管理员。
//! 仅由「配置向导」页签进入或页内按钮触发（见 wizard.rs），不常驻轮询。
//!
//! 实现说明：Windows 上 ICMP raw ping 需要管理员，且调 ping.exe 依赖进程、易被企业
//! 杀软拦截；因此用 socket2 先把 socket bind 到候选卡的源 IP，再 TCP 连 `www.apple.com:443`
//! —— 与“ping 判定能否出外网”目的等价且更稳。

use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, Type};

use dualnic_core::diff::AdapterView;

/// 默认探测目标（未配置/为空时使用）。
pub const DEFAULT_TARGET: &str = "www.apple.com";
const TARGET_PORT: u16 = 443;
const TIMEOUT: Duration = Duration::from_secs(2);

/// 返回能连得上 `host:443` 的候选网卡 if_index 列表（host 为空则用默认 apple.com）。
/// 入参建议已过滤为“已连接物理卡”；DNS 解析失败/无候选时返回空（调用方自行回退网关启发式）。
pub fn reachable_wans(adapters: &[AdapterView], host: &str) -> Vec<u32> {
    let host = if host.trim().is_empty() { DEFAULT_TARGET } else { host.trim() };
    let Some(target) = resolve_target_v4(host) else {
        return Vec::new();
    };
    let mut ok = Vec::new();
    for a in adapters {
        let Some(src) = a.primary_ipv4 else { continue };
        // 安全兜底：跳过 APIPA / 未连接
        if is_link_local(src) {
            continue;
        }
        if try_connect(src, target) {
            ok.push(a.if_index);
        }
    }
    ok
}

fn resolve_target_v4(host: &str) -> Option<SocketAddr> {
    (host, TARGET_PORT).to_socket_addrs().ok()?.find(|s| s.is_ipv4())
}

fn try_connect(src: Ipv4Addr, target: SocketAddr) -> bool {
    let Ok(sock) = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)) else {
        return false;
    };
    let bind_addr = SocketAddr::new(src.into(), 0);
    let ok = sock
        .bind(&bind_addr.into())
        .and_then(|_| sock.set_nonblocking(true))
        .and_then(|_| sock.connect_timeout(&target.into(), TIMEOUT))
        .is_ok();
    drop(sock);
    ok
}

fn is_link_local(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 169 && o[1] == 254
}
