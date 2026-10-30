use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use globibot_core::serenity::all::UserId;
use parking_lot::Mutex;

/// Per-user rate limit on features that call the LLM, to keep OpenRouter costs in check
#[derive(Debug)]
pub struct Cooldowns {
    duration: Duration,
    last_use_by_user: Mutex<HashMap<UserId, Instant>>,
}

impl Cooldowns {
    pub fn new(duration: Duration) -> Self {
        Self {
            duration,
            last_use_by_user: Mutex::default(),
        }
    }

    /// Starts a cooldown for `user_id`, unless one is still running, in which case the time left
    /// on it is returned
    pub fn try_start(&self, user_id: UserId) -> Result<(), Duration> {
        self.try_start_at(user_id, Instant::now())
    }

    fn try_start_at(&self, user_id: UserId, now: Instant) -> Result<(), Duration> {
        let mut last_use_by_user = self.last_use_by_user.lock();
        if let Some(last_use) = last_use_by_user.get(&user_id) {
            let elapsed = now.saturating_duration_since(*last_use);
            if elapsed < self.duration {
                return Err(self.duration - elapsed);
            }
        }

        last_use_by_user.insert(user_id, now);
        Ok(())
    }
}

pub fn cooldown_message(remaining: Duration) -> String {
    format!(
        "Slow down! You can use that again in {}s",
        remaining.as_secs().max(1)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_until_the_cooldown_expires() {
        let cooldowns = Cooldowns::new(Duration::from_secs(10));
        let user = UserId::new(1);
        let start = Instant::now();

        assert!(cooldowns.try_start_at(user, start).is_ok());
        assert_eq!(
            cooldowns.try_start_at(user, start + Duration::from_secs(4)),
            Err(Duration::from_secs(6))
        );
        assert!(
            cooldowns
                .try_start_at(user, start + Duration::from_secs(10))
                .is_ok()
        );
    }

    #[test]
    fn users_have_separate_cooldowns() {
        let cooldowns = Cooldowns::new(Duration::from_secs(10));
        let start = Instant::now();

        assert!(cooldowns.try_start_at(UserId::new(1), start).is_ok());
        assert!(cooldowns.try_start_at(UserId::new(2), start).is_ok());
    }

    #[test]
    fn zero_duration_never_blocks() {
        let cooldowns = Cooldowns::new(Duration::ZERO);
        let start = Instant::now();

        assert!(cooldowns.try_start_at(UserId::new(1), start).is_ok());
        assert!(cooldowns.try_start_at(UserId::new(1), start).is_ok());
    }
}
