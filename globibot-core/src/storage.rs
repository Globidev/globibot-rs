use redis::{AsyncCommands, RedisWrite, ToRedisArgs, ToSingleRedisArg};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serenity::all::UserId;

#[derive(Debug, Clone)]
pub struct RedisStorage {
    #[expect(unused)] // Not really sure about the usefulness of `Client` yet
    client: redis::Client,

    conn: redis::aio::MultiplexedConnection,
}

impl RedisStorage {
    pub async fn from_env() -> Result<Self, InitError> {
        let redis_url = std::env::var("REDIS_URL")?;
        let client = redis::Client::open(redis_url)?;
        let conn = client.get_multiplexed_async_connection().await?;
        Ok(Self { client, conn })
    }

    pub async fn set<T: StorageValue>(
        &mut self,
        key: impl StorageSubkey,
        value: &T,
    ) -> Result<(), StorageError> {
        let ns_key = StorageKey {
            ns: T::REDIS_NS,
            key,
        };
        let value = JsonStorageValue(value);
        Ok(self.conn.set(ns_key, value).await?)
    }

    pub async fn get<T: StorageValue>(
        &mut self,
        key: impl StorageSubkey,
    ) -> Result<T, StorageError> {
        let ns_key = StorageKey {
            ns: T::REDIS_NS,
            key,
        };
        let raw_value: String = self.conn.get(ns_key).await?;
        let value = serde_json::from_str(&raw_value)?;
        Ok(value)
    }

    pub async fn del<Ns: StorageValue>(
        &mut self,
        key: impl StorageSubkey,
    ) -> Result<(), StorageError> {
        let ns_key = StorageKey {
            ns: Ns::REDIS_NS,
            key,
        };
        Ok(self.conn.del(ns_key).await?)
    }

    pub async fn expire<Ns: StorageValue>(
        &mut self,
        key: impl StorageSubkey,
        seconds: i64,
    ) -> Result<(), StorageError> {
        let ns_key = StorageKey {
            ns: Ns::REDIS_NS,
            key,
        };
        Ok(self.conn.expire(ns_key, seconds).await?)
    }
}

struct JsonStorageValue<T>(T);

impl<T: Serialize> ToSingleRedisArg for JsonStorageValue<T> {}
impl<T: Serialize> ToRedisArgs for JsonStorageValue<T> {
    fn write_redis_args<W>(&self, out: &mut W)
    where
        W: ?Sized + RedisWrite,
    {
        serde_json::to_writer(out.writer_for_next_arg(), &self.0)
            .expect("Failed to serialize to JSON");
    }
}

struct StorageKey<K> {
    ns: &'static str,
    key: K,
}

pub trait StorageSubkey: std::fmt::Display + Send + Sync {}
impl<T: std::fmt::Display + Send + Sync> StorageSubkey for T {}

impl<K: std::fmt::Display> ToSingleRedisArg for StorageKey<K> {}
impl<K: std::fmt::Display> ToRedisArgs for StorageKey<K> {
    fn write_redis_args<W>(&self, out: &mut W)
    where
        W: ?Sized + RedisWrite,
    {
        use std::io::Write;

        write!(out.writer_for_next_arg(), "{}:{}", self.ns, self.key)
            .expect("Failed to write redis args");
    }
}

pub trait StorageValue: Send + Sync + Serialize + DeserializeOwned {
    const REDIS_NS: &'static str;
}

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("Environment variable error: {0}")]
    Env(#[from] std::env::VarError),
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("Deserialization error: {0}")]
    Deserialize(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordSession {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_ts: i64,
}

impl StorageValue for DiscordSession {
    const REDIS_NS: &'static str = "discord_session";
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiscordProfile {
    pub user_id: UserId,
    pub username: String,
    pub avatar_url: Option<String>,
}

impl StorageValue for DiscordProfile {
    const REDIS_NS: &'static str = "discord_profile";
}
