/// Password grants belong to a terminal session and are never logged.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Read,
    Write,
}

struct Entry {
    password: String,
    read: bool,
    write: bool,
    one_time: bool,
}

#[derive(Default)]
pub struct Grants {
    entries: Vec<Entry>,
}

impl Grants {
    pub fn grant(&mut self, password: &str, direction: Direction, one_time: bool) {
        if password.is_empty() || password.len() > 128 {
            return;
        }
        let index = if let Some(index) =
            self.entries.iter().position(|e| e.password == password)
        {
            self.entries[index].one_time &= one_time;
            index
        } else {
            if self.entries.len() == 32 {
                self.entries.remove(0);
            }
            self.entries.push(Entry {
                password: password.to_owned(),
                read: false,
                write: false,
                one_time,
            });
            self.entries.len() - 1
        };
        match direction {
            Direction::Read => self.entries[index].read = true,
            Direction::Write => self.entries[index].write = true,
        }
    }

    /// One-time passwords are consumed even on a direction mismatch, matching
    /// Kitty's pop-on-check behavior. Listing-only reads must bypass this call.
    pub fn consume(&mut self, password: &str, direction: Direction) -> bool {
        let Some(index) = self.entries.iter().position(|e| e.password == password) else {
            return false;
        };
        let entry = &self.entries[index];
        let allowed = match direction {
            Direction::Read => entry.read,
            Direction::Write => entry.write,
        };
        if entry.one_time {
            self.entries.remove(index);
        }
        allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn directions_are_independent_and_persistent_grants_survive() {
        let mut grants = Grants::default();
        grants.grant("pw", Direction::Read, false);
        assert!(!grants.consume("pw", Direction::Write));
        assert!(grants.consume("pw", Direction::Read));
        assert!(grants.consume("pw", Direction::Read));
        grants.grant("pw", Direction::Write, true);
        assert!(grants.consume("pw", Direction::Write));
        assert!(grants.consume("pw", Direction::Write));
    }
    #[test]
    fn one_time_is_consumed_even_on_mismatch() {
        let mut grants = Grants::default();
        grants.grant("otp", Direction::Read, true);
        assert!(!grants.consume("otp", Direction::Write));
        assert!(!grants.consume("otp", Direction::Read));
        grants.grant("otp", Direction::Read, true);
        assert!(grants.consume("otp", Direction::Read));
        assert!(!grants.consume("otp", Direction::Read));
    }
    #[test]
    fn bounded_passwords_and_oldest_eviction() {
        let mut grants = Grants::default();
        grants.grant("", Direction::Read, false);
        grants.grant(&"x".repeat(129), Direction::Read, false);
        assert!(grants.entries.is_empty());
        for i in 0..33 {
            grants.grant(&format!("pw{i}"), Direction::Read, false);
        }
        assert_eq!(grants.entries.len(), 32);
        assert!(!grants.consume("pw0", Direction::Read));
        assert!(grants.consume("pw32", Direction::Read));
    }
}
