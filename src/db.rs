//! Durable account-isolated cache, delta publication, and write intent journal.
use crate::{Error, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::Duration,
};

fn storage(_: impl std::fmt::Display) -> Error {
    Error::new("storage_error", "The local database operation failed")
}
fn missing() -> Error {
    Error::new(
        "account_not_found",
        "The account is not available; select an authorized account",
    )
}
fn parse(s: String) -> Result<Value> {
    serde_json::from_str(&s).map_err(storage)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub display_name: String,
    pub is_default: bool,
    pub generation: i64,
}
#[derive(Clone, Debug, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub account_ids: Vec<String>,
    #[serde(default)]
    pub list_ids: Vec<String>,
    #[serde(default)]
    pub text: String,
    pub status: Option<String>,
    pub due_from: Option<String>,
    pub due_to: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}
fn default_limit() -> usize {
    25
}
impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            account_ids: vec![],
            list_ids: vec![],
            text: String::new(),
            status: None,
            due_from: None,
            due_to: None,
            limit: 25,
            offset: 0,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Operation {
    pub operation_key: String,
    pub state: String,
    pub response: Value,
    pub request: Value,
    pub created: bool,
}
#[derive(Debug)]
pub struct SyncCursor {
    pub url: String,
    pub rebuilding: bool,
}

