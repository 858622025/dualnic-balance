//! SQLite 持久化：配置（JSON 文本）+ 事件日志。
//!
//! bundled SQLite、单文件 `%ProgramData%\DualNIC Balance\dualnic.db`。
//! 配置仍以 JSON 文本序列化存储（保留 `PolicyConfig`/`ProfilesDoc` 的 serde 格式），
//! 只是介质从 config.json 换成 SQLite；事件日志结构化落表，支持按时间倒序查询。

use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::{params, Connection};

use dualnic_core::ipc::{EventLogEntry, EventsData};

use crate::paths;

/// 事件日志查询上限（防一次性拉取过大）。
const MAX_EVENTS_LIMIT: usize = 500;

/// 数据库句柄：内部 Mutex 串行化（服务单进程写，够用）。
pub struct Db {
    conn: Mutex<Connection>,
    /// false = 内存降级库（文件打开失败），无文件大小可报。
    file_backed: bool,
}

impl Db {
    /// 打开/创建文件数据库 + 建表 + 迁移旧 config.json。
    pub fn open() -> Result<Self, dualnic_core::msg::MessageRef> {
        let path = paths::db_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| dualnic_core::msgref!("DNERR", 74; &e.to_string()))?;
        }
        let conn = Connection::open(&path).map_err(|e| dualnic_core::msgref!("DNERR", 75; &e.to_string()))?;
        Self::init_schema(&conn)?;
        // 历史遗留：旧版每次写 main 前会复制一份到 backup 键（单级回滚），回滚删除后无消费方。
        // 幂等清理，让老库也不再留着这份死数据。
        let _ = conn.execute("DELETE FROM config WHERE key = 'backup'", []);
        let db = Db { conn: Mutex::new(conn), file_backed: true };
        db.migrate_legacy_config()?;
        Ok(db)
    }

    /// 内存数据库（文件打开失败时的降级：不持久化，服务照常跑）。
    pub fn open_in_memory() -> Self {
        let conn = Connection::open_in_memory().expect("in-memory SQLite init failed");
        let _ = Self::init_schema(&conn);
        Db { conn: Mutex::new(conn), file_backed: false }
    }

    fn init_schema(conn: &Connection) -> Result<(), dualnic_core::msg::MessageRef> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS config (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS events (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                ts_unix_secs INTEGER NOT NULL,
                level        TEXT NOT NULL,
                source       TEXT NOT NULL,
                message      TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_events_ts ON events(ts_unix_secs);",
        )
        .map_err(|e| dualnic_core::msgref!("DNERR", 76; &e.to_string()))?;
        // v2 事件结构化列（msgid/msgno/msgv）：老库按列探测补列（SQLite 无 ADD COLUMN IF NOT EXISTS）
        for (col, ddl) in [
            ("msgid", "ALTER TABLE events ADD COLUMN msgid TEXT"),
            ("msgno", "ALTER TABLE events ADD COLUMN msgno INTEGER"),
            ("msgv", "ALTER TABLE events ADD COLUMN msgv TEXT"),
        ] {
            let exists: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('events') WHERE name = ?1",
                    [col],
                    |r| r.get::<_, i64>(0),
                )
                .map(|n| n > 0)
                .unwrap_or(true); // 探测失败宁可不补列，也不要误判成缺列炸掉启动
            if !exists {
                conn.execute_batch(ddl)
                    .map_err(|e| dualnic_core::msgref!("DNERR", 77; col, &e.to_string()))?;
            }
        }
        Ok(())
    }

    /// 首次启动：若库内无配置且旧 config.json 存在，则迁入并重命名旧文件为 .migrated。
    fn migrate_legacy_config(&self) -> Result<(), dualnic_core::msg::MessageRef> {
        if self.get_config_raw()?.is_some() {
            return Ok(());
        }
        let path = paths::config_path();
        if path.exists() {
            let s = std::fs::read_to_string(&path).map_err(|e| dualnic_core::msgref!("DNERR", 78; &e.to_string()))?;
            self.set_config_raw("main", &s)?;
            let mut bak = path.clone().into_os_string();
            bak.push(".migrated");
            let _ = std::fs::rename(&path, PathBuf::from(bak));
            tracing::info!("{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 18;)));
        }
        Ok(())
    }

    // ──────────────────────────── 配置 ────────────────────────────

    /// 读主配置的 JSON 文本（None = 尚未写入）。
    pub fn get_config_raw(&self) -> Result<Option<String>, dualnic_core::msg::MessageRef> {
        self.get_raw("main")
    }

    /// 读任意 key 的配置文本。
    pub fn get_raw(&self, key: &str) -> Result<Option<String>, dualnic_core::msg::MessageRef> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT value FROM config WHERE key = ?1")
            .map_err(|e| dualnic_core::msgref!("DNERR", 79; &e.to_string()))?;
        let mut rows = stmt.query([key]).map_err(|e| dualnic_core::msgref!("DNERR", 79; &e.to_string()))?;
        let Some(row) = rows.next().map_err(|e| dualnic_core::msgref!("DNERR", 79; &e.to_string()))? else {
            return Ok(None);
        };
        Ok(Some(row.get::<_, String>(0).map_err(|e| dualnic_core::msgref!("DNERR", 79; &e.to_string()))?))
    }

    /// 写配置的 JSON 文本（UPSERT，原子）。
    pub fn set_config_raw(&self, key: &str, json: &str) -> Result<(), dualnic_core::msg::MessageRef> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO config(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, json],
        )
        .map_err(|e| dualnic_core::msgref!("DNERR", 80; &e.to_string()))?;
        Ok(())
    }

    // ──────────────────────────── 事件日志 ────────────────────────────

    /// 追加一条结构化事件：msgid/msgno/msgv 入库（GUI 按当前语言渲染），
    /// message 列存语言无关的代码形态原文（如 `DNREC-001(3, 0, 1, 2)`）供老工具/兜底。
    pub fn append_event_msg(&self, level: &str, source: &str, msg: &dualnic_core::msg::MessageRef) {
        let msgv = serde_json::to_string(&msg.args).unwrap_or_else(|_| "[]".into());
        let conn = self.conn.lock().unwrap();
        let r = conn.execute(
            "INSERT INTO events(ts_unix_secs, level, source, message, msgid, msgno, msgv)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                crate::status::unix_now_secs(),
                level,
                source,
                msg.code_form(),
                msg.msgid,
                msg.msgno as i64,
                msgv
            ],
        );
        if let Err(e) = r {
            tracing::warn!(error = %e, "{}", dualnic_core::msg::t(&dualnic_core::msgref!("DNLOG", 19;)));
        }
    }

    /// 查最近事件（按时间倒序，`since` 之后的事件），附带数据库文件大小。
    pub fn query_events(&self, since: Option<u64>, limit: usize) -> EventsData {
        let limit = limit.min(MAX_EVENTS_LIMIT);
        let conn = self.conn.lock().unwrap();
        let sql = "SELECT ts_unix_secs, level, source, message, msgid, msgno, msgv FROM events
                   WHERE (?1 IS NULL OR ts_unix_secs > ?1)
                   ORDER BY ts_unix_secs DESC, id DESC LIMIT ?2";
        let mut stmt = match conn.prepare(sql) {
            Ok(s) => s,
            Err(_) => return EventsData { entries: Vec::new(), db_size_bytes: self.db_size_bytes() },
        };
        let mapped = stmt.query_map(params![since, limit as i64], |row| {
            let msgid: Option<String> = row.get(4)?;
            let msg = msgid.and_then(|msgid| {
                let msgno: i64 = row.get(5).ok()?;
                if msgno < 0 {
                    return None;
                }
                let msgv_raw: Option<String> = row.get(6).ok()?;
                let args: Vec<String> = msgv_raw
                    .as_deref()
                    .and_then(|v| serde_json::from_str(v).ok())
                    .unwrap_or_default();
                Some(dualnic_core::msg::MessageRef { msgid, msgno: msgno as u32, args })
            });
            Ok(EventLogEntry {
                ts_unix_secs: row.get(0)?,
                level: row.get(1)?,
                source: row.get(2)?,
                message: row.get(3)?,
                msg,
            })
        });
        let entries = match mapped {
            Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        };
        EventsData { entries, db_size_bytes: self.db_size_bytes() }
    }

    /// 清空全部事件日志并 VACUUM 回收文件空间。
    pub fn clear_events(&self) -> Result<(), dualnic_core::msg::MessageRef> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("DELETE FROM events; VACUUM;")
            .map_err(|e| dualnic_core::msgref!("DNERR", 81; &e.to_string()))
    }

    /// 数据库文件大小（字节）。内存降级库返回 None。
    pub fn db_size_bytes(&self) -> Option<u64> {
        if !self.file_backed {
            return None;
        }
        std::fs::metadata(paths::db_path()).ok().map(|m| m.len())
    }
}
