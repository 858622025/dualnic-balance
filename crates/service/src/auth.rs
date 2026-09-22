//! 配置写操作的一次性会话令牌（尽力而为的摩擦层）。
//!
//! 目的：防止本机其它普通进程随手通过回环 IPC 改写配置。**不是安全边界**——
//! 熵源自 pid/时间/ASLR 地址/单调时钟，对同机有进程级读写能力的攻击者不够；
//! 如需进一步加固，可迁移命名管道并对 GUI 会话做 DACL。
//!
//! 语义：单槽、覆盖式签发；`consume` 要求值匹配且未过期，成功后即清除（一次性）。

use std::sync::Mutex;
use std::time::{Duration, Instant};

const TOKEN_TTL: Duration = Duration::from_secs(60);

pub struct SessionAuth {
    inner: Mutex<Option<StampedToken>>,
}

struct StampedToken {
    value: String,
    issued_at: Instant,
}

impl Default for SessionAuth {
    fn default() -> Self {
        SessionAuth::new()
    }
}

impl SessionAuth {
    pub fn new() -> Self {
        SessionAuth { inner: Mutex::new(None) }
    }

    /// 签发（覆盖旧令牌），返回明文 token。同一时刻仅一个有效会话。
    pub fn issue(&self) -> String {
        let token = gen_token();
        *self.inner.lock().unwrap() = Some(StampedToken { value: token.clone(), issued_at: Instant::now() });
        token
    }

    /// 校验并消费：值匹配且未过期 → 清除并返回 true；否则 false（不改状态）。
    pub fn consume(&self, token: &str) -> bool {
        let mut slot = self.inner.lock().unwrap();
        match slot.as_ref() {
            Some(s) if s.value == token && s.issued_at.elapsed() < TOKEN_TTL => {
                *slot = None;
                true
            }
            _ => false,
        }
    }
}

/// 生成一个不确定的 64-hex 令牌（SplitMix64 展宽一个由杂熵拼出的种子）。
fn gen_token() -> String {
    // 杂熵种子：地址随机化 + 进程 + 单调时钟低位。
    let aslr = Box::new(0u8);
    let aslr_addr = &*aslr as *const u8 as usize as u64;
    let pid = std::process::id() as u64;
    let clock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let mono = Instant::now().elapsed().as_nanos() as u64;
    let mut seed = aslr_addr ^ (pid.rotate_left(32)) ^ (clock.rotate_left(16)) ^ mono;
    seed |= 1;

    let mut out = String::with_capacity(64);
    for _ in 0..8 {
        seed = splitmix64(seed);
        out.push_str(&format!("{:016x}", seed));
    }
    out
}

/// SplitMix64：确定性小状态伪随机，够生成不直观 token，不用于加密。
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
