use std::{collections::BTreeSet, fs, io, path::PathBuf};

/// Cars this host will answer. The server also checks pairing; this is the
/// defence in depth from the plan (a compromised VM can't attach an unknown car).
/// Input injection is a separate, per-car opt-in that defaults to off.
pub struct AllowList {
    path: Option<PathBuf>,
    cars: BTreeSet<String>,
    input_enabled: BTreeSet<String>,
}

impl AllowList {
    pub fn in_memory() -> Self {
        Self { path: None, cars: Default::default(), input_enabled: Default::default() }
    }

    /// File format: one line per car, `<car_id>` or `<car_id> input`.
    pub fn load(path: PathBuf) -> io::Result<Self> {
        let mut l = Self { path: Some(path.clone()), cars: Default::default(), input_enabled: Default::default() };
        match fs::read_to_string(&path) {
            Ok(s) => {
                for line in s.lines() {
                    let mut it = line.split_whitespace();
                    if let Some(id) = it.next() {
                        l.cars.insert(id.to_string());
                        if it.next() == Some("input") {
                            l.input_enabled.insert(id.to_string());
                        }
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(l)
    }

    fn save(&self) {
        if let Some(p) = &self.path {
            let s: String = self
                .cars
                .iter()
                .map(|c| if self.input_enabled.contains(c) { format!("{c} input\n") } else { format!("{c}\n") })
                .collect();
            if let Some(d) = p.parent() {
                let _ = fs::create_dir_all(d);
            }
            let _ = fs::write(p, s);
        }
    }

    pub fn add(&mut self, car_id: &str) {
        self.cars.insert(car_id.to_string());
        self.save();
    }
    pub fn remove(&mut self, car_id: &str) {
        self.cars.remove(car_id);
        self.input_enabled.remove(car_id);
        self.save();
    }
    pub fn allows(&self, car_id: &str) -> bool {
        self.cars.contains(car_id)
    }
    pub fn set_input(&mut self, car_id: &str, on: bool) {
        if !self.cars.contains(car_id) {
            return;
        }
        if on { self.input_enabled.insert(car_id.to_string()); } else { self.input_enabled.remove(car_id); }
        self.save();
    }
    pub fn input_allowed(&self, car_id: &str) -> bool {
        self.input_enabled.contains(car_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_defaults_off_and_persists() {
        let p = std::env::temp_dir().join(format!("ps-al-{}", rand::random::<u32>()));
        let mut l = AllowList::load(p.clone()).unwrap();
        l.add("car1");
        assert!(l.allows("car1") && !l.input_allowed("car1"));
        l.set_input("car1", true);
        l.set_input("ghost", true);
        let l2 = AllowList::load(p.clone()).unwrap();
        assert!(l2.input_allowed("car1") && !l2.allows("ghost"));
        let _ = fs::remove_file(p);
    }
}
