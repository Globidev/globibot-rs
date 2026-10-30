use std::collections::{BTreeMap, HashMap};

use globibot_core::{
    serenity::all::UserId,
    storage::{self, StorageValue},
};

pub const RESULTS_KEY: &str = "latest";

/// Every solve reported by the crossword app, keyed by puzzle date (`YYYY-MM-DD`)
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct CrosswordResults {
    pub solves_by_date: BTreeMap<String, HashMap<UserId, u32>>,
    pub players: HashMap<UserId, Player>,
}

impl StorageValue for CrosswordResults {
    type Format = storage::JsonFormat;

    const REDIS_NS: &'static str = "crossword_results";
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Player {
    pub user_id: UserId,
    pub username: String,
    pub avatar_url: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct Leaderboard {
    pub latest_date: Option<String>,
    pub latest: Vec<LatestSolve>,
    pub all_time: Vec<PlayerStats>,
}

#[derive(Debug, serde::Serialize)]
pub struct LatestSolve {
    pub player: Player,
    pub solve_time_secs: u32,
}

#[derive(Debug, serde::Serialize)]
pub struct PlayerStats {
    pub player: Player,
    pub solved: usize,
    /// Puzzles this player solved the fastest
    pub wins: usize,
    pub best_secs: u32,
    pub average_secs: u32,
    /// Consecutive puzzles solved up to the latest one
    pub current_streak: usize,
}

impl CrosswordResults {
    /// Records a solve. Only the first solve of a puzzle counts, so replays can't improve a time
    pub fn record(&mut self, player: Player, date: String, solve_time_secs: u32) -> bool {
        let user_id = player.user_id;
        self.players.insert(user_id, player);

        let solves = self.solves_by_date.entry(date).or_default();
        if solves.contains_key(&user_id) {
            return false;
        }
        solves.insert(user_id, solve_time_secs);
        true
    }

    pub fn leaderboard(&self) -> Leaderboard {
        let latest = self.solves_by_date.iter().next_back();

        let latest_solves = latest
            .map(|(_, solves)| {
                let mut latest_solves = solves
                    .iter()
                    .filter_map(|(user_id, &solve_time_secs)| {
                        Some(LatestSolve {
                            player: self.players.get(user_id)?.clone(),
                            solve_time_secs,
                        })
                    })
                    .collect::<Vec<_>>();
                latest_solves.sort_by_key(|solve| solve.solve_time_secs);
                latest_solves
            })
            .unwrap_or_default();

        let mut times_by_user = HashMap::<UserId, Vec<u32>>::new();
        let mut wins_by_user = HashMap::<UserId, usize>::new();
        for solves in self.solves_by_date.values() {
            for (&user_id, &secs) in solves {
                times_by_user.entry(user_id).or_default().push(secs);
            }
            if let Some((&winner_id, _)) = solves.iter().min_by_key(|(_, secs)| **secs) {
                *wins_by_user.entry(winner_id).or_default() += 1;
            }
        }

        let latest_day = latest.and_then(|(date, _)| day_number(date));
        let mut all_time = times_by_user
            .into_iter()
            .filter_map(|(user_id, times)| {
                let total_secs = times.iter().map(|&secs| u64::from(secs)).sum::<u64>();
                Some(PlayerStats {
                    player: self.players.get(&user_id)?.clone(),
                    solved: times.len(),
                    wins: wins_by_user.get(&user_id).copied().unwrap_or_default(),
                    best_secs: times.iter().copied().min().unwrap_or_default(),
                    average_secs: (total_secs / times.len() as u64) as u32,
                    current_streak: latest_day
                        .map_or(0, |latest_day| self.streak(user_id, latest_day)),
                })
            })
            .collect::<Vec<_>>();
        all_time.sort_by(|a, b| {
            b.solved
                .cmp(&a.solved)
                .then(a.average_secs.cmp(&b.average_secs))
        });

        Leaderboard {
            latest_date: latest.map(|(date, _)| date.clone()),
            latest: latest_solves,
            all_time,
        }
    }

    fn streak(&self, user_id: UserId, latest_day: i64) -> usize {
        let solved_days = self
            .solves_by_date
            .iter()
            .filter(|(_, solves)| solves.contains_key(&user_id))
            .filter_map(|(date, _)| day_number(date))
            .collect::<std::collections::HashSet<_>>();

        (0..)
            .take_while(|offset| solved_days.contains(&(latest_day - offset)))
            .count()
    }
}

/// Days since 1970-01-01 for a `YYYY-MM-DD` date, `None` if it isn't a valid date
pub fn day_number(date: &str) -> Option<i64> {
    let mut parts = date.splitn(3, '-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    let (year, month, day) = (
        year.parse::<i64>().ok()?,
        month.parse::<i64>().ok()?,
        day.parse::<i64>().ok()?,
    );

    let is_leap_year = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year => 29,
        2 => 28,
        _ => return None,
    };
    if !(1..=days_in_month).contains(&day) {
        return None;
    }

    // Howard Hinnant's days_from_civil
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn player(id: u64) -> Player {
        Player {
            user_id: UserId::new(id),
            username: format!("player{id}"),
            avatar_url: None,
        }
    }

    #[test]
    fn parses_dates() {
        assert_eq!(day_number("1970-01-01"), Some(0));
        assert_eq!(day_number("2000-03-01"), Some(11_017));
        assert_eq!(day_number("2024-02-29"), Some(19_782));
        assert_eq!(day_number("2023-02-29"), None);
        assert_eq!(day_number("2024-13-01"), None);
        assert_eq!(day_number("2024-1-01"), None);
        assert_eq!(day_number("today"), None);
    }

    #[test]
    fn keeps_the_first_solve_of_a_puzzle() {
        let mut results = CrosswordResults::default();
        assert!(results.record(player(1), "2026-10-30".into(), 90));
        assert!(!results.record(player(1), "2026-10-30".into(), 30));
        assert_eq!(results.solves_by_date["2026-10-30"][&UserId::new(1)], 90);
    }

    #[test]
    fn ranks_players_and_counts_streaks() {
        let mut results = CrosswordResults::default();
        results.record(player(1), "2026-10-28".into(), 60);
        results.record(player(1), "2026-10-29".into(), 50);
        results.record(player(1), "2026-10-30".into(), 70);
        results.record(player(2), "2026-10-28".into(), 40);
        results.record(player(2), "2026-10-30".into(), 30);

        let leaderboard = results.leaderboard();
        assert_eq!(leaderboard.latest_date.as_deref(), Some("2026-10-30"));

        let latest_ids = leaderboard
            .latest
            .iter()
            .map(|solve| solve.player.user_id.get())
            .collect::<Vec<_>>();
        assert_eq!(latest_ids, [2, 1]);

        let first = &leaderboard.all_time[0];
        assert_eq!(first.player.user_id.get(), 1);
        assert_eq!((first.solved, first.wins, first.current_streak), (3, 1, 3));
        assert_eq!((first.best_secs, first.average_secs), (50, 60));

        let second = &leaderboard.all_time[1];
        assert_eq!(
            (second.solved, second.wins, second.current_streak),
            (2, 2, 1)
        );
    }
}