pub struct Db(Mutex<Connection>);
impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(storage)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path)
                .map_err(storage)?;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(storage)?;
        }
        let conn = Connection::open(path).map_err(storage)?;
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(storage)?;
        if version > 2 {
            return Err(Error::new(
                "unsupported_database_version",
                "This database requires a newer server version",
            ));
        }
        conn.busy_timeout(Duration::from_secs(5)).map_err(storage)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA secure_delete=ON;
        CREATE TABLE IF NOT EXISTS accounts (
            id TEXT PRIMARY KEY, display_name TEXT NOT NULL, is_default INTEGER NOT NULL DEFAULT 0,
            encrypted_token TEXT NOT NULL, generation INTEGER NOT NULL);
        CREATE UNIQUE INDEX IF NOT EXISTS one_default ON accounts(is_default) WHERE is_default=1;
        CREATE TABLE IF NOT EXISTS lists (
            account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
            id TEXT NOT NULL, data TEXT NOT NULL, PRIMARY KEY(account_id,id));
        CREATE TABLE IF NOT EXISTS tasks (
            account_id TEXT NOT NULL, list_id TEXT NOT NULL, id TEXT NOT NULL, data TEXT NOT NULL,
            title TEXT NOT NULL, body TEXT NOT NULL, status TEXT NOT NULL, due_date TEXT,
            PRIMARY KEY(account_id,list_id,id),
            FOREIGN KEY(account_id,list_id) REFERENCES lists(account_id,id) ON DELETE CASCADE);
        CREATE INDEX IF NOT EXISTS task_filters ON tasks(account_id,status,due_date);
        CREATE TABLE IF NOT EXISTS sync_cursors (
            account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
            stream TEXT NOT NULL, next_url TEXT, delta_url TEXT, rebuilding INTEGER NOT NULL DEFAULT 1,
            PRIMARY KEY(account_id,stream));
        CREATE TABLE IF NOT EXISTS sync_stage (
            account_id TEXT NOT NULL, stream TEXT NOT NULL, id TEXT NOT NULL, data TEXT NOT NULL,
            PRIMARY KEY(account_id,stream,id),
            FOREIGN KEY(account_id,stream) REFERENCES sync_cursors(account_id,stream) ON DELETE CASCADE);
        CREATE TABLE IF NOT EXISTS sync_status (
            account_id TEXT PRIMARY KEY REFERENCES accounts(id) ON DELETE CASCADE,
            state TEXT NOT NULL, last_success TEXT, error_code TEXT);
        CREATE TABLE IF NOT EXISTS operations (
            account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
            operation_key TEXT NOT NULL, fingerprint TEXT NOT NULL, state TEXT NOT NULL,
            request TEXT NOT NULL, response TEXT NOT NULL DEFAULT 'null',
            created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY(account_id,operation_key));
        UPDATE operations SET state='unknown' WHERE state='pending';
        UPDATE sync_status SET state='interrupted' WHERE state='syncing';
        CREATE VIRTUAL TABLE IF NOT EXISTS task_fts USING fts5(title,body,content='tasks',content_rowid='rowid',tokenize='trigram case_sensitive 1');
        CREATE TRIGGER IF NOT EXISTS task_fts_insert AFTER INSERT ON tasks BEGIN
            INSERT INTO task_fts(rowid,title,body) VALUES(new.rowid,new.title,new.body); END;
        CREATE TRIGGER IF NOT EXISTS task_fts_delete AFTER DELETE ON tasks BEGIN
            INSERT INTO task_fts(task_fts,rowid,title,body) VALUES('delete',old.rowid,old.title,old.body); END;
        CREATE TRIGGER IF NOT EXISTS task_fts_update AFTER UPDATE ON tasks BEGIN
            INSERT INTO task_fts(task_fts,rowid,title,body) VALUES('delete',old.rowid,old.title,old.body);
            INSERT INTO task_fts(rowid,title,body) VALUES(new.rowid,new.title,new.body); END;").map_err(storage)?;
        if version < 2 {
            conn.execute("INSERT INTO task_fts(task_fts) VALUES('rebuild')", [])
                .map_err(storage)?;
            let rows: Vec<(i64, String)> = {
                let mut stmt = conn
                    .prepare("SELECT rowid,data FROM tasks")
                    .map_err(storage)?;
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                    .map_err(storage)?
                    .collect::<std::result::Result<_, _>>()
                    .map_err(storage)?
            };
            for (id, data) in rows {
                let v = parse(data)?;
                conn.execute(
                    "UPDATE tasks SET body=?2 WHERE rowid=?1",
                    params![id, search_body(&v)],
                )
                .map_err(storage)?;
            }
            conn.execute("INSERT INTO task_fts(task_fts) VALUES('rebuild')", [])
                .map_err(storage)?;
            conn.pragma_update(None, "user_version", 2)
                .map_err(storage)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if path.exists() {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                    .map_err(storage)?;
            }
        }
        Ok(Self(Mutex::new(conn)))
    }
    fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.0.lock().map_err(storage)
    }
    pub fn accounts(&self) -> Result<Vec<Account>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare("SELECT id,display_name,is_default,generation FROM accounts ORDER BY is_default DESC,id").map_err(storage)?;
        stmt.query_map([], row_account)
            .map_err(storage)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(storage)
    }
    pub fn account(&self, id: &str) -> Result<Account> {
        self.conn()?
            .query_row(
                "SELECT id,display_name,is_default,generation FROM accounts WHERE id=?1",
                [id],
                row_account,
            )
            .optional()
            .map_err(storage)?
            .ok_or_else(missing)
    }
    pub fn resolve_account(&self, id: Option<&str>) -> Result<Account> {
        if let Some(id) = id {
            return self.account(id);
        }
        self.accounts()?
            .into_iter()
            .find(|a| a.is_default)
            .ok_or_else(missing)
    }
    pub fn upsert_account(
        &self,
        id: &str,
        display_name: &str,
        encrypted_token: &str,
    ) -> Result<Account> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(storage)?;
        let is_default = tx
            .query_row("SELECT is_default FROM accounts WHERE id=?1", [id], |r| {
                r.get::<_, bool>(0)
            })
            .optional()
            .map_err(storage)?
            .unwrap_or(
                tx.query_row("SELECT COUNT(*)=0 FROM accounts", [], |r| {
                    r.get::<_, bool>(0)
                })
                .map_err(storage)?,
            );
        let generation = (rand::random::<u64>() & i64::MAX as u64) as i64;
        // Preserve completed cache on renewed consent, invalidate in-flight work and pending cursor pages.
        tx.execute("INSERT INTO accounts(id,display_name,is_default,encrypted_token,generation) VALUES(?1,?2,?3,?4,?5)
            ON CONFLICT(id) DO UPDATE SET display_name=excluded.display_name,encrypted_token=excluded.encrypted_token,generation=excluded.generation", params![id,display_name,is_default,encrypted_token,generation]).map_err(storage)?;
        tx.execute("DELETE FROM sync_cursors WHERE account_id=?1", [id])
            .map_err(storage)?;
        tx.execute(
            "UPDATE operations SET state='unknown' WHERE account_id=?1 AND state='pending'",
            [id],
        )
        .map_err(storage)?;
        tx.execute("INSERT INTO sync_status(account_id,state) VALUES(?1,'not_synced') ON CONFLICT(account_id) DO UPDATE SET state='not_synced',error_code=NULL", [id]).map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(Account {
            id: id.into(),
            display_name: display_name.into(),
            is_default,
            generation,
        })
    }
    pub fn token(&self, id: &str) -> Result<(String, i64)> {
        self.conn()?
            .query_row(
                "SELECT encrypted_token,generation FROM accounts WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(storage)?
            .ok_or_else(missing)
    }
    pub fn update_token(&self, id: &str, generation: i64, encrypted_token: &str) -> Result<()> {
        if self
            .conn()?
            .execute(
                "UPDATE accounts SET encrypted_token=?3 WHERE id=?1 AND generation=?2",
                params![id, generation, encrypted_token],
            )
            .map_err(storage)?
            != 1
        {
            return Err(missing());
        }
        Ok(())
    }
    pub fn set_default(&self, id: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(storage)?;
        if !tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1)",
                [id],
                |r| r.get::<_, bool>(0),
            )
            .map_err(storage)?
        {
            return Err(missing());
        }
        tx.execute("UPDATE accounts SET is_default=0 WHERE is_default=1", [])
            .map_err(storage)?;
        tx.execute("UPDATE accounts SET is_default=1 WHERE id=?1", [id])
            .map_err(storage)?;
        tx.commit().map_err(storage)
    }
    pub fn remove_account(&self, id: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(storage)?;
        tx.execute("DELETE FROM accounts WHERE id=?1", [id])
            .map_err(storage)?;
        tx.execute("UPDATE accounts SET is_default=1 WHERE id=(SELECT id FROM accounts ORDER BY id LIMIT 1) AND NOT EXISTS(SELECT 1 FROM accounts WHERE is_default=1)",[]).map_err(storage)?;
        tx.commit().map_err(storage)
    }
    pub fn lists(&self, account: &str) -> Result<Vec<Value>> {
        self.account(account)?;
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT data FROM lists WHERE account_id=?1 ORDER BY id")
            .map_err(storage)?;
        stmt.query_map([account], |r| r.get::<_, String>(0))
            .map_err(storage)?
            .map(|v| parse(v.map_err(storage)?))
            .collect()
    }
    pub fn tasks(&self, account: &str, list: &str) -> Result<Vec<Value>> {
        self.account(account)?;
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT data FROM tasks WHERE account_id=?1 AND list_id=?2 ORDER BY id")
            .map_err(storage)?;
        stmt.query_map(params![account, list], |r| r.get::<_, String>(0))
            .map_err(storage)?
            .map(|v| parse(v.map_err(storage)?))
            .collect()
    }
    /// Bounded cache listing without materializing the complete account.
    pub fn cached_page(
        &self,
        account: &str,
        list: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Value>> {
        self.account(account)?;
        if limit > 101 || offset > 1_000_000 {
            return Err(Error::new("invalid_input", "Invalid page bounds"));
        }
        let conn = self.conn()?;
        let (sql, values): (String, Vec<rusqlite::types::Value>) = if let Some(list) = list {
            ("SELECT data FROM tasks WHERE account_id=? AND list_id=? ORDER BY id LIMIT ? OFFSET ?".into(),vec![account.to_owned().into(),list.to_owned().into(),(limit as i64).into(),(offset as i64).into()])
        } else {
            (
                "SELECT data FROM lists WHERE account_id=? ORDER BY id LIMIT ? OFFSET ?".into(),
                vec![
                    account.to_owned().into(),
                    (limit as i64).into(),
                    (offset as i64).into(),
                ],
            )
        };
        let mut stmt = conn.prepare(&sql).map_err(storage)?;
        stmt.query_map(rusqlite::params_from_iter(values), |r| {
            r.get::<_, String>(0)
        })
        .map_err(storage)?
        .map(|r| parse(r.map_err(storage)?))
        .collect()
    }
    pub fn task(&self, account: &str, list: &str, id: &str) -> Result<Value> {
        self.account(account)?;
        self.conn()?
            .query_row(
                "SELECT data FROM tasks WHERE account_id=?1 AND list_id=?2 AND id=?3",
                params![account, list, id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(storage)?
            .ok_or_else(|| Error::new("not_found", "Task is not present in the cache"))
            .and_then(parse)
    }
    pub fn put_list(&self, account: &str, generation: i64, value: &Value) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(storage)?;
        fence(&tx, account, generation)?;
        put_list_tx(&tx, account, value)?;
        tx.commit().map_err(storage)
    }
    pub fn delete_list(&self, account: &str, generation: i64, id: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(storage)?;
        fence(&tx, account, generation)?;
        delete_list_tx(&tx, account, id)?;
        tx.commit().map_err(storage)
    }
    pub fn put_task(
        &self,
        account: &str,
        generation: i64,
        list: &str,
        value: &Value,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(storage)?;
        fence(&tx, account, generation)?;
        put_task_tx(&tx, account, list, value)?;
        tx.commit().map_err(storage)
    }
    pub fn delete_task(&self, account: &str, generation: i64, list: &str, id: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(storage)?;
        fence(&tx, account, generation)?;
        tx.execute(
            "DELETE FROM tasks WHERE account_id=?1 AND list_id=?2 AND id=?3",
            params![account, list, id],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)
    }
    pub fn search(&self, q: &SearchQuery) -> Result<Value> {
        if q.limit == 0
            || q.limit > 100
            || q.offset > 100_000
            || q.text.len() > 4096
            || q.account_ids.len() > 32
            || q.list_ids.len() > 100
        {
            return Err(Error::new(
                "invalid_arguments",
                "Search requires limit 1–100 and bounded text, account and list filters",
            ));
        }
        let accounts = if q.account_ids.is_empty() {
            vec![self.resolve_account(None)?.id]
        } else {
            q.account_ids.clone()
        };
        for a in &accounts {
            self.account(a)?;
        }
        for date in [&q.due_from, &q.due_to].into_iter().flatten() {
            if chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err() {
                return Err(Error::new(
                    "invalid_arguments",
                    "Date filters must use YYYY-MM-DD",
                ));
            }
        }
        let mut clauses = vec![format!(
            "account_id IN ({})",
            vec!["?"; accounts.len()].join(",")
        )];
        let mut values: Vec<rusqlite::types::Value> =
            accounts.iter().map(|v| v.clone().into()).collect();
        if !q.list_ids.is_empty() {
            clauses.push(format!(
                "list_id IN ({})",
                vec!["?"; q.list_ids.len()].join(",")
            ));
            values.extend(q.list_ids.iter().map(|s| s.clone().into()));
        }
        if !q.text.is_empty() {
            let needle = q.text.to_lowercase();
            if needle.chars().count() >= 3 {
                clauses.push("rowid IN (SELECT rowid FROM task_fts WHERE task_fts MATCH ?)".into());
                values.push(format!("\"{}\"", needle.replace('"', "\"\"")).into());
            }
            // Candidate verification preserves literal substring semantics even for FTS punctuation.
            clauses.push("(instr(title,?)>0 OR instr(body,?)>0)".into());
            values.push(needle.clone().into());
            values.push(needle.into());
        }
        if let Some(v) = &q.status {
            clauses.push("status=?".into());
            values.push(v.clone().into());
        }
        if let Some(v) = &q.due_from {
            clauses.push("substr(due_date,1,10)>=?".into());
            values.push(v.clone().into());
        }
        if let Some(v) = &q.due_to {
            clauses.push("substr(due_date,1,10)<=?".into());
            values.push(v.clone().into());
        }
        let sql = format!(
            "SELECT account_id,list_id,data FROM tasks WHERE {} ORDER BY account_id,list_id,id LIMIT ? OFFSET ?",
            clauses.join(" AND ")
        );
        values.push(((q.limit + 1) as i64).into());
        values.push((q.offset as i64).into());
        let mut results = Vec::new();
        {
            let conn = self.conn()?;
            let mut stmt = conn.prepare(&sql).map_err(storage)?;
            let rows = stmt
                .query_map(rusqlite::params_from_iter(values), |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(storage)?;
            for row in rows {
                let (account, list, data) = row.map_err(storage)?;
                results.push(json!({"account_id":account,"list_id":list,"task":parse(data)?}));
            }
        }
        let has_more = results.len() > q.limit;
        results.truncate(q.limit);
        let freshness: Vec<_> = accounts
            .iter()
            .map(|id| self.sync_status(id))
            .collect::<Result<_>>()?;
        Ok(
            json!({"tasks":results,"next_offset":if has_more {Some(q.offset+q.limit)} else {None},"sync":freshness,"search_mode":"unicode_substring_fts5_trigram","date_filter_semantics":"task_local_due_date"}),
        )
    }
    pub fn sync_status(&self, account: &str) -> Result<Value> {
        self.account(account)?;
        let conn = self.conn()?;
        let (state, last, error) = conn
            .query_row(
                "SELECT state,last_success,error_code FROM sync_status WHERE account_id=?1",
                [account],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(storage)?
            .unwrap_or(("not_synced".into(), None, None));
        let pending: i64 = conn
            .query_row(
                "SELECT count(*) FROM sync_cursors WHERE account_id=?1 AND next_url IS NOT NULL",
                [account],
                |r| r.get(0),
            )
            .map_err(storage)?;
        Ok(
            json!({"account_id":account,"state":state,"last_success":last,"error_code":error,"pending_streams":pending,"complete":last.is_some() && state=="ready"}),
        )
    }
    pub fn set_sync_status(
        &self,
        account: &str,
        generation: i64,
        state: &str,
        error: Option<&str>,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(storage)?;
        fence(&tx, account, generation)?;
        tx.execute("INSERT INTO sync_status(account_id,state,last_success,error_code) VALUES(?1,?2,CASE WHEN ?2='ready' THEN strftime('%Y-%m-%dT%H:%M:%fZ','now') END,?3)
            ON CONFLICT(account_id) DO UPDATE SET state=?2,error_code=?3,last_success=CASE WHEN ?2='ready' THEN strftime('%Y-%m-%dT%H:%M:%fZ','now') ELSE sync_status.last_success END",params![account,state,error]).map_err(storage)?;
        tx.commit().map_err(storage)
    }
    pub fn sync_cursor(
        &self,
        account: &str,
        generation: i64,
        stream: &str,
        initial: &str,
    ) -> Result<SyncCursor> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(storage)?;
        fence(&tx, account, generation)?;
        tx.execute(
            "INSERT OR IGNORE INTO sync_cursors(account_id,stream,next_url) VALUES(?1,?2,?3)",
            params![account, stream, initial],
        )
        .map_err(storage)?;
        let cursor=tx.query_row("SELECT COALESCE(next_url,delta_url),rebuilding FROM sync_cursors WHERE account_id=?1 AND stream=?2",params![account,stream],|r|Ok(SyncCursor{url:r.get(0)?,rebuilding:r.get(1)?})).map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(cursor)
    }
    pub fn reset_cursor(
        &self,
        account: &str,
        generation: i64,
        stream: &str,
        initial: &str,
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(storage)?;
        fence(&tx, account, generation)?;
        tx.execute(
            "DELETE FROM sync_cursors WHERE account_id=?1 AND stream=?2",
            params![account, stream],
        )
        .map_err(storage)?;
        tx.execute(
            "INSERT INTO sync_cursors(account_id,stream,next_url) VALUES(?1,?2,?3)",
            params![account, stream, initial],
        )
        .map_err(storage)?;
        tx.commit().map_err(storage)
    }
    /// Stage each page and its continuation atomically. Publish only a completed delta cycle.
    pub fn apply_sync_page(
        &self,
        account: &str,
        generation: i64,
        stream: &str,
        values: &[Value],
        next: Option<&str>,
        delta: Option<&str>,
    ) -> Result<()> {
        if next.is_none() && delta.is_none() {
            return Err(Error::new(
                "invalid_graph_response",
                "Delta response lacks a continuation or delta link",
            ));
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(storage)?;
        fence(&tx, account, generation)?;
        let rebuilding: bool = tx
            .query_row(
                "SELECT rebuilding FROM sync_cursors WHERE account_id=?1 AND stream=?2",
                params![account, stream],
                |r| r.get(0),
            )
            .map_err(storage)?;
        for value in values {
            let id = entity_id(value)?;
            // A repeated delta ID may supply only changed fields; merge without erasing explicit nulls.
            let old: Option<String> = tx
                .query_row(
                    "SELECT data FROM sync_stage WHERE account_id=?1 AND stream=?2 AND id=?3",
                    params![account, stream, id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(storage)?;
            let staged = merge_entity(old.map(parse).transpose()?, value);
            tx.execute("INSERT INTO sync_stage(account_id,stream,id,data) VALUES(?1,?2,?3,?4) ON CONFLICT(account_id,stream,id) DO UPDATE SET data=excluded.data",params![account,stream,id,staged.to_string()]).map_err(storage)?;
        }
        if let Some(next) = next {
            tx.execute(
                "UPDATE sync_cursors SET next_url=?3 WHERE account_id=?1 AND stream=?2",
                params![account, stream, next],
            )
            .map_err(storage)?;
        } else {
            let staged: Vec<Value> = {
                let mut stmt = tx
                    .prepare(
                        "SELECT data FROM sync_stage WHERE account_id=?1 AND stream=?2 ORDER BY id",
                    )
                    .map_err(storage)?;
                stmt.query_map(params![account, stream], |r| r.get::<_, String>(0))
                    .map_err(storage)?
                    .map(|v| parse(v.map_err(storage)?))
                    .collect::<Result<_>>()?
            };
            if stream == "lists" {
                if rebuilding {
                    let old: Vec<String> = {
                        let mut stmt = tx
                            .prepare("SELECT id FROM lists WHERE account_id=?1")
                            .map_err(storage)?;
                        stmt.query_map([account], |r| r.get(0))
                            .map_err(storage)?
                            .collect::<std::result::Result<_, _>>()
                            .map_err(storage)?
                    };
                    for id in old {
                        if !staged.iter().any(|v| {
                            v.get("id").and_then(Value::as_str) == Some(id.as_str())
                                && v.get("@removed").is_none()
                        }) {
                            delete_list_tx(&tx, account, &id)?;
                        }
                    }
                }
                for value in &staged {
                    if value.get("@removed").is_some() {
                        delete_list_tx(&tx, account, entity_id(value)?)?;
                    } else {
                        put_list_tx(&tx, account, value)?;
                    }
                }
            } else if let Some(list) = stream.strip_prefix("tasks:") {
                if rebuilding {
                    tx.execute(
                        "DELETE FROM tasks WHERE account_id=?1 AND list_id=?2",
                        params![account, list],
                    )
                    .map_err(storage)?;
                }
                for value in &staged {
                    if value.get("@removed").is_some() {
                        tx.execute(
                            "DELETE FROM tasks WHERE account_id=?1 AND list_id=?2 AND id=?3",
                            params![account, list, entity_id(value)?],
                        )
                        .map_err(storage)?;
                    } else {
                        put_task_tx(&tx, account, list, value)?;
                    }
                }
            } else {
                return Err(Error::new(
                    "invalid_sync_stream",
                    "Unknown synchronization stream",
                ));
            }
            tx.execute(
                "DELETE FROM sync_stage WHERE account_id=?1 AND stream=?2",
                params![account, stream],
            )
            .map_err(storage)?;
            tx.execute("UPDATE sync_cursors SET next_url=NULL,delta_url=?3,rebuilding=0 WHERE account_id=?1 AND stream=?2",params![account,stream,delta]).map_err(storage)?;
        }
        tx.commit().map_err(storage)
    }
    pub fn operation_claim(
        &self,
        account: &str,
        key: &str,
        fingerprint: &str,
        request: &Value,
    ) -> Result<Operation> {
        self.claim_operation(account, None, key, fingerprint, request)
    }
    pub fn operation_claim_at_generation(
        &self,
        account: &str,
        generation: i64,
        key: &str,
        fingerprint: &str,
        request: &Value,
    ) -> Result<Operation> {
        self.claim_operation(account, Some(generation), key, fingerprint, request)
    }
    fn claim_operation(
        &self,
        account: &str,
        generation: Option<i64>,
        key: &str,
        fingerprint: &str,
        request: &Value,
    ) -> Result<Operation> {
        if key.is_empty() || key.len() > 128 || fingerprint.is_empty() {
            return Err(Error::new(
                "invalid_arguments",
                "An operation_key of 1–128 characters is required",
            ));
        }
        let mut conn = self.conn()?;
        let tx = conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(storage)?;
        if let Some(generation) = generation {
            fence(&tx, account, generation)?;
        }
        if !tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1)",
                [account],
                |r| r.get::<_, bool>(0),
            )
            .map_err(storage)?
        {
            return Err(missing());
        }
        if let Some((old_hash,state,response,old_request))=tx.query_row("SELECT fingerprint,state,response,request FROM operations WHERE account_id=?1 AND operation_key=?2",params![account,key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?))).optional().map_err(storage)? {
            if old_hash!=fingerprint {return Err(Error::new("operation_conflict","This operation_key was already used with different arguments"));}
            return Ok(Operation{operation_key:key.into(),state,response:parse(response)?,request:parse(old_request)?,created:false});
        }
        tx.execute("INSERT INTO operations(account_id,operation_key,fingerprint,state,request) VALUES(?1,?2,?3,'pending',?4)",params![account,key,fingerprint,request.to_string()]).map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(Operation {
            operation_key: key.into(),
            state: "pending".into(),
            response: Value::Null,
            request: request.clone(),
            created: true,
        })
    }
    pub fn operation(&self, account: &str, key: &str) -> Result<Operation> {
        self.account(account)?;
        let (state,response,request)=self.conn()?.query_row("SELECT state,response,request FROM operations WHERE account_id=?1 AND operation_key=?2",params![account,key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional().map_err(storage)?.ok_or_else(||Error::new("not_found","Operation key was not found for this account"))?;
        Ok(Operation {
            operation_key: key.into(),
            state,
            response: parse(response)?,
            request: parse(request)?,
            created: false,
        })
    }
    pub fn operation_finish(
        &self,
        account: &str,
        key: &str,
        state: &str,
        response: &Value,
    ) -> Result<()> {
        self.finish_operation(account, None, key, state, response)
    }
    pub fn operation_finish_at_generation(
        &self,
        account: &str,
        generation: i64,
        key: &str,
        state: &str,
        response: &Value,
    ) -> Result<()> {
        self.finish_operation(account, Some(generation), key, state, response)
    }
    fn finish_operation(
        &self,
        account: &str,
        generation: Option<i64>,
        key: &str,
        state: &str,
        response: &Value,
    ) -> Result<()> {
        if !["succeeded", "unknown", "failed"].contains(&state) {
            return Err(Error::new(
                "invalid_operation_state",
                "Unsupported write outcome",
            ));
        }
        let mut conn = self.conn()?;
        let tx = conn.transaction().map_err(storage)?;
        if let Some(generation) = generation {
            fence(&tx, account, generation)?;
        }
        if tx.execute("UPDATE operations SET state=?3,response=?4,updated_at=CURRENT_TIMESTAMP WHERE account_id=?1 AND operation_key=?2 AND state IN ('pending','unknown')",params![account,key,state,response.to_string()]).map_err(storage)?!=1 {return Err(Error::new("operation_not_pending","The operation is missing or already finalized"));}
        tx.commit().map_err(storage)
    }
}
fn row_account(r: &rusqlite::Row<'_>) -> rusqlite::Result<Account> {
    Ok(Account {
        id: r.get(0)?,
        display_name: r.get(1)?,
        is_default: r.get(2)?,
        generation: r.get(3)?,
    })
}
fn fence(tx: &Transaction<'_>, account: &str, generation: i64) -> Result<()> {
    if !tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE id=?1 AND generation=?2)",
            params![account, generation],
            |r| r.get::<_, bool>(0),
        )
        .map_err(storage)?
    {
        return Err(Error::new(
            "account_changed",
            "The account was revoked or reauthorized during this operation",
        ));
    }
    Ok(())
}
fn entity_id(v: &Value) -> Result<&str> {
    v.get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::new("invalid_graph_response", "Graph entity is missing its ID"))
}
fn merge_entity(old: Option<Value>, incoming: &Value) -> Value {
    if incoming.get("@removed").is_some() {
        return incoming.clone();
    }
    let mut merged = old
        .filter(|v| v.get("@removed").is_none())
        .unwrap_or_else(|| json!({}));
    if let (Some(dest), Some(src)) = (merged.as_object_mut(), incoming.as_object()) {
        for (k, v) in src {
            dest.insert(k.clone(), v.clone());
        }
    }
    merged
}
fn put_list_tx(tx: &Transaction<'_>, account: &str, v: &Value) -> Result<()> {
    let old: Option<String> = tx
        .query_row(
            "SELECT data FROM lists WHERE account_id=?1 AND id=?2",
            params![account, entity_id(v)?],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    let merged = merge_entity(old.map(parse).transpose()?, v);
    tx.execute("INSERT INTO lists(account_id,id,data) VALUES(?1,?2,?3) ON CONFLICT(account_id,id) DO UPDATE SET data=excluded.data",params![account,entity_id(v)?,merged.to_string()]).map_err(storage)?;
    Ok(())
}
fn delete_list_tx(tx: &Transaction<'_>, account: &str, id: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM sync_cursors WHERE account_id=?1 AND stream=?2",
        params![account, format!("tasks:{id}")],
    )
    .map_err(storage)?;
    tx.execute(
        "DELETE FROM lists WHERE account_id=?1 AND id=?2",
        params![account, id],
    )
    .map_err(storage)?;
    Ok(())
}
fn put_task_tx(tx: &Transaction<'_>, account: &str, list: &str, v: &Value) -> Result<()> {
    let id = entity_id(v)?;
    // Graph delta properties can be partial. Merge into the existing representation, preserving explicit nulls.
    let old: Option<String> = tx
        .query_row(
            "SELECT data FROM tasks WHERE account_id=?1 AND list_id=?2 AND id=?3",
            params![account, list, id],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    let mut merged = old.map(parse).transpose()?.unwrap_or_else(|| json!({}));
    if let (Some(dest), Some(src)) = (merged.as_object_mut(), v.as_object()) {
        for (k, v) in src {
            dest.insert(k.clone(), v.clone());
        }
    }
    let title = merged
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let body = search_body(&merged);
    let status = merged
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("notStarted");
    let due = merged
        .pointer("/dueDateTime/dateTime")
        .and_then(Value::as_str);
    tx.execute("INSERT INTO tasks(account_id,list_id,id,data,title,body,status,due_date) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
        ON CONFLICT(account_id,list_id,id) DO UPDATE SET data=excluded.data,title=excluded.title,body=excluded.body,status=excluded.status,due_date=excluded.due_date",params![account,list,id,merged.to_string(),title,body,status,due]).map_err(storage)?;
    Ok(())
}

fn search_body(task: &Value) -> String {
    let body = task
        .pointer("/body/content")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !task
        .pointer("/body/contentType")
        .and_then(Value::as_str)
        .is_some_and(|t| t.eq_ignore_ascii_case("html"))
    {
        return body.to_lowercase();
    }
    html_escape::decode_html_entities(&html_text(body)).to_lowercase()
}
/// Extract text only. No URL fetches, script execution, or browser rendering.
fn html_text(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        if rest.starts_with("<!--") {
            if let Some(end) = rest.find("-->") {
                rest = &rest[end + 3..];
                continue;
            }
            rest = "";
            break;
        }
        let mut quote = None;
        let mut end = None;
        for (i, c) in rest.char_indices().skip(1) {
            if let Some(q) = quote {
                if c == q {
                    quote = None;
                }
            } else if c == '\'' || c == '"' {
                quote = Some(c);
            } else if c == '>' {
                end = Some(i);
                break;
            }
        }
        let Some(end) = end else {
            out.push_str(rest);
            return out;
        };
        let raw = rest[1..end].trim();
        let closing = raw.starts_with('/');
        let name = raw
            .trim_start_matches('/')
            .split(|c: char| !c.is_ascii_alphanumeric())
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        rest = &rest[end + 1..];
        if !closing && (name == "script" || name == "style") {
            if let Some(close) = rest.to_ascii_lowercase().find(&format!("</{name}")) {
                rest = &rest[close..];
            } else {
                rest = "";
                break;
            }
        }
        if [
            "br",
            "p",
            "div",
            "li",
            "tr",
            "td",
            "th",
            "h1",
            "h2",
            "h3",
            "h4",
            "h5",
            "h6",
            "section",
            "article",
            "blockquote",
        ]
        .contains(&name.as_str())
            && !out.ends_with('\n')
        {
            out.push('\n');
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (tempfile::TempDir, Db, Account) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        let a = db.upsert_account("a", "Account A", "encrypted").unwrap();
        (dir, db, a)
    }
    fn seed(db: &Db, a: &Account, list: &str, task: &str, title: &str) {
        db.put_list(
            &a.id,
            a.generation,
            &json!({"id":list,"displayName":"清单"}),
        )
        .unwrap();
        db.put_task(&a.id,a.generation,list,&json!({"id":task,"title":title,"body":{"content":"中文正文 ABC123"},"status":"notStarted","dueDateTime":{"dateTime":"2026-11-06T00:00:00","timeZone":"China Standard Time"}})).unwrap();
    }
    #[test]
    fn chinese_search_scopes_and_pagination() {
        let (_dir, db, a) = setup();
        let b = db.upsert_account("b", "B", "secret").unwrap();
        seed(&db, &a, "one", "same", "采购会议");
        seed(&db, &a, "two", "other", "审查会议");
        seed(&db, &b, "one", "same", "别的会议");
        let q = SearchQuery {
            text: "会议".into(),
            limit: 1,
            ..Default::default()
        };
        let first = db.search(&q).unwrap();
        assert_eq!(first["tasks"].as_array().unwrap().len(), 1);
        assert_eq!(first["next_offset"], 1);
        assert_eq!(first["tasks"][0]["account_id"], "a");
        let second = db.search(&SearchQuery { offset: 1, ..q }).unwrap();
        assert!(second["next_offset"].is_null());
        let both = db
            .search(&SearchQuery {
                account_ids: vec!["a".into(), "b".into()],
                text: "正文 abc123".into(),
                due_from: Some("2026-11-06".into()),
                due_to: Some("2026-11-06".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(both["tasks"].as_array().unwrap().len(), 3);
        assert!(
            db.search(&SearchQuery {
                account_ids: vec!["missing".into()],
                ..Default::default()
            })
            .is_err()
        );
    }
    #[test]
    fn explicit_null_clears_due_and_partial_preserves_body() {
        let (_dir, db, a) = setup();
        seed(&db, &a, "list", "task", "旧标题");
        db.put_task(
            "a",
            a.generation,
            "list",
            &json!({"id":"task","title":"新标题","dueDateTime":null}),
        )
        .unwrap();
        let task = db.task("a", "list", "task").unwrap();
        assert!(task["dueDateTime"].is_null());
        assert_eq!(task["body"]["content"], "中文正文 ABC123");
    }
    #[test]
    fn delta_stages_and_resumes_atomically_across_restart() {
        let (dir, db, a) = setup();
        seed(&db, &a, "list", "old", "旧任务");
        db.sync_cursor("a", a.generation, "tasks:list", "initial")
            .unwrap();
        db.apply_sync_page(
            "a",
            a.generation,
            "tasks:list",
            &[json!({"id":"new","title":"新任务","body":{"content":"保留正文"}})],
            Some("page2"),
            None,
        )
        .unwrap();
        assert!(db.task("a", "list", "new").is_err());
        assert!(db.task("a", "list", "old").is_ok());
        drop(db);
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        assert_eq!(
            db.sync_cursor("a", a.generation, "tasks:list", "initial")
                .unwrap()
                .url,
            "page2"
        );
        db.apply_sync_page(
            "a",
            a.generation,
            "tasks:list",
            &[json!({"id":"new","title":"最终标题"})],
            None,
            Some("delta1"),
        )
        .unwrap();
        let task = db.task("a", "list", "new").unwrap();
        assert_eq!(task["title"], "最终标题");
        assert_eq!(task["body"]["content"], "保留正文");
        assert!(db.task("a", "list", "old").is_err());
        assert_eq!(
            db.sync_cursor("a", a.generation, "tasks:list", "initial")
                .unwrap()
                .url,
            "delta1"
        );
    }
    #[test]
    fn delta_tombstones_and_expiry_rebuild() {
        let (_dir, db, a) = setup();
        seed(&db, &a, "list", "old", "old");
        db.sync_cursor("a", a.generation, "tasks:list", "initial")
            .unwrap();
        db.apply_sync_page(
            "a",
            a.generation,
            "tasks:list",
            &[json!({"id":"old","title":"old"})],
            None,
            Some("d1"),
        )
        .unwrap();
        db.apply_sync_page(
            "a",
            a.generation,
            "tasks:list",
            &[
                json!({"id":"old","@removed":{"reason":"deleted"}}),
                json!({"id":"new","title":"new"}),
            ],
            None,
            Some("d2"),
        )
        .unwrap();
        assert!(db.task("a", "list", "old").is_err());
        assert!(db.task("a", "list", "new").is_ok());
        db.reset_cursor("a", a.generation, "tasks:list", "initial")
            .unwrap();
        assert!(db.task("a", "list", "new").is_ok());
        db.apply_sync_page("a", a.generation, "tasks:list", &[], None, Some("d3"))
            .unwrap();
        assert!(db.tasks("a", "list").unwrap().is_empty());
    }
    #[test]
    fn revoke_cascades_and_generation_fences_reauthorization() {
        let (_dir, db, a) = setup();
        seed(&db, &a, "list", "task", "private");
        db.operation_claim("a", "op", "fp", &json!({"title":"private"}))
            .unwrap();
        db.sync_cursor("a", a.generation, "tasks:list", "initial")
            .unwrap();
        db.remove_account("a").unwrap();
        assert!(db.token("a").is_err());
        let replacement = db.upsert_account("a", "Again", "new-secret").unwrap();
        assert_ne!(a.generation, replacement.generation);
        assert!(
            db.put_task("a", a.generation, "list", &json!({"id":"task"}))
                .is_err()
        );
        assert!(db.update_token("a", a.generation, "old-secret").is_err());
        assert!(
            db.apply_sync_page("a", a.generation, "tasks:list", &[], None, Some("delta"))
                .is_err()
        );
        assert!(db.operation("a", "op").is_err());
        assert!(db.lists("a").unwrap().is_empty());
    }
    #[test]
    fn deleting_list_cleans_tasks_and_streams() {
        let (_dir, db, a) = setup();
        seed(&db, &a, "list", "task", "private");
        db.sync_cursor("a", a.generation, "tasks:list", "initial")
            .unwrap();
        db.sync_cursor("a", a.generation, "lists", "lists-initial")
            .unwrap();
        db.apply_sync_page(
            "a",
            a.generation,
            "lists",
            &[json!({"id":"list","@removed":{}})],
            None,
            Some("lists-delta"),
        )
        .unwrap();
        assert!(db.tasks("a", "list").unwrap().is_empty());
        assert_eq!(db.sync_status("a").unwrap()["pending_streams"], 0);
    }
    #[test]
    fn journal_replay_conflict_and_restart_unknown() {
        let (dir, db, a) = setup();
        let request = json!({"title":"Write"});
        let first = db
            .operation_claim("a", "key", "fingerprint", &request)
            .unwrap();
        assert!(first.created);
        assert!(
            !db.operation_claim("a", "key", "fingerprint", &request)
                .unwrap()
                .created
        );
        assert_eq!(
            db.operation_claim("a", "key", "different", &request)
                .unwrap_err()
                .code,
            "operation_conflict"
        );
        drop(db);
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        assert_eq!(db.operation("a", "key").unwrap().state, "unknown");
        db.operation_finish_at_generation(
            "a",
            a.generation,
            "key",
            "succeeded",
            &json!({"id":"server-id"}),
        )
        .unwrap();
        assert_eq!(
            db.operation_claim("a", "key", "fingerprint", &request)
                .unwrap()
                .response["id"],
            "server-id"
        );
        assert!(
            db.operation_finish("a", "key", "failed", &Value::Null)
                .is_err()
        );
    }
    #[test]
    fn journal_and_defaults_are_account_isolated() {
        let (_dir, db, a) = setup();
        let b = db.upsert_account("b", "B", "x").unwrap();
        db.operation_claim("a", "key", "fp-a", &Value::Null)
            .unwrap();
        assert!(
            db.operation_claim("b", "key", "fp-b", &Value::Null)
                .unwrap()
                .created
        );
        db.set_default("b").unwrap();
        assert_eq!(db.resolve_account(None).unwrap().id, b.id);
        assert!(db.set_default("absent").is_err());
        assert_eq!(db.resolve_account(None).unwrap().id, "b");
        db.remove_account("b").unwrap();
        assert_eq!(db.resolve_account(None).unwrap().id, a.id);
    }
    #[test]
    fn invalid_page_never_advances_cursor() {
        let (_dir, db, a) = setup();
        db.sync_cursor("a", a.generation, "lists", "initial")
            .unwrap();
        assert!(
            db.apply_sync_page(
                "a",
                a.generation,
                "lists",
                &[json!({"displayName":"No id"})],
                None,
                Some("d")
            )
            .is_err()
        );
        assert_eq!(
            db.sync_cursor("a", a.generation, "lists", "initial")
                .unwrap()
                .url,
            "initial"
        );
        assert!(
            db.apply_sync_page("a", a.generation, "lists", &[], None, None)
                .is_err()
        );
    }
    #[test]
    fn persistent_trigram_search_handles_short_chinese_html_and_literal_operators() {
        let (dir, db, a) = setup();
        db.put_list("a", a.generation, &json!({"id":"list"}))
            .unwrap();
        db.put_task("a",a.generation,"list",&json!({"id":"task","title":"中文搜索 ABC-123 OR \"quote\" *","body":{"contentType":"html","content":"<p>中<strong>文</strong>正文 &amp; &#x4F1A;&#35758;</p><script>隐藏词</script><style>.秘密{}</style>"}})).unwrap();
        for text in [
            "中",
            "中文",
            "中文搜",
            "中文正文",
            "abc-123",
            "OR \"quote\" *",
            "& 会议",
        ] {
            let result = db
                .search(&SearchQuery {
                    text: text.into(),
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(result["tasks"].as_array().unwrap().len(), 1, "{text}");
        }
        for text in ["隐藏词", "秘密", "<strong>", "not present"] {
            assert!(
                db.search(&SearchQuery {
                    text: text.into(),
                    ..Default::default()
                })
                .unwrap()["tasks"]
                    .as_array()
                    .unwrap()
                    .is_empty(),
                "{text}"
            );
        }
        drop(db);
        let db = Db::open(&dir.path().join("test.db")).unwrap();
        assert_eq!(
            db.search(&SearchQuery {
                text: "中文正文".into(),
                ..Default::default()
            })
            .unwrap()["tasks"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        db.delete_task("a", a.generation, "list", "task").unwrap();
        assert!(
            db.search(&SearchQuery {
                text: "中文正文".into(),
                ..Default::default()
            })
            .unwrap()["tasks"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn empty_cache_reports_unready_and_rejects_invalid_search_dates() {
        let (_dir, db, _) = setup();
        let result = db.search(&SearchQuery::default()).unwrap();
        assert!(result["tasks"].as_array().unwrap().is_empty());
        assert_eq!(result["sync"][0]["complete"], false);
        assert!(result["sync"][0]["last_success"].is_null());
        assert!(
            db.search(&SearchQuery {
                due_from: Some("2026-02-30".into()),
                ..Default::default()
            })
            .is_err()
        );
        assert!(
            db.search(&SearchQuery {
                limit: 101,
                ..Default::default()
            })
            .is_err()
        );
    }
    #[test]
    fn reauthorized_account_cannot_receive_old_operation_completion_or_claim() {
        let (_dir, db, a) = setup();
        db.operation_claim_at_generation("a", a.generation, "same", "fp", &Value::Null)
            .unwrap();
        db.remove_account("a").unwrap();
        let fresh = db.upsert_account("a", "Fresh", "encrypted2").unwrap();
        db.operation_claim_at_generation("a", fresh.generation, "same", "fp", &Value::Null)
            .unwrap();
        assert_eq!(
            db.operation_finish_at_generation("a", a.generation, "same", "succeeded", &Value::Null)
                .unwrap_err()
                .code,
            "account_changed"
        );
        assert_eq!(
            db.operation_claim_at_generation("a", a.generation, "other", "fp", &Value::Null)
                .unwrap_err()
                .code,
            "account_changed"
        );
        assert_eq!(db.operation("a", "same").unwrap().state, "pending");
    }
    #[test]
    fn database_migration_builds_index_and_rejects_future_version() {
        let (dir, db, a) = setup();
        seed(&db, &a, "list", "task", "迁移搜索");
        drop(db);
        let path = dir.path().join("test.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch("DROP TRIGGER task_fts_insert;DROP TRIGGER task_fts_update;DROP TRIGGER task_fts_delete;DROP TABLE task_fts;PRAGMA user_version=1;").unwrap();
        drop(conn);
        let db = Db::open(&path).unwrap();
        assert_eq!(
            db.search(&SearchQuery {
                text: "迁移搜索".into(),
                ..Default::default()
            })
            .unwrap()["tasks"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        drop(db);
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 99).unwrap();
        drop(conn);
        assert!(
            matches!(Db::open(&path),Err(Error{code,..}) if code=="unsupported_database_version")
        );
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            99
        );
    }
}
