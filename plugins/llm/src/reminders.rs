//! Reminders the model sets with tools, sent by a task that sleeps until the next one is due.
//! ponytail: all of them in one JSON value, loaded at start and saved on each change. A sorted
//! set if there are ever thousands.

use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::{
    Extension, Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use globibot_core::{
    rpc,
    serenity::all::{ChannelId, Message, MessageId, UserId},
    storage::{self, DiscordProfile, StorageValue},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    LlmPlugin,
    message_search::{days_from_civil, parse_id},
    unix_now,
};

pub const REMINDERS_KEY: &str = "latest";
const MAX_PER_USER: usize = 10;
const MAX_AHEAD_SECS: u64 = 366 * 86_400;
/// Sent with a note when the bot was down at the time
const LATE_AFTER_SECS: u64 = 120;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reminder {
    pub id: u64,
    pub user_id: UserId,
    /// Where it's posted
    pub channel_id: ChannelId,
    /// The message asking for it, replied to when it's due unless posted elsewhere
    pub message_id: MessageId,
    /// In another channel than the one it was asked in: a message linking to the ask, not a reply
    #[serde(default)]
    pub elsewhere: bool,
    /// Unix seconds
    pub due: u64,
    pub text: String,
    /// For the website. Defaults for reminders set before it showed them
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub message_url: String,
    /// Asked in or posted to a DM: not shown to the admin on the website
    #[serde(default)]
    pub in_dm: bool,
    /// Names when it was set: "#general", "DM with someone"
    #[serde(default)]
    pub channel_name: String,
    /// `None` for reminders set before it was saved
    #[serde(default)]
    pub asked_in: Option<ChannelId>,
    #[serde(default)]
    pub asked_in_name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Reminders {
    next_id: u64,
    pending: Vec<Reminder>,
}

impl StorageValue for Reminders {
    type Format = storage::JsonFormat;

    const REDIS_NS: &'static str = "llm_reminders";
}

impl Reminders {
    fn add(&mut self, mut reminder: Reminder) -> Result<u64, String> {
        let user_count = self.of_user(reminder.user_id).count();
        if user_count >= MAX_PER_USER {
            return Err(format!(
                "They already have {user_count} reminders, the limit is {MAX_PER_USER}: cancel one first"
            ));
        }
        self.next_id += 1;
        reminder.id = self.next_id;
        self.pending.push(reminder);
        Ok(self.next_id)
    }

    fn of_user(&self, user_id: UserId) -> impl Iterator<Item = &Reminder> {
        self.pending.iter().filter(move |r| r.user_id == user_id)
    }

    /// Only the user's own reminders can be cancelled
    fn cancel(&mut self, user_id: UserId, id: u64) -> bool {
        let before = self.pending.len();
        self.pending
            .retain(|r| !(r.id == id && r.user_id == user_id));
        self.pending.len() < before
    }

    /// Removes and returns the reminders due at `now`
    fn take_due(&mut self, now: u64) -> Vec<Reminder> {
        let (due, pending) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|r| r.due <= now);
        self.pending = pending;
        due
    }

    fn next_due(&self) -> Option<u64> {
        self.pending.iter().map(|r| r.due).min()
    }

    fn owner(&self, id: u64) -> Option<UserId> {
        self.pending.iter().find(|r| r.id == id).map(|r| r.user_id)
    }
}

pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "set_reminder",
                "description": "Remind the person asking of something later: at that time, a reply to their \
                    message pings them with `message`, or a message in `channel` if they want it somewhere else. \
                    Write `message` as what you'll say then, in your voice. Give either `in_minutes` or `at`. Clock times without a timezone are Europe/Paris. Only for the \
                    person asking, never for someone else. Show the time in your answer as given in the result: \
                    Discord shows it in each reader's timezone.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "message": { "type": "string" },
                        "in_minutes": { "type": "integer", "description": "From now" },
                        "at": { "type": "string", "description": "UTC time, YYYY-MM-DD HH:MM" },
                        "channel": {
                            "type": "string",
                            "description": "Only to post it somewhere else: a channel of the server, as its id or \
                                <#id>, or \"dm\" to DM them"
                        }
                    },
                    "required": ["message"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "list_reminders",
                "description": "The pending reminders of the person asking",
                "parameters": { "type": "object", "properties": {} }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "cancel_reminder",
                "description": "Cancel one of the reminders of the person asking, by id (see list_reminders)",
                "parameters": {
                    "type": "object",
                    "properties": { "id": { "type": "integer" } },
                    "required": ["id"]
                }
            }
        }),
    ]
}

