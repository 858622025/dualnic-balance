//! 回环 TCP IPC 服务器（JSON 行协议）。
//!
//! 线程模型：非阻塞 accept + ~100ms 轮询关闭令牌；每连接一个线程，**一请求一响应后关闭**。
//! 协议契约见 `dualnic_core::ipc`；写配置类请求走 `SessionAuth` 令牌（见 auth.rs）。

use std::io::{BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use dualnic_core::config::PolicyConfig;
use dualnic_core::ipc::error_code::{BAD_CONFIG, INTERNAL, IO_FAILED, PROFILE_NOT_FOUND, UNAUTHORIZED};
use dualnic_core::ipc::{self, IpcError, Request, Response, RpcError};

use crate::auth::SessionAuth;
use crate::status::{build_status, config_snapshot, unix_now_secs, RuntimeState};

/// 关闭令牌：`--console` 的 Ctrl-C 与 SCM 的 Stop **共用**，保证两模式停止行为一致。
#[derive(Clone, Default)]
pub struct Shutdown {
    flag: Arc<AtomicBool>,
    cond: Arc<(Mutex<bool>, Condvar)>,
}

impl Shutdown {
    pub fn new() -> Self {
        Shutdown {
            flag: Arc::new(AtomicBool::new(false)),
            cond: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }

    /// 请求关闭（幂等）：置位 + 唤醒所有等待者。
    pub fn request(&self) {
        self.flag.store(true, Ordering::Release);
        let (lock, cv) = &*self.cond;
        let mut v = lock.lock().unwrap();
        *v = true;
        cv.notify_all();
    }

    pub fn is_requested(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// 阻塞直到 `request()` 被调用。
    pub fn wait_stopped(&self) {
        let (lock, cv) = &*self.cond;
        let mut v = lock.lock().unwrap();
        while !*v {
            v = cv.wait(v).unwrap();
        }
    }
}

pub struct IpcServer {
    listener: TcpListener,
}

impl IpcServer {
    /// 绑定并置非阻塞。返回（server, 实际监听地址）。
    pub fn bind(addr: &str) -> std::io::Result<(IpcServer, SocketAddr)> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        let local = listener.local_addr()?;
        Ok((IpcServer { listener }, local))
    }

    /// accept 轮询循环：100ms 粒度检查关闭令牌；每连接开线程处理（一请求一响应）。
    pub fn serve_loop(&self, state: &Arc<RuntimeState>, auth: &Arc<SessionAuth>, sd: &Shutdown) {
        loop {
            if sd.is_requested() {
                break;
            }
            match self.listener.accept() {
                Ok((stream, _peer)) => {
                    let st = Arc::clone(state);
                    let au = Arc::clone(auth);
                    thread::spawn(move || handle_connection(stream, &st, &au));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 76;)));
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}

/// 单连接处理：读一行 → 分派 → 回一行 → 关闭。
fn handle_connection(mut stream: TcpStream, state: &Arc<RuntimeState>, auth: &Arc<SessionAuth>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    // 用 `BufReader<&TcpStream>` 读完一行即释放读借用，随后写同一 socket。
    let line = {
        let mut reader = BufReader::new(&stream);
        ipc::read_line_bounded(&mut reader, ipc::MAX_LINE_BYTES)
    };

    let resp = dispatch(line, state, auth);
    let bytes = match ipc::encode_line(&resp) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(error = %e, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 77;)));
            return;
        }
    };
    if let Err(e) = stream.write_all(&bytes).and_then(|_| stream.flush()) {
        tracing::warn!(error = %e, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 78;)));
    }
    // drop(stream)：一次连接即关
}

