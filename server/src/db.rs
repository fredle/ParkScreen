//! Pairing storage. All reads are served from memory (the server is a single instance and
//! signalling does a lookup per message); writes go through to a `Persist` backend so
//! pairings survive restarts. Backends: SQLite (local/VM), Firestore (Cloud Run), none (tests).

use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

#[async_trait]
pub trait Persist: Send + Sync {
    /// Everything stored: (host ids, (car_id, host_id) pairings).
    async fn load(&self) -> Result<(Vec<String>, Vec<(String, String)>), String>;
    async fn put_host(&self, host_id: &str) -> Result<(), String>;
    async fn put_pairing(&self, car_id: &str, host_id: &str) -> Result<(), String>;
    async fn delete_pairing(&self, car_id: &str, host_id: &str) -> Result<(), String>;
}

/// Persists nothing.
pub struct NoPersist;

#[async_trait]
impl Persist for NoPersist {
    async fn load(&self) -> Result<(Vec<String>, Vec<(String, String)>), String> {
        Ok((vec![], vec![]))
    }
    async fn put_host(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    async fn put_pairing(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
    async fn delete_pairing(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Default)]
struct Mem {
    hosts: HashSet<String>,
    by_car: HashMap<String, HashSet<String>>,
    by_host: HashMap<String, HashSet<String>>,
}

impl Mem {
    fn add(&mut self, car: &str, host: &str) -> bool {
        self.by_host.entry(host.into()).or_default().insert(car.into());
        self.by_car.entry(car.into()).or_default().insert(host.into())
    }
    fn remove(&mut self, car: &str, host: &str) {
        if let Some(s) = self.by_car.get_mut(car) {
            s.remove(host);
        }
        if let Some(s) = self.by_host.get_mut(host) {
            s.remove(car);
        }
    }
}

pub struct Db {
    mem: Mutex<Mem>,
    persist: Box<dyn Persist>,
}

fn sorted(s: Option<&HashSet<String>>) -> Vec<String> {
    let mut v: Vec<String> = s.map(|s| s.iter().cloned().collect()).unwrap_or_default();
    v.sort();
    v
}

impl Db {
    pub async fn open(persist: Box<dyn Persist>) -> Result<Self, String> {
        let (hosts, pairings) = persist.load().await?;
        let mut mem = Mem { hosts: hosts.into_iter().collect(), ..Default::default() };
        for (car, host) in pairings {
            mem.add(&car, &host);
        }
        Ok(Self { mem: Mutex::new(mem), persist })
    }

    /// Non-persistent, for tests.
    pub fn in_memory() -> Self {
        Self { mem: Mutex::new(Mem::default()), persist: Box::new(NoPersist) }
    }

    /// Remember a host the first time it signs in (not on every reconnect).
    pub async fn register_host(&self, id: &str) -> Result<(), String> {
        if !self.mem.lock().unwrap().hosts.insert(id.to_string()) {
            return Ok(());
        }
        if let Err(e) = self.persist.put_host(id).await {
            self.mem.lock().unwrap().hosts.remove(id);
            return Err(e);
        }
        Ok(())
    }

    pub async fn add_pairing(&self, car_id: &str, host_id: &str) -> Result<(), String> {
        let new = self.mem.lock().unwrap().add(car_id, host_id);
        if new {
            if let Err(e) = self.persist.put_pairing(car_id, host_id).await {
                self.mem.lock().unwrap().remove(car_id, host_id);
                return Err(e);
            }
        }
        Ok(())
    }

    pub async fn revoke(&self, car_id: &str, host_id: &str) -> Result<(), String> {
        self.mem.lock().unwrap().remove(car_id, host_id);
        self.persist.delete_pairing(car_id, host_id).await
    }

    pub fn is_paired(&self, car_id: &str, host_id: &str) -> bool {
        self.mem.lock().unwrap().by_car.get(car_id).is_some_and(|s| s.contains(host_id))
    }

    pub fn hosts_for_car(&self, car_id: &str) -> Vec<String> {
        sorted(self.mem.lock().unwrap().by_car.get(car_id))
    }

    pub fn cars_for_host(&self, host_id: &str) -> Vec<String> {
        sorted(self.mem.lock().unwrap().by_host.get(host_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn pairing_roundtrip() {
        let db = Db::in_memory();
        db.register_host("h").await.unwrap();
        db.add_pairing("c", "h").await.unwrap();
        assert!(db.is_paired("c", "h"));
        assert_eq!(db.hosts_for_car("c"), vec!["h"]);
        assert_eq!(db.cars_for_host("h"), vec!["c"]);
        db.revoke("c", "h").await.unwrap();
        assert!(!db.is_paired("c", "h"));
        assert!(db.cars_for_host("h").is_empty());
    }

    /// Records calls so we can check write-through, load-on-open and rollback.
    #[derive(Default, Clone)]
    struct Rec {
        calls: Arc<Mutex<Vec<String>>>,
        fail: Arc<Mutex<bool>>,
        preload: Arc<Mutex<(Vec<String>, Vec<(String, String)>)>>,
    }
    #[async_trait]
    impl Persist for Rec {
        async fn load(&self) -> Result<(Vec<String>, Vec<(String, String)>), String> {
            Ok(self.preload.lock().unwrap().clone())
        }
        async fn put_host(&self, h: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("host {h}"));
            Ok(())
        }
        async fn put_pairing(&self, c: &str, h: &str) -> Result<(), String> {
            if *self.fail.lock().unwrap() {
                return Err("down".into());
            }
            self.calls.lock().unwrap().push(format!("pair {c} {h}"));
            Ok(())
        }
        async fn delete_pairing(&self, c: &str, h: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("del {c} {h}"));
            Ok(())
        }
    }

    #[tokio::test]
    async fn writes_through_once_and_loads_on_open() {
        let rec = Rec::default();
        *rec.preload.lock().unwrap() = (vec!["h0".into()], vec![("c0".into(), "h0".into())]);
        let db = Db::open(Box::new(rec.clone())).await.unwrap();
        assert!(db.is_paired("c0", "h0"), "loaded from the backend");
        db.register_host("h0").await.unwrap(); // already known: no write
        db.register_host("h1").await.unwrap();
        db.register_host("h1").await.unwrap(); // reconnect: no second write
        db.add_pairing("c1", "h1").await.unwrap();
        db.add_pairing("c1", "h1").await.unwrap(); // duplicate: no second write
        db.revoke("c1", "h1").await.unwrap();
        assert_eq!(*rec.calls.lock().unwrap(), vec!["host h1", "pair c1 h1", "del c1 h1"]);
    }

    #[tokio::test]
    async fn failed_persist_rolls_back() {
        let rec = Rec::default();
        let db = Db::open(Box::new(rec.clone())).await.unwrap();
        *rec.fail.lock().unwrap() = true;
        assert!(db.add_pairing("c", "h").await.is_err());
        assert!(!db.is_paired("c", "h"), "a pairing that couldn't be saved must not be usable");
    }
}