#[derive(Debug, Deserialize)]
pub struct SetReminder {
    message: String,
    in_minutes: Option<u64>,
    at: Option<String>,
    channel: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CancelReminder {
    id: u64,
}

impl LlmPlugin {
    pub async fn set_reminder(
        &self,
        rpc: &rpc::ProtocolClient,
        asked: &Message,
        args: SetReminder,
    ) -> String {
        let now = unix_now();
        let due = match (args.in_minutes, args.at.as_deref()) {
            (Some(minutes), _) => now + minutes * 60,
            (None, Some(at)) => match parse_utc(at) {
                Some(due) => due,
                None => return format!("Invalid time {at:?}, expected YYYY-MM-DD HH:MM"),
            },
            (None, None) => return "Give either in_minutes or at".to_string(),
        };
        if due <= now {
            return "That time is in the past".to_string();
        }
        if due > now + MAX_AHEAD_SECS {
            return "At most a year ahead".to_string();
        }
        let text = crate::context::clamp_chars(args.message.trim(), 1000);
        if text.is_empty() {
            return "The message is empty".to_string();
        }

        let wants_dm = args
            .channel
            .as_deref()
            .is_some_and(|channel| channel.trim().eq_ignore_ascii_case("dm"));
        let channel_id = match args.channel.as_deref().map(parse_id) {
            _ if wants_dm => match rpc
                .dm_channel(rpc::context::current(), asked.author.id)
                .await
            {
                Ok(Ok(channel_id)) => channel_id,
                Ok(Err(err)) => return format!("Couldn't open a DM with them: {err}"),
                Err(err) => return format!("Couldn't open a DM with them: {err}"),
            },
            None => asked.channel_id,
            Some(Some(id)) if id == asked.channel_id.get() => asked.channel_id,
            Some(Some(id)) => {
                let channel_id = ChannelId::new(id);
                // ponytail: checked when set, not again when sent
                if let Err(why) = self.check_can_post(rpc, channel_id, asked.author.id).await {
                    return why;
                }
                channel_id
            }
            Some(None) => return "Invalid channel, expected its id, <#id> or \"dm\"".to_string(),
        };
        let asked_in_name = self.channel_name(rpc, asked.channel_id).await;
        let channel_name = if channel_id == asked.channel_id {
            asked_in_name.clone()
        } else {
            self.channel_name(rpc, channel_id).await
        };

        let added = self.reminders.lock().add(Reminder {
            id: 0,
            user_id: asked.author.id,
            channel_id,
            message_id: asked.id,
            elsewhere: channel_id != asked.channel_id,
            due,
            text,
            user_name: asked
                .member
                .as_ref()
                .and_then(|member| member.nick.clone())
                .unwrap_or_else(|| asked.author.display_name().to_string()),
            message_url: asked.link(),
            in_dm: asked.guild_id.is_none() || wants_dm,
            channel_name,
            asked_in: Some(asked.channel_id),
            asked_in_name,
        });
        match added {
            Ok(id) => {
                tracing::info!(user_id = %asked.author.id, id, due, "Reminder set");
                self.reminders_changed().await;
                format!("Set, id {id}, due <t:{due}:f> (<t:{due}:R>) in <#{channel_id}>")
            }
            Err(why) => why,
        }
    }

