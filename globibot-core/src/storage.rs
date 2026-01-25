use std::fmt::Display;

use redis::{AsyncCommands, FromRedisValue, RedisWrite, ToRedisArgs, ToSingleRedisArg};
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
        let value = T::Format::to_single_redis_arg(value);
        self.conn.set(ns_key, value).await
    }

    pub async fn get<T: StorageValue>(
        &mut self,
        key: impl StorageSubkey,
    ) -> Result<T, StorageError> {
        let ns_key = StorageKey {
            ns: T::REDIS_NS,
            key,
        };
        let extracted = self.conn.get(ns_key).await?;
        Ok(T::Format::from_extracted(extracted))
    }

    pub async fn list_keys<Ns: StorageValue>(
        &mut self,
        pattern: &str,
    ) -> Result<Vec<String>, StorageError> {
        let ns_key = StorageKey {
            ns: Ns::REDIS_NS,
            key: pattern,
        };
        let keys: Vec<String> = self.conn.keys(ns_key).await?;
        let prefix = format!("{}:", Ns::REDIS_NS);
        let stripped_keys = keys
            .into_iter()
            .map(|full_key| {
                full_key
                    .strip_prefix(&prefix)
                    .unwrap_or(&full_key)
                    .to_string()
            })
            .collect();
        Ok(stripped_keys)
    }

    pub async fn del<Ns: StorageValue>(
        &mut self,
        key: impl StorageSubkey,
    ) -> Result<(), StorageError> {
        let ns_key = StorageKey {
            ns: Ns::REDIS_NS,
            key,
        };
        self.conn.del(ns_key).await
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
        self.conn.expire(ns_key, seconds).await
    }
}

struct StorageKey<K> {
    ns: &'static str,
    key: K,
}

pub trait StorageSubkey: Display + Send + Sync {}
impl<T: Display + Send + Sync> StorageSubkey for T {}

impl<K: Display> ToSingleRedisArg for StorageKey<K> {}
impl<K: Display> ToRedisArgs for StorageKey<K> {
    fn write_redis_args<W>(&self, out: &mut W)
    where
        W: ?Sized + RedisWrite,
    {
        use std::io::Write;

        let Self { ns, key } = self;
        write!(out.writer_for_next_arg(), "{ns}:{key}").expect("Failed to write redis args");
    }
}

pub trait StorageValue: Sized + Send + Sync {
    type Format: StorageValueFormat<Self>;

    const REDIS_NS: &'static str;
}

pub trait StorageValueFormat<T> {
    type Extract: FromRedisValue;

    fn from_extracted(extracted: Self::Extract) -> T;
    fn to_single_redis_arg(value: &T) -> impl ToSingleRedisArg + Send + Sync;
}

pub enum JsonFormat {}

impl<T: Serialize + DeserializeOwned + Send + Sync> StorageValueFormat<T> for JsonFormat {
    type Extract = ExtractJsonValue<T>;

    fn from_extracted(ExtractJsonValue(inner): Self::Extract) -> T {
        inner
    }

    fn to_single_redis_arg(value: &T) -> impl ToSingleRedisArg + Send + Sync {
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

        JsonStorageValue(value)
    }
}

pub enum StringFormat {}
impl<T: AsRef<str> + From<String>> StorageValueFormat<T> for StringFormat {
    type Extract = ExtractStringValue<T>;

    fn from_extracted(ExtractStringValue(inner): Self::Extract) -> T {
        inner
    }

    fn to_single_redis_arg(value: &T) -> impl ToSingleRedisArg + Send + Sync {
        value.as_ref()
    }
}

pub struct ExtractJsonValue<T>(T);

impl<T: DeserializeOwned> FromRedisValue for ExtractJsonValue<T> {
    fn from_redis_value(v: redis::Value) -> Result<Self, redis::ParsingError> {
        let extracted: String = FromRedisValue::from_redis_value(v)?;
        let value = serde_json::from_str(&extracted).map_err(|err| err.to_string())?;
        Ok(Self(value))
    }
}

pub struct ExtractStringValue<T>(T);

impl<T: From<String>> FromRedisValue for ExtractStringValue<T> {
    fn from_redis_value(v: redis::Value) -> Result<Self, redis::ParsingError> {
        let extracted: String = FromRedisValue::from_redis_value(v)?;
        Ok(Self(extracted.into()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("Environment variable error: {0}")]
    Env(#[from] std::env::VarError),
}

pub type StorageError = redis::RedisError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordSession {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at_ts: i64,
}

impl StorageValue for DiscordSession {
    type Format = JsonFormat;

    const REDIS_NS: &'static str = "discord_session";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordProfile {
    pub user_id: UserId,
    pub username: String,
    pub avatar_url: Option<String>,
}

impl StorageValue for DiscordProfile {
    type Format = JsonFormat;

    const REDIS_NS: &'static str = "discord_profile";
}