/// 请求 → 响应。坏行 / 坏 JSON / 未知 type 统一走 `bad_request` 信封，不 panic。
fn dispatch(
    line: Result<Vec<u8>, IpcError>,
    state: &RuntimeState,
    auth: &SessionAuth,
) -> Response {
    let req = match line.and_then(|bytes| ipc::parse_request(&bytes)) {
        Ok(r) => r,
        Err(e) => return Response::err((&e).into()),
    };

    let data = match req {
        Request::Ping => serde_json::to_value(ipc::PingData {
            pong: true,
            server_unix_secs: unix_now_secs(),
        }),
        Request::GetStatus => serde_json::to_value(build_status(state)),
        Request::GetSnapshot => match crate::status::build_snapshot(state) {
            Some(s) => serde_json::to_value(s),
            None => serde_json::to_value(ipc::SnapshotData {
                fetched_at_unix_secs: unix_now_secs(),
                read_error: Some(dualnic_core::msgref!("DNERR", 99;)),
                report: Default::default(),
            }),
        },
        Request::Handshake => serde_json::to_value(ipc::HandshakeData {
            token: auth.issue(),
            ttl_secs: 60,
        }),
        Request::GetConfig => serde_json::to_value(config_snapshot(state).config),
        Request::SetConfig { token, config } => {
            return handle_set_config(auth, state, &token, config);
        }
        Request::ListProfiles => serde_json::to_value(crate::status::list_profiles(state)),
        Request::CreateProfile { token, name, from } => {
            return handle_profile_write(state, auth, &token, |st| {
                crate::status::create_profile(st, &name, from.as_deref())
            });
        }
        Request::DeleteProfile { token, name } => {
            return handle_profile_write(state, auth, &token, |st| {
                crate::status::delete_profile(st, &name)
            });
        }
        Request::SwitchProfile { token, name } => {
            return handle_profile_write(state, auth, &token, |st| {
                crate::status::switch_profile(st, &name)
            });
        }
        Request::RenameProfile { token, old_name, new_name } => {
            let name2 = new_name.clone();
            return handle_profile_write(state, auth, &token, |st| {
                crate::status::rename_profile(st, &old_name, &name2)
            });
        }
        Request::DetectEnvironment => serde_json::to_value(crate::status::detect_environment(state)),
        Request::AutoMatch { token } => {
            return handle_auto_match(auth, state, &token);
        }
        Request::ReconcileNow { token } => {
            return handle_reconcile(auth, state, &token, |st| crate::reconcile::reconcile_once(st));
        }
        Request::Diagnose => serde_json::to_value(crate::diagnose::diagnose(state)),
        Request::SetPaused { token, paused } => {
            return handle_paused(auth, state, &token, paused);
        }
        Request::GetEvents { since_unix_secs, limit } => {
            serde_json::to_value(state.db.query_events(since_unix_secs, limit))
        }
        Request::ClearEvents { token } => {
            return handle_clear_events(auth, state, &token);
        }
        Request::ExportConfig => serde_json::to_value(ipc::ExportConfigData {
            config_json: state.db.get_config_raw().ok().flatten(),
        }),
        Request::ImportConfig { token, config_json } => {
            return handle_import_config(auth, state, &token, &config_json);
        }
        Request::GetPhysicalNics => serde_json::to_value(ipc::PhysicalNicsData {
            guids: crate::status::get_physical_nics(state),
        }),
        Request::SetPhysicalNics { token, guids } => {
            return handle_physical_nics(auth, state, &token, guids);
        }
        Request::GetProfileConfig { name } => match crate::status::get_profile_config(state, &name) {
            Ok(cfg) => serde_json::to_value(cfg),
            Err(msg) => return Response::err(RpcError::new(PROFILE_NOT_FOUND, msg)),
        },
        Request::SetProfileConfig { token, name, config } => {
            return handle_set_profile_config(auth, state, &token, &name, config);
        }
    };

    match data {
        Ok(v) => Response::ok(v),
        Err(e) => Response::err(RpcError::new(INTERNAL, dualnic_core::msgref!("DNERR", 110; &e.to_string()))),
    }
}