    /// From the channel contexts, else asked to Discord
    async fn channel_name(&self, rpc: &rpc::ProtocolClient, channel_id: ChannelId) -> String {
        let known = self
            .contexts_by_channel
            .lock()
            .get(&channel_id)
            .map(|context| crate::channel_label(&context.channel));
        if let Some(name) = known {
            return name;
        }
        match rpc.get_channel(rpc::context::current(), channel_id).await {
            Ok(Ok(channel)) => crate::channel_label(&channel),
            _ => String::new(),
        }
    }

    /// Both the asker and the bot must be able to post there
    async fn check_can_post(
        &self,
        rpc: &rpc::ProtocolClient,
        channel_id: ChannelId,
        user_id: UserId,
    ) -> Result<(), String> {
        for (who, id) in [("They", user_id), ("You", self.bot_id)] {
            let can = rpc
                .can_send_in(rpc::context::current(), self.guild_id, channel_id, id)
                .await;
            match can {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) => return Err(format!("{who} can't post in <#{channel_id}>")),
                Ok(Err(err)) => return Err(format!("Couldn't check <#{channel_id}>: {err}")),
                Err(err) => return Err(format!("Couldn't check <#{channel_id}>: {err}")),
            }
        }
        Ok(())
    }

    pub fn list_reminders(&self, user_id: UserId) -> String {
        let reminders = self.reminders.lock();
        let lines = reminders
            .of_user(user_id)
            .map(|r| {
                format!(
                    "id {}: due <t:{}:f> in <#{}>: {}",
                    r.id, r.due, r.channel_id, r.text
                )
            })
            .collect::<Vec<_>>();
        if lines.is_empty() {
            "No pending reminders".to_string()
        } else {
            lines.join("\n")
        }
    }

    pub async fn cancel_reminder(&self, user_id: UserId, args: CancelReminder) -> String {
        if self.cancel_reminder_of(user_id, args.id).await {
            "Cancelled".to_string()
        } else {
            format!("They have no reminder with id {}", args.id)
        }
    }

    async fn cancel_reminder_of(&self, user_id: UserId, id: u64) -> bool {
        let cancelled = self.reminders.lock().cancel(user_id, id);
        if cancelled {
            self.reminders_changed().await;
        }
        cancelled
    }

    /// Saves the reminders and wakes the sending task, which may now sleep less
    async fn reminders_changed(&self) {
        let reminders = self.reminders.lock().clone();
        if let Err(err) = self.storage.clone().set(REMINDERS_KEY, &reminders).await {
            tracing::error!("Failed to save reminders: {err:?}");
        }
        self.reminders_wakeup.notify_one();
    }
}

#[derive(Debug, Serialize)]
pub struct WebReminders {
    reminders: Vec<WebReminder>,
    is_admin: bool,
}

#[derive(Debug, Serialize)]
struct WebReminder {
    id: u64,
    due: u64,
    text: String,
    user_name: String,
    /// Where it will be posted
    channel_name: String,
    /// Where it was asked, when posted somewhere else
    asked_in_name: Option<String>,
    message_url: String,
    mine: bool,
}

