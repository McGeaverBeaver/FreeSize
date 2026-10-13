//! Limits on what the internet can try without signing in: pairing codes
//! and sign-ins. Each limiter counts attempts in the last minute per
//! address and in total, so one address can't lock everyone out and many
//! addresses can't guess together.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(60);

pub struct Limiter {
    per_address: usize,
    total: usize,
    hits: Mutex<VecDeque<(Instant, Option<IpAddr>)>>,
}

impl Limiter {
    /// Wrong pairing codes: 10 a minute from one address, 20 in all.
    pub fn pairing() -> Limiter {
        Limiter::new(10, 20)
    }

    /// Sign-in starts and failed callbacks: 30 a minute from one address,
    /// 300 in all (a household's browsers, not a scripted guess).
    pub fn sign_in() -> Limiter {
        Limiter::new(30, 300)
    }

    pub fn new(per_address: usize, total: usize) -> Limiter {
        Limiter {
            per_address,
            total,
            hits: Mutex::new(VecDeque::new()),
        }
    }

    /// Whether `peer` (or everyone, when the address isn't known) has to
    /// wait.
    pub fn blocked(&self, peer: Option<IpAddr>) -> bool {
        let mut hits = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        while hits.front().is_some_and(|(t, _)| t.elapsed() > WINDOW) {
            hits.pop_front();
        }
        if hits.len() >= self.total {
            return true;
        }
        let from_peer = hits.iter().filter(|(_, p)| *p == peer).count();
        from_peer >= self.per_address
    }

    /// Counts an attempt.
    pub fn record(&self, peer: Option<IpAddr>) {
        self.hits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back((Instant::now(), peer));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_address_cant_lock_the_others_out_but_many_together_can() {
        let limiter = Limiter::new(2, 5);
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        limiter.record(Some(a));
        limiter.record(Some(a));
        assert!(limiter.blocked(Some(a)));
        assert!(!limiter.blocked(Some(b)));
        limiter.record(Some(b));
        limiter.record(Some(b));
        let c: IpAddr = "10.0.0.3".parse().unwrap();
        assert!(!limiter.blocked(Some(c)));
        limiter.record(Some(c));
        // Five in all: everyone waits now.
        assert!(limiter.blocked(Some(c)));
        assert!(limiter.blocked(None));
    }
}