/// 清空事件日志：令牌校验 → DELETE + VACUUM → 返回清空后的库大小。
fn handle_clear_events(auth: &SessionAuth, state: &RuntimeState, token: &str) -> Response {
    if !auth.consume(token) {
        return Response::err(RpcError::new(UNAUTHORIZED, dualnic_core::msgref!("DNERR", 111;)));
    }
    if let Err(msg) = state.db.clear_events() {
        return Response::err(RpcError::new(INTERNAL, msg));
    }
    state.db.append_event_msg("info", "config", &dualnic_core::msgref!("DNREC", 1;));
    match serde_json::to_value(ipc::ClearEventsData {
        db_size_bytes: state.db.db_size_bytes(),
    }) {
        Ok(v) => Response::ok(v),
        Err(e) => Response::err(RpcError::new(INTERNAL, dualnic_core::msgref!("DNERR", 110; &e.to_string()))),
    }
}

/// 暂停/恢复对账：令牌校验 → 置位 paused → 记事件。
fn handle_paused(auth: &SessionAuth, state: &RuntimeState, token: &str, paused: bool) -> Response {
    if !auth.consume(token) {
        return Response::err(RpcError::new(UNAUTHORIZED, dualnic_core::msgref!("DNERR", 111;)));
    }
    state.paused.store(paused, Ordering::Release);
    let msg = if paused {
        dualnic_core::msgref!("DNREC", 13;)
    } else {
        dualnic_core::msgref!("DNREC", 14;)
    };
    state.db.append_event_msg("info", "config", &msg);
    Response::ok(serde_json::json!({ "paused": paused }))
}

/// 导入配置：令牌校验 → 解析 JSON → 写 SQLite + 热重载。
fn handle_import_config(
    auth: &SessionAuth,
    state: &RuntimeState,
    token: &str,
    config_json: &str,
) -> Response {
    if !auth.consume(token) {
        return Response::err(RpcError::new(UNAUTHORIZED, dualnic_core::msgref!("DNERR", 111;)));
    }
    match crate::status::import_config(state, config_json) {
        Ok(summary) => {
            let v = serde_json::json!({ "saved": true, "summary": summary });
            match serde_json::to_value(v) {
                Ok(val) => Response::ok(val),
                Err(e) => Response::err(RpcError::new(INTERNAL, dualnic_core::msgref!("DNERR", 110; &e.to_string()))),
            }
        }
        Err(msg) => Response::err(RpcError::new(BAD_CONFIG, msg)),
    }
}

/// SetPhysicalNics：令牌 → 写全局物理网卡标记集合。
fn handle_physical_nics(
    auth: &SessionAuth,
    state: &RuntimeState,
    token: &str,
    guids: Vec<String>,
) -> Response {
    if !auth.consume(token) {
        return Response::err(RpcError::new(UNAUTHORIZED, dualnic_core::msgref!("DNERR", 111;)));
    }
    match crate::status::set_physical_nics(state, &guids) {
        Ok(()) => {
            let v = serde_json::json!({ "saved": true });
            match serde_json::to_value(v) {
                Ok(val) => Response::ok(val),
                Err(e) => Response::err(RpcError::new(INTERNAL, dualnic_core::msgref!("DNERR", 110; &e.to_string()))),
            }
        }
        Err(msg) => Response::err(RpcError::new(IO_FAILED, msg)),
    }
}

/// SetProfileConfig：令牌 → 保存指定方案的配置（只持久化，不触发对账）。
fn handle_set_profile_config(
    auth: &SessionAuth,
    state: &RuntimeState,
    token: &str,
    name: &str,
    config: serde_json::Value,
) -> Response {
    if !auth.consume(token) {
        return Response::err(RpcError::new(UNAUTHORIZED, dualnic_core::msgref!("DNERR", 111;)));
    }
    let cfg = match PolicyConfig::from_json_value(config) {
        Ok(c) => c,
        Err(e) => {
            return Response::err(RpcError::new(BAD_CONFIG, e.message()));
        }
    };
    match crate::status::set_profile_config(state, name, &cfg) {
        Ok(summary) => {
            let v = serde_json::json!({ "saved": true, "summary": summary });
            match serde_json::to_value(v) {
                Ok(val) => Response::ok(val),
                Err(e) => Response::err(RpcError::new(INTERNAL, dualnic_core::msgref!("DNERR", 110; &e.to_string()))),
            }
        }
        Err(msg) => Response::err(RpcError::new(PROFILE_NOT_FOUND, msg)),
    }
}