/// Your own reminders. The admin also sees everyone's, except those set in DMs
pub async fn web_list(
    State(plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
) -> Json<WebReminders> {
    let is_admin = plugin.admin_id == profile.user_id;
    let channel_names = plugin
        .contexts_by_channel
        .lock()
        .iter()
        .map(|(id, context)| (*id, crate::channel_label(&context.channel)))
        .collect::<HashMap<_, _>>();
    // Reminders set before names were saved
    let name_or = |saved: &str, channel_id: ChannelId| {
        if !saved.is_empty() {
            return saved.to_string();
        }
        channel_names
            .get(&channel_id)
            .cloned()
            .unwrap_or_else(|| "unknown channel".to_string())
    };
    let mut reminders = plugin
        .reminders
        .lock()
        .pending
        .iter()
        .filter(|r| r.user_id == profile.user_id || (is_admin && !r.in_dm))
        .map(|r| WebReminder {
            id: r.id,
            due: r.due,
            text: r.text.clone(),
            user_name: r.user_name.clone(),
            channel_name: name_or(&r.channel_name, r.channel_id),
            asked_in_name: r.elsewhere.then(|| match r.asked_in {
                Some(asked_in) => name_or(&r.asked_in_name, asked_in),
                None => "another channel".to_string(),
            }),
            message_url: r.message_url.clone(),
            mine: r.user_id == profile.user_id,
        })
        .collect::<Vec<_>>();
    reminders.sort_by_key(|r| r.due);
    Json(WebReminders {
        reminders,
        is_admin,
    })
}

#[derive(Debug, Deserialize)]
pub struct WebCancel {
    id: u64,
}

/// Your own reminders, or anyone's for the admin
pub async fn web_cancel(
    State(plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
    Json(WebCancel { id }): Json<WebCancel>,
) -> Response {
    let owner = plugin.reminders.lock().owner(id);
    match owner {
        None => (
            StatusCode::NOT_FOUND,
            "No such reminder, it may have been sent",
        )
            .into_response(),
        Some(owner) if owner != profile.user_id && plugin.admin_id != profile.user_id => (
            StatusCode::FORBIDDEN,
            "You can only cancel your own reminders",
        )
            .into_response(),
        Some(owner) => {
            plugin.cancel_reminder_of(owner, id).await;
            StatusCode::NO_CONTENT.into_response()
        }
    }
}

/// Sends reminders when they're due, forever
pub async fn send_when_due(plugin: Arc<LlmPlugin>, rpc: rpc::ProtocolClient) {
    loop {
        let now = unix_now();
        let due = plugin.reminders.lock().take_due(now);
        if !due.is_empty() {
            for reminder in &due {
                send(&rpc, reminder, now).await;
            }
            plugin.reminders_changed().await;
        }

        // Woken early when one is added. Never more than an hour: the clock can jump
        let next_due = plugin.reminders.lock().next_due();
        let sleep = next_due.map_or(3600, |due| due.saturating_sub(unix_now()).min(3600));
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(sleep)) => {}
            _ = plugin.reminders_wakeup.notified() => {}
        }
    }
}

async fn send(rpc: &rpc::ProtocolClient, reminder: &Reminder, now: u64) {
    let late = if now > reminder.due + LATE_AFTER_SECS {
        format!(" (late: due <t:{}:R>, I was down)", reminder.due)
    } else {
        String::new()
    };
    // Written by the model: no @everyone, @here or role pings, like its answers
    let text = crate::fun::sanitize_llm_output(&format!(
        "<@{}> ⏰ {}{late}",
        reminder.user_id, reminder.text
    ));

    if reminder.elsewhere {
        let text = if reminder.message_url.is_empty() {
            text
        } else {
            format!("{text}\n-# Asked in {}", reminder.message_url)
        };
        let sent = rpc
            .send_message(rpc::context::current(), reminder.channel_id, text)
            .await;
        if matches!(sent, Ok(Ok(_))) {
            return;
        }
        // E.g. their DMs are closed: fall back to replying where they asked
        tracing::warn!(
            id = reminder.id,
            "Failed to send a reminder elsewhere: {sent:?}"
        );
        let Some(asked_in) = reminder.asked_in else {
            return;
        };
        let reminder = Reminder {
            channel_id: asked_in,
            elsewhere: false,
            text: format!(
                "{} (I couldn't post it in {})",
                reminder.text, reminder.channel_name
            ),
            ..reminder.clone()
        };
        return Box::pin(send(rpc, &reminder, now)).await;
    }

    let replied = rpc
        .send_reply(
            rpc::context::current(),
            reminder.channel_id,
            text.clone(),
            reminder.message_id,
        )
        .await;
    if matches!(replied, Ok(Ok(_))) {
        return;
    }
    // The message may be deleted
    let sent = rpc
        .send_message(rpc::context::current(), reminder.channel_id, text)
        .await;
    if !matches!(sent, Ok(Ok(_))) {
        tracing::warn!(
            id = reminder.id,
            "Failed to send a reminder: {sent:?}, dropping it"
        );
    }
}

