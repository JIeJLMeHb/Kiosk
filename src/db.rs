use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use anyhow::{bail, Context, Result};
use rusqlite::Connection;

const MIGRATIONS: &[&str] = &[include_str!("../migrations/001_init.sql")];

pub struct Db {
    pub conn: Mutex<Connection>,
    pub root: PathBuf,
}

impl Db {
    pub fn open(root: &Path) -> Result<Self> {
        for d in ["store", "cache/thumb", "cache/screen", "logs"] {
            std::fs::create_dir_all(root.join(d))?;
        }
        let mut conn = Connection::open(root.join("library.db"))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(Duration::from_secs(5))?;
        migrate(&mut conn)?;

        // bundled SQLite обязан содержать FTS5 — проверяем явно и понятно падаем
        if let Err(e) = conn.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS temp.fts_probe USING fts5(x); DROP TABLE temp.fts_probe;",
        ) {
            bail!("SQLite собран без FTS5: {e}. Проверьте feature `bundled` у rusqlite.");
        }
        Ok(Self { conn: Mutex::new(conn), root: root.to_owned() })
    }

    pub fn store_path(&self, hash: &str) -> PathBuf {
        self.root.join("store").join(&hash[..2]).join(hash)
    }
    pub fn thumb_path(&self, hash: &str) -> PathBuf {
        self.root.join("cache/thumb").join(format!("{hash}.jpg"))
    }
    pub fn screen_path(&self, hash: &str, side: u32) -> PathBuf {
        self.root.join("cache/screen").join(format!("{hash}_{side}.jpg"))
    }
}

fn migrate(conn: &mut Connection) -> Result<()> {
    let v: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(v as usize) {
        let tx = conn.transaction().context("migration tx")?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
        tracing::info!("миграция {} применена", i + 1);
    }
    Ok(())
}