/// SetConfig：令牌 → 校验 → 原子写 + 热重载。失败不触盘。
fn handle_set_config(auth: &SessionAuth, state: &RuntimeState, token: &str, config: serde_json::Value) -> Response {
    if !auth.consume(token) {
        return Response::err(RpcError::new(UNAUTHORIZED, dualnic_core::msgref!("DNERR", 111;)));
    }
    let cfg = match PolicyConfig::from_json_value(config) {
        Ok(c) => c,
        Err(e) => {
            return Response::err(RpcError::new(BAD_CONFIG, e.message()));
        }
    };
    match crate::status::apply_new_config(state, &cfg) {
        Ok(summary) => {
            let v = serde_json::json!({ "saved": true, "summary": summary });
            match serde_json::to_value(v) {
                Ok(val) => Response::ok(val),
                Err(e) => Response::err(RpcError::new(INTERNAL, dualnic_core::msgref!("DNERR", 110; &e.to_string()))),
            }
        }
        Err(msg) => Response::err(RpcError::new(IO_FAILED, msg)),
    }
}

/// 对账/回滚写操作：令牌校验 → reconcile → ok(ReconcileResult) / 权限错误。
fn handle_reconcile(
    auth: &SessionAuth,
    state: &RuntimeState,
    token: &str,
    op: impl FnOnce(&RuntimeState) -> dualnic_core::ipc::ReconcileResult,
) -> Response {
    if !auth.consume(token) {
        return Response::err(RpcError::new(UNAUTHORIZED, dualnic_core::msgref!("DNERR", 111;)));
    }
    let result = op(state);
    match serde_json::to_value(result) {
        Ok(v) => Response::ok(v),
        Err(e) => Response::err(RpcError::new(INTERNAL, dualnic_core::msgref!("DNERR", 110; &e.to_string()))),
    }
}

/// 自动匹配写操作：令牌校验 → auto_match → ok(AutoMatchData) / 错误。
fn handle_auto_match(auth: &SessionAuth, state: &RuntimeState, token: &str) -> Response {
    if !auth.consume(token) {
        return Response::err(RpcError::new(UNAUTHORIZED, dualnic_core::msgref!("DNERR", 111;)));
    }
    match crate::status::auto_match(state) {
        Ok(data) => match serde_json::to_value(data) {
            Ok(v) => Response::ok(v),
            Err(e) => Response::err(RpcError::new(INTERNAL, dualnic_core::msgref!("DNERR", 110; &e.to_string()))),
        },
        Err(msg) => Response::err(RpcError::new(IO_FAILED, msg)),
    }
}

/// 方案写操作公共：令牌校验 + status 操作 → ok{ saved:true, summary } / 错误映射。
fn handle_profile_write(
    state: &RuntimeState,
    auth: &SessionAuth,
    token: &str,
    op: impl FnOnce(&RuntimeState) -> Result<ipc::ConfigSummary, dualnic_core::msg::MessageRef>,
) -> Response {
    if !auth.consume(token) {
        return Response::err(RpcError::new(UNAUTHORIZED, dualnic_core::msgref!("DNERR", 111;)));
    }
    match op(state) {
        Ok(summary) => {
            let v = serde_json::json!({ "saved": true, "summary": summary });
            match serde_json::to_value(v) {
                Ok(val) => Response::ok(val),
                Err(e) => Response::err(RpcError::new(INTERNAL, dualnic_core::msgref!("DNERR", 110; &e.to_string()))),
            }
        }
        Err(msg) => {
            // 错误码按结构化消息号判定（DNERR-87/92=不存在，88=已存在，其余=IO）
            let is_dnerr = msg.msgid == "DNERR";
            let code = if is_dnerr && (msg.msgno == 87 || msg.msgno == 92) {
                PROFILE_NOT_FOUND
            } else if is_dnerr && msg.msgno == 88 {
                dualnic_core::ipc::error_code::ALREADY_EXISTS
            } else {
                IO_FAILED
            };
            Response::err(RpcError::new(code, msg))
        }
    }
}
