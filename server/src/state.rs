use crate::db::Db;
use rand::Rng;
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};
use tokio::sync::mpsc::UnboundedSender;

pub type Tx = UnboundedSender<String>;

pub const PAIR_CODE_TTL: Duration = Duration::from_secs(300);
const MAX_FAILED_CLAIMS: usize = 20;
const FAIL_WINDOW: Duration = Duration::from_secs(60);

pub struct AppState {
    pub db: Db,
    pub hosts: Mutex<HashMap<String, Tx>>,
    pub cars: Mutex<HashMap<String, Tx>>,
    codes: Mutex<HashMap<String, (String, Instant)>>,
    failed_claims: Mutex<Vec<Instant>>,
}

impl AppState {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            hosts: Default::default(),
            cars: Default::default(),
            codes: Default::default(),
            failed_claims: Default::default(),
        }
    }

    /// Issue a single-use 6-digit code for `host_id`, replacing any earlier one.
    pub fn new_pair_code(&self, host_id: &str) -> String {
        let mut codes = self.codes.lock().unwrap();
        codes.retain(|_, (h, exp)| h != host_id && *exp > Instant::now());
        loop {
            let code = format!("{:06}", rand::thread_rng().gen_range(0..1_000_000));
            if !codes.contains_key(&code) {
                codes.insert(code.clone(), (host_id.to_string(), Instant::now() + PAIR_CODE_TTL));
                return code;
            }
        }
    }

    /// Consume a code. Returns the host id, or `None` if wrong/expired/rate limited.
    pub fn claim_pair_code(&self, code: &str) -> Option<String> {
        let now = Instant::now();
        {
            let mut f = self.failed_claims.lock().unwrap();
            f.retain(|t| now.duration_since(*t) < FAIL_WINDOW);
            if f.len() >= MAX_FAILED_CLAIMS {
                return None;
            }
        }
        let hit = self.codes.lock().unwrap().remove(code).filter(|(_, exp)| *exp > now);
        match hit {
            Some((host, _)) => Some(host),
            None => {
                self.failed_claims.lock().unwrap().push(now);
                None
            }
        }
    }
}

pub fn send<T: serde::Serialize>(tx: &Tx, msg: &T) {
    if let Ok(s) = serde_json::to_string(msg) {
        let _ = tx.send(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_is_single_use() {
        let s = AppState::new(Db::open(":memory:").unwrap());
        let c = s.new_pair_code("h");
        assert_eq!(c.len(), 6);
        assert_eq!(s.claim_pair_code(&c).as_deref(), Some("h"));
        assert_eq!(s.claim_pair_code(&c), None);
    }

    #[test]
    fn new_code_replaces_old() {
        let s = AppState::new(Db::open(":memory:").unwrap());
        let a = s.new_pair_code("h");
        let b = s.new_pair_code("h");
        if a != b {
            assert_eq!(s.claim_pair_code(&a), None);
        }
        assert_eq!(s.claim_pair_code(&b).as_deref(), Some("h"));
    }

    #[test]
    fn brute_force_is_limited() {
        let s = AppState::new(Db::open(":memory:").unwrap());
        let code = s.new_pair_code("h");
        for _ in 0..MAX_FAILED_CLAIMS {
            assert_eq!(s.claim_pair_code("nope!!"), None);
        }
        assert_eq!(s.claim_pair_code(&code), None);
    }
}
