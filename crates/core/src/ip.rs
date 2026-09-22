//! IPv4 地址与 CIDR 前缀的轻量表示。
//!
//! 刻意不引入 `ipnet` 等第三方 crate：本项目的运算很窄（前缀匹配、比较、序列化），
//! 自己实现十几行，避免依赖膨胀并保持 core 可单测。若后续确实需要更多 IP 能力，
//! 再评估引入 ipnet。

use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::CoreError;

/// 一个 IPv4 CIDR 前缀，如 `203.0.113.0/24`。
///
/// serde：序列化为 `"a.b.c.d/len"` 字符串（配置里人类可读），而非 `{addr,len}` 结构体。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Prefix {
    /// 网络地址（已按 prefix_len 规整）。
    pub addr: Ipv4Addr,
    /// 前缀长度 0..=32。
    pub len: u8,
}

impl Prefix {
    /// 从「网络地址 + 前缀长度」构造，并校验 / 规整。
    pub fn new(addr: Ipv4Addr, len: u8) -> Result<Self, CoreError> {
        if len > 32 {
            return Err(CoreError::InvalidAddress(format!("prefix len {len} > 32")));
        }
        let masked = mask_addr(addr, len);
        Ok(Prefix { addr: masked, len })
    }

    /// 构造一个 /32 主机前缀（用于解析单 IP 查询）。
    pub fn host(addr: Ipv4Addr) -> Self {
        Prefix { addr, len: 32 }
    }

    /// 判断给定的 IPv4 地址是否落在本前缀内。
    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        if self.len == 0 {
            return true; // 0.0.0.0/0
        }
        let shift = 32 - self.len as u32;
        let net = u32::from(self.addr) >> shift;
        let host = u32::from(ip) >> shift;
        net == host
    }

    /// 前缀的 u32 掩码，如 /24 -> 0xFFFFFF00。
    pub fn netmask(&self) -> u32 {
        if self.len == 0 {
            0
        } else {
            u32::MAX << (32 - self.len as u32)
        }
    }
}

/// 由连续子网掩码求前缀长度（0.0.0.0→0、255.255.255.0→24）。
/// 非连续掩码（如 255.0.255.0）→ `Err`，避免把畸形掩码误判成长度。
pub fn netmask_len(mask: Ipv4Addr) -> Result<u8, CoreError> {
    let m = u32::from(mask);
    if m == 0 {
        return Ok(0);
    }
    let ones = m.leading_ones() as u8;
    if ones == 32 || (m << ones) == 0 {
        Ok(ones)
    } else {
        Err(CoreError::Msg(crate::msgref!("DNERR", 10; &mask.to_string())))
    }
}

impl FromStr for Prefix {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (ip_part, len_part) = s
            .split_once('/')
            .ok_or_else(|| CoreError::Msg(crate::msgref!("DNERR", 11; s)))?;
        let addr: Ipv4Addr = ip_part
            .parse()
            .map_err(|_| CoreError::Msg(crate::msgref!("DNERR", 12; ip_part)))?;
        let len: u8 = len_part
            .parse()
            .map_err(|_| CoreError::Msg(crate::msgref!("DNERR", 13; len_part)))?;
        Prefix::new(addr, len)
    }
}

impl fmt::Display for Prefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.len)
    }
}

/// 把地址按前缀长度规整（低位置零）。
fn mask_addr(addr: Ipv4Addr, len: u8) -> Ipv4Addr {
    let raw = u32::from(addr);
    let masked = if len == 0 {
        0
    } else {
        raw & (u32::MAX << (32 - len as u32))
    };
    Ipv4Addr::from(masked)
}

impl Serialize for Prefix {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Prefix {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        raw.parse()
            .map_err(|e: CoreError| serde::de::Error::custom(e.to_string()))
    }
}
