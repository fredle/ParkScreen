use rusqlite::{params, Connection, OptionalExtension};
use std::sync::Mutex;

/// Hosts' public keys and paired cars. Tokens are stored only as SHA-256 hashes.
pub struct Db(Mutex<Connection>);

impl Db {
    pub fn open(url: &str) -> rusqlite::Result<Self> {
        let path = url.strip_prefix("sqlite://").unwrap_or(url);
        let conn = if path == ":memory:" { Connection::open_in_memory()? } else { Connection::open(path)? };
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS hosts (id TEXT PRIMARY KEY, created INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS cars (
               car_id TEXT NOT NULL, host_id TEXT NOT NULL, created INTEGER NOT NULL,
               PRIMARY KEY (car_id, host_id));",
        )?;
        Ok(Self(Mutex::new(conn)))
    }

    pub fn register_host(&self, id: &str) -> rusqlite::Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT OR IGNORE INTO hosts (id, created) VALUES (?1, strftime('%s','now'))",
            params![id],
        )?;
        Ok(())
    }

    pub fn add_pairing(&self, car_id: &str, host_id: &str) -> rusqlite::Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT OR IGNORE INTO cars (car_id, host_id, created) VALUES (?1, ?2, strftime('%s','now'))",
            params![car_id, host_id],
        )?;
        Ok(())
    }

    pub fn revoke(&self, car_id: &str, host_id: &str) -> rusqlite::Result<()> {
        self.0.lock().unwrap().execute(
            "DELETE FROM cars WHERE car_id = ?1 AND host_id = ?2",
            params![car_id, host_id],
        )?;
        Ok(())
    }

    pub fn is_paired(&self, car_id: &str, host_id: &str) -> rusqlite::Result<bool> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT 1 FROM cars WHERE car_id = ?1 AND host_id = ?2",
                params![car_id, host_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn hosts_for_car(&self, car_id: &str) -> rusqlite::Result<Vec<String>> {
        let conn = self.0.lock().unwrap();
        let mut st = conn.prepare("SELECT host_id FROM cars WHERE car_id = ?1")?;
        let rows = st.query_map(params![car_id], |r| r.get(0))?;
        rows.collect()
    }

    pub fn cars_for_host(&self, host_id: &str) -> rusqlite::Result<Vec<String>> {
        let conn = self.0.lock().unwrap();
        let mut st = conn.prepare("SELECT car_id FROM cars WHERE host_id = ?1")?;
        let rows = st.query_map(params![host_id], |r| r.get(0))?;
        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_roundtrip() {
        let db = Db::open(":memory:").unwrap();
        db.register_host("h").unwrap();
        db.add_pairing("c", "h").unwrap();
        assert!(db.is_paired("c", "h").unwrap());
        assert_eq!(db.hosts_for_car("c").unwrap(), vec!["h"]);
        assert_eq!(db.cars_for_host("h").unwrap(), vec!["c"]);
        db.revoke("c", "h").unwrap();
        assert!(!db.is_paired("c", "h").unwrap());
    }
}
