use crate::db::Persist;
use async_trait::async_trait;
use rusqlite::{params, Connection};
use std::sync::Mutex;

/// Local / VM persistence.
pub struct SqlitePersist(Mutex<Connection>);

impl SqlitePersist {
    pub fn open(url: &str) -> Result<Self, String> {
        let path = url.strip_prefix("sqlite://").unwrap_or(url);
        let conn = if path == ":memory:" { Connection::open_in_memory() } else { Connection::open(path) }.map_err(|e| e.to_string())?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS hosts (id TEXT PRIMARY KEY, created INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS cars (
               car_id TEXT NOT NULL, host_id TEXT NOT NULL, created INTEGER NOT NULL,
               PRIMARY KEY (car_id, host_id));",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self(Mutex::new(conn)))
    }
}

fn e(err: rusqlite::Error) -> String {
    err.to_string()
}

#[async_trait]
impl Persist for SqlitePersist {
    async fn load(&self) -> Result<(Vec<String>, Vec<(String, String)>), String> {
        let c = self.0.lock().unwrap();
        let hosts = c.prepare("SELECT id FROM hosts").map_err(e)?.query_map([], |r| r.get(0)).map_err(e)?.collect::<Result<_, _>>().map_err(e)?;
        let pairs = c
            .prepare("SELECT car_id, host_id FROM cars")
            .map_err(e)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(e)?
            .collect::<Result<_, _>>()
            .map_err(e)?;
        Ok((hosts, pairs))
    }
    async fn put_host(&self, id: &str) -> Result<(), String> {
        self.0.lock().unwrap().execute("INSERT OR IGNORE INTO hosts (id, created) VALUES (?1, strftime('%s','now'))", params![id]).map_err(e)?;
        Ok(())
    }
    async fn put_pairing(&self, car: &str, host: &str) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .execute("INSERT OR IGNORE INTO cars (car_id, host_id, created) VALUES (?1, ?2, strftime('%s','now'))", params![car, host])
            .map_err(e)?;
        Ok(())
    }
    async fn delete_pairing(&self, car: &str, host: &str) -> Result<(), String> {
        self.0.lock().unwrap().execute("DELETE FROM cars WHERE car_id = ?1 AND host_id = ?2", params![car, host]).map_err(e)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[tokio::test]
    async fn survives_reopen() {
        let path = std::env::temp_dir().join(format!("ps-{}.db", rand::random::<u32>()));
        let url = path.to_string_lossy().to_string();
        {
            let db = Db::open(Box::new(SqlitePersist::open(&url).unwrap())).await.unwrap();
            db.register_host("h").await.unwrap();
            db.add_pairing("c", "h").await.unwrap();
        }
        let db = Db::open(Box::new(SqlitePersist::open(&url).unwrap())).await.unwrap();
        assert!(db.is_paired("c", "h"));
        let _ = std::fs::remove_file(path);
    }
}