/// "YYYY-MM-DD HH:MM" (or with a T) in UTC, as unix seconds
fn parse_utc(at: &str) -> Option<u64> {
    let (date, time) = at.trim().split_once([' ', 'T'])?;
    let mut date = date.splitn(3, '-').map(str::parse::<i64>);
    let (Some(Ok(year)), Some(Ok(month)), Some(Ok(day))) = (date.next(), date.next(), date.next())
    else {
        return None;
    };
    let mut time = time
        .trim_end_matches('Z')
        .splitn(3, ':')
        .map(str::parse::<i64>);
    let (Some(Ok(hour)), Some(Ok(minute))) = (time.next(), time.next()) else {
        return None;
    };
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60;
    u64::try_from(secs).ok()
}

/// Like "Saturday 2026-10-31 19:48 UTC", for the model to work out times from
pub fn format_utc(unix: u64) -> String {
    const WEEKDAYS: [&str; 7] = [
        "Thursday",
        "Friday",
        "Saturday",
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
    ];
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{} {year}-{month:02}-{day:02} {:02}:{:02} UTC",
        WEEKDAYS[(days % 7) as usize],
        secs / 3600,
        secs % 3600 / 60
    )
}

/// The inverse of `days_from_civil` (Howard Hinnant's algorithm)
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reminder(user: u64, due: u64) -> Reminder {
        Reminder {
            id: 0,
            user_id: UserId::new(user),
            channel_id: ChannelId::new(1),
            message_id: MessageId::new(1),
            elsewhere: false,
            due,
            text: "x".to_string(),
            user_name: String::new(),
            message_url: String::new(),
            in_dm: false,
            channel_name: String::new(),
            asked_in: None,
            asked_in_name: String::new(),
        }
    }

    #[test]
    fn due_reminders_are_taken_once() {
        let mut reminders = Reminders::default();
        let first = reminders.add(reminder(1, 100)).unwrap();
        reminders.add(reminder(1, 200)).unwrap();
        assert_eq!(reminders.next_due(), Some(100));
        assert_eq!(
            reminders
                .take_due(150)
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>(),
            [first]
        );
        assert!(reminders.take_due(150).is_empty());
        assert_eq!(reminders.next_due(), Some(200));
    }

    #[test]
    fn only_owners_cancel_and_users_are_capped() {
        let mut reminders = Reminders::default();
        let id = reminders.add(reminder(1, 100)).unwrap();
        assert!(!reminders.cancel(UserId::new(2), id));
        assert!(reminders.cancel(UserId::new(1), id));
        for _ in 0..MAX_PER_USER {
            reminders.add(reminder(1, 100)).unwrap();
        }
        assert!(reminders.add(reminder(1, 100)).is_err());
        assert!(reminders.add(reminder(2, 100)).is_ok());
    }

    #[test]
    fn utc_times_round_trip() {
        // 2026-10-31 19:48 UTC
        let at = parse_utc("2026-10-31 19:48").unwrap();
        assert_eq!(at, 1_793_476_080);
        assert_eq!(parse_utc("2026-10-31T19:48:00Z"), Some(at));
        assert_eq!(format_utc(at), "Saturday 2026-10-31 19:48 UTC");
        assert_eq!(format_utc(0), "Thursday 1970-01-01 00:00 UTC");
        assert_eq!(
            format_utc(parse_utc("2028-02-29 00:00").unwrap()),
            "Tuesday 2028-02-29 00:00 UTC"
        );
        assert!(parse_utc("2026-13-01 10:00").is_none() && parse_utc("tomorrow").is_none());
    }
}
