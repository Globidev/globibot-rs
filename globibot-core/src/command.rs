//! Typed application commands.
//!
//! Declare each Discord command once, as Rust types, and get from that single declaration:
//! - its Discord definition (the JSON sent to `upsert_global_command` / `upsert_guild_command`);
//! - a parser from an incoming [`CommandInteraction`] to that type;
//! - matching by command name and type, so plugins don't need to store command IDs.
//!
//! ```ignore
//! match LlmCommands::parse(&interaction) {
//!     Some(Ok(LlmCommands::Llm(Llm::Model(Model::Set { model })))) => self.set_model(model).await,
//!     Some(Ok(LlmCommands::Roast(Roast { user }))) => self.vibe_check(user, Vibe::Roast).await,
//!     Some(Err(err)) => tracing::warn!("Invalid command interaction: {err}"),
//!     None => {} // not one of ours
//! }
//! ```
//!
//! # Principles
//!
//! - Declarative macros only.
//! - Nothing is derived: every name and description Discord sees is written out.
//! - Attributes are Discord JSON: after the keys the macro needs (`scope`, `name`), every
//!   `key = value` pair is copied verbatim into the definition, so any Discord field works
//!   (`description`, `max_length`, `min_value`, `handler`, `nsfw`, ...). Keys must be identifiers.
//! - Only what the Rust types decide is generated: option `type`, `required`, `choices`, nesting
//!   and the command `type`.
//! - Doc comments are allowed (before the `#[...]` attribute of each item, variant or field) and
//!   become rustdoc on the generated items. They are never sent to Discord.
//!
//! # Top-level commands: [`bot_command!`]
//!
//! The attribute name is the command kind:
//!
//! | Attribute | Discord `type` | Body |
//! |---|---|---|
//! | `#[slash(..)]` | 1 CHAT_INPUT | a struct (its fields are options) or an enum (subcommands / groups) |
//! | `#[user(..)]` | 2 USER | a struct with one field implementing [`Target`] (`UserId`, `User`) |
//! | `#[message(..)]` | 3 MESSAGE | a struct with one field implementing [`Target`] (`Message`) |
//! | `#[entry_point(..)]` | 4 PRIMARY_ENTRY_POINT | a unit struct |
//!
//! Inside, `scope` (`guild` or `global`) and `name` come first, in that order, then any Discord
//! fields. `scope` only picks the upsert RPC; it isn't sent to Discord.
//!
//! ```ignore
//! bot_command! {
//!     #[slash(scope = guild, name = "tldr",
//!             description = "Summarize recent messages in this channel (only you will see it)")]
//!     pub struct Tldr {
//!         #[option(name = "messages", description = "How many recent messages to cover",
//!                  min_value = 5, max_value = 200)]
//!         messages: Option<i64>,
//!     }
//! }
//!
//! bot_command! {
//!     #[message(scope = guild, name = "Add to lore")]
//!     pub struct AddToLore {
//!         message: Message,
//!     }
//! }
//!
//! bot_command! {
//!     #[entry_point(scope = global, name = "launch", description = "...", handler = 1)]
//!     pub struct Launch;
//! }
//! ```
//!
//! Generated struct fields are `pub`.
//!
//! # Subcommands and groups
//!
//! A `#[slash]` enum's variants are either:
//! - `#[subcommand(name = "..", ..)]` on a unit variant: a subcommand without options;
//! - `#[subcommand(name = "..", ..)]` on a struct variant: its fields are `#[option]`s;
//! - `#[group(name = "..", ..)]` on a tuple variant wrapping a [`bot_subcommands!`] enum.
//!
//! [`bot_subcommands!`] enums only accept `#[subcommand]` variants, which enforces Discord's
//! maximum depth (command → group → subcommand) at compile time.
//!
//! ```ignore
//! bot_command! {
//!     #[slash(scope = guild, name = "llm", description = "Configure LLM settings")]
//!     pub enum Llm {
//!         #[group(name = "model", description = "Get or set the underlying model used")]
//!         Model(Model),
//!     }
//! }
//!
//! bot_subcommands! {
//!     pub enum Model {
//!         #[subcommand(name = "show", description = "Display the underlying model used")]
//!         Show,
//!         #[subcommand(name = "set", description = "Set the underlying model used")]
//!         Set {
//!             #[option(name = "model", description = "The model to set")]
//!             model: String,
//!         },
//!     }
//! }
//! ```
//!
//! # Choices: [`bot_choices!`]
//!
//! A string or integer option limited to fixed values. The type after `:` is the option's type.
//! Each choice's `value()` returns what Discord sends for it.
//!
//! ```ignore
//! bot_choices! {
//!     pub enum SlapFlavor: i64 {
//!         #[choice(name = "HD static slap", value = 0)]
//!         Static,
//!         #[choice(name = "Animated slap", value = 1)]
//!         Animated,
//!     }
//! }
//! ```
//!
//! # A plugin's command set: [`bot_commands!`]
//!
//! ```ignore
//! bot_commands! {
//!     pub enum LlmCommands {
//!         Llm(Llm),
//!         Tldr(Tldr),
//!         AddToLore(AddToLore),
//!     }
//! }
//!
//! LlmCommands::register(&rpc, Some(guild_id)).await?; // at startup
//! LlmCommands::parse(&interaction)                      // on each interaction
//! ```
//!
//! # Option types
//!
//! A field's type decides its option `type` and `required` through [`OptionValue`]:
//!
//! | Rust field type | Discord option `type` |
//! |---|---|
//! | `String` | 3 STRING |
//! | `i64` | 4 INTEGER |
//! | `bool` | 5 BOOLEAN |
//! | `UserId` | 6 USER |
//! | `ChannelId` | 7 CHANNEL |
//! | `RoleId` | 8 ROLE |
//! | `f64` | 10 NUMBER |
//! | `AttachmentId` | 11 ATTACHMENT |
//! | a [`bot_choices!`] enum | its type's, plus `choices` |
//! | `Option<T>` | `T`'s, with `required: false` |
//!
//! Every non-`Option` field is `required: true`. Options are emitted in field order; Discord
//! rejects required options after optional ones when the plugin registers the command.
//!
//! # Matching
//!
//! An interaction belongs to a command when its `data.name` and `data.kind` equal the command's
//! `NAME` and `KIND`. A [`bot_commands!`] set panics in `definitions()` if two of its commands
//! share a name and kind.
//!
//! # Not covered (yet)
//!
//! - Autocomplete: choices are fixed at compile time, and the bot drops autocomplete interactions.
//! - Responding to interactions (ephemeral replies, deferring, launching Activities).

use serde_json::{Map, Value};
use serenity::all::{
    AttachmentId, ChannelId, CommandData, CommandDataOption, CommandDataOptionValue,
    CommandInteraction, Message, ResolvedTarget, RoleId, User, UserId,
};

/// Which upsert RPC a command is registered with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Guild,
    Global,
}

/// An interaction for one of our commands whose data doesn't match the declaration. Only
/// happens when Discord's registered definition and the code disagree.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ParseError {
    #[error("missing option `{0}`")]
    MissingOption(&'static str),
    #[error("option `{option}` should be {expected}")]
    WrongOptionType {
        option: &'static str,
        expected: &'static str,
    },
    #[error("missing subcommand")]
    MissingSubcommand,
    #[error("unknown subcommand `{0}`")]
    UnknownSubcommand(String),
    #[error("unknown choice `{value}` for option `{option}`")]
    UnknownChoice { option: &'static str, value: String },
    #[error("missing target")]
    MissingTarget,
}

#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    #[error("command `{0}` is guild-scoped but no guild ID was given")]
    MissingGuildId(String),
    #[error(transparent)]
    Rpc(#[from] tarpc::client::RpcError),
    #[error(transparent)]
    Discord(#[from] crate::rpc::DiscordApiError),
}

/// Implemented by every [`bot_command!`] type.
pub trait Command: Sized {
    const NAME: &'static str;
    /// Discord's command `type`: 1 slash, 2 user, 3 message, 4 entry point.
    const KIND: u8;
    const SCOPE: Scope;

    /// The JSON definition to pass to `upsert_*_command`.
    fn definition() -> Value;

    /// `None` if the data is for another command (name or kind differ), `Some(Err(_))` if it's
    /// ours but doesn't match the declaration.
    fn parse_data(data: &CommandData) -> Option<Result<Self, ParseError>>;

    fn parse(interaction: &CommandInteraction) -> Option<Result<Self, ParseError>> {
        Self::parse_data(&interaction.data)
    }
}

/// Implemented by [`bot_subcommands!`] enums and `#[slash]` enums.
pub trait Subcommands: Sized {
    /// Definitions of the subcommands and groups.
    fn options() -> Vec<Value>;
    fn parse_options(options: &[CommandDataOption]) -> Result<Self, ParseError>;
}

/// A type usable as an option field. `parse` gets `None` when the option wasn't sent.
pub trait OptionValue: Sized {
    const TYPE: u8;
    const REQUIRED: bool = true;

    /// Extra definition fields, e.g. `choices`.
    fn extra_fields() -> Map<String, Value> {
        Map::new()
    }

    fn parse(
        option: &'static str,
        value: Option<&CommandDataOptionValue>,
    ) -> Result<Self, ParseError>;
}

macro_rules! impl_option_value {
    ($($ty:ty => $kind:literal, $variant:ident, $expected:literal;)*) => {$(
        impl OptionValue for $ty {
            const TYPE: u8 = $kind;

            fn parse(
                option: &'static str,
                value: Option<&CommandDataOptionValue>,
            ) -> Result<Self, ParseError> {
                match value {
                    Some(CommandDataOptionValue::$variant(value)) => Ok(value.clone()),
                    Some(_) => Err(ParseError::WrongOptionType { option, expected: $expected }),
                    None => Err(ParseError::MissingOption(option)),
                }
            }
        }
    )*};
}

impl_option_value! {
    String => 3, String, "a string";
    i64 => 4, Integer, "an integer";
    bool => 5, Boolean, "a boolean";
    UserId => 6, User, "a user";
    ChannelId => 7, Channel, "a channel";
    RoleId => 8, Role, "a role";
    f64 => 10, Number, "a number";
    AttachmentId => 11, Attachment, "an attachment";
}

impl<T: OptionValue> OptionValue for Option<T> {
    const TYPE: u8 = T::TYPE;
    const REQUIRED: bool = false;

    fn extra_fields() -> Map<String, Value> {
        T::extra_fields()
    }

    fn parse(
        option: &'static str,
        value: Option<&CommandDataOptionValue>,
    ) -> Result<Self, ParseError> {
        value.map(|value| T::parse(option, Some(value))).transpose()
    }
}

/// The right-clicked user or message of a `#[user]` / `#[message]` command.
pub trait Target: Sized {
    fn from_data(data: &CommandData) -> Result<Self, ParseError>;
}

impl Target for UserId {
    fn from_data(data: &CommandData) -> Result<Self, ParseError> {
        data.target_id
            .map(|id| id.to_user_id())
            .ok_or(ParseError::MissingTarget)
    }
}

impl Target for User {
    fn from_data(data: &CommandData) -> Result<Self, ParseError> {
        match data.target() {
            Some(ResolvedTarget::User(user, _)) => Ok(user.clone()),
            _ => Err(ParseError::MissingTarget),
        }
    }
}

impl Target for Message {
    fn from_data(data: &CommandData) -> Result<Self, ParseError> {
        match data.target() {
            Some(ResolvedTarget::Message(message)) => Ok(message.clone()),
            _ => Err(ParseError::MissingTarget),
        }
    }
}

/// Used by the macros' expansions.
#[doc(hidden)]
pub mod __private {
    use super::*;

    pub use serde_json::{Map, Value};
    pub use serenity::all::{CommandData, CommandDataOption, CommandInteraction, GuildId};

    /// Implemented by `#[slash]` structs.
    pub trait OptionFields: Sized {
        fn option_definitions() -> Vec<Value>;
        fn parse_fields(options: &[CommandDataOption]) -> Result<Self, ParseError>;
    }

    pub fn to_json<T: serde::Serialize>(value: T) -> Value {
        serde_json::to_value(value).expect("command attribute values serialize to JSON")
    }

    pub fn object(fields: Vec<(&'static str, Value)>) -> Map<String, Value> {
        fields
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect()
    }

    /// A command, subcommand or group definition.
    pub fn definition(
        kind: u8,
        name: &str,
        options: Vec<Value>,
        extra: Map<String, Value>,
    ) -> Value {
        let mut definition = object(vec![("name", name.into()), ("type", kind.into())]);
        if !options.is_empty() {
            definition.insert("options".to_string(), Value::Array(options));
        }
        definition.extend(extra);
        Value::Object(definition)
    }

    pub fn option_definition<T: OptionValue>(name: &str, extra: Map<String, Value>) -> Value {
        let mut definition = object(vec![
            ("name", name.into()),
            ("type", T::TYPE.into()),
            ("required", T::REQUIRED.into()),
        ]);
        definition.extend(T::extra_fields());
        definition.extend(extra);
        Value::Object(definition)
    }

    pub fn choice<T: serde::Serialize>(name: &str, value: T) -> Value {
        Value::Object(object(vec![("name", name.into()), ("value", to_json(value))]))
    }

    pub fn parse_option<T: OptionValue>(
        options: &[CommandDataOption],
        name: &'static str,
    ) -> Result<T, ParseError> {
        let value = options
            .iter()
            .find(|option| option.name == name)
            .map(|option| &option.value);
        T::parse(name, value)
    }

    pub fn subcommand_options<'a>(
        option: &'a CommandDataOption,
        name: &'static str,
    ) -> Result<&'a [CommandDataOption], ParseError> {
        match &option.value {
            CommandDataOptionValue::SubCommand(options) => Ok(options),
            _ => Err(ParseError::WrongOptionType {
                option: name,
                expected: "a subcommand",
            }),
        }
    }

    pub fn group_options<'a>(
        option: &'a CommandDataOption,
        name: &'static str,
    ) -> Result<&'a [CommandDataOption], ParseError> {
        match &option.value {
            CommandDataOptionValue::SubCommandGroup(options) => Ok(options),
            _ => Err(ParseError::WrongOptionType {
                option: name,
                expected: "a subcommand group",
            }),
        }
    }

    pub fn matches(data: &CommandData, name: &str, kind: u8) -> bool {
        data.name == name && u8::from(data.kind) == kind
    }

    pub fn check_unique(commands: &[(&str, u8)]) {
        for (i, command) in commands.iter().enumerate() {
            assert!(
                !commands[..i].contains(command),
                "two commands are named `{}` with type {}",
                command.0,
                command.1
            );
        }
    }

    pub async fn register(
        rpc: &crate::rpc::ProtocolClient,
        guild_id: Option<GuildId>,
        definitions: Vec<(Scope, Value)>,
    ) -> Result<(), RegisterError> {
        for (scope, definition) in definitions {
            let ctx = crate::rpc::context::current();
            match scope {
                Scope::Global => {
                    rpc.upsert_global_command(ctx, definition).await??;
                }
                Scope::Guild => {
                    let Some(guild_id) = guild_id else {
                        let name = definition["name"].as_str().unwrap_or_default();
                        return Err(RegisterError::MissingGuildId(name.to_string()));
                    };
                    rpc.upsert_guild_command(ctx, guild_id, definition).await??;
                }
            }
        }
        Ok(())
    }
}

/// Declares a top-level command. See the [module docs](crate::command).
#[macro_export]
macro_rules! bot_command {
    // Slash command with options
    (
        $(#[doc = $doc:literal])*
        #[slash(scope = $scope:ident, name = $name:literal $(, $key:ident = $value:expr)* $(,)?)]
        $vis:vis struct $Name:ident $($body:tt)*
    ) => {
        $crate::__bot_command_struct!([$(#[doc = $doc])*] $vis $Name $($body)*);

        impl $crate::command::Command for $Name {
            const NAME: &'static str = $name;
            const KIND: u8 = 1;
            const SCOPE: $crate::command::Scope = $crate::__bot_command_scope!($scope);

            fn definition() -> $crate::command::__private::Value {
                $crate::command::__private::definition(
                    1,
                    $name,
                    <Self as $crate::command::__private::OptionFields>::option_definitions(),
                    $crate::__bot_command_fields!($($key = $value),*),
                )
            }

            fn parse_data(
                data: &$crate::command::__private::CommandData,
            ) -> Option<Result<Self, $crate::command::ParseError>> {
                if !$crate::command::__private::matches(data, $name, 1) {
                    return None;
                }
                Some(<Self as $crate::command::__private::OptionFields>::parse_fields(&data.options))
            }
        }
    };

    // Slash command with subcommands / groups
    (
        $(#[doc = $doc:literal])*
        #[slash(scope = $scope:ident, name = $name:literal $(, $key:ident = $value:expr)* $(,)?)]
        $vis:vis enum $Name:ident { $($body:tt)* }
    ) => {
        $crate::__bot_command_enum!(groups_allowed [$(#[doc = $doc])*] $vis $Name { $($body)* });

        impl $crate::command::Command for $Name {
            const NAME: &'static str = $name;
            const KIND: u8 = 1;
            const SCOPE: $crate::command::Scope = $crate::__bot_command_scope!($scope);

            fn definition() -> $crate::command::__private::Value {
                $crate::command::__private::definition(
                    1,
                    $name,
                    <Self as $crate::command::Subcommands>::options(),
                    $crate::__bot_command_fields!($($key = $value),*),
                )
            }

            fn parse_data(
                data: &$crate::command::__private::CommandData,
            ) -> Option<Result<Self, $crate::command::ParseError>> {
                if !$crate::command::__private::matches(data, $name, 1) {
                    return None;
                }
                Some(<Self as $crate::command::Subcommands>::parse_options(&data.options))
            }
        }
    };

    (
        $(#[doc = $doc:literal])*
        #[user(scope = $scope:ident, name = $name:literal $(, $key:ident = $value:expr)* $(,)?)]
        $vis:vis struct $Name:ident { $field:ident : $ty:ty $(,)? }
    ) => {
        $crate::__bot_command_target!(2 [$(#[doc = $doc])*] $scope $name [$($key = $value),*] $vis $Name $field $ty);
    };

    (
        $(#[doc = $doc:literal])*
        #[message(scope = $scope:ident, name = $name:literal $(, $key:ident = $value:expr)* $(,)?)]
        $vis:vis struct $Name:ident { $field:ident : $ty:ty $(,)? }
    ) => {
        $crate::__bot_command_target!(3 [$(#[doc = $doc])*] $scope $name [$($key = $value),*] $vis $Name $field $ty);
    };

    (
        $(#[doc = $doc:literal])*
        #[entry_point(scope = $scope:ident, name = $name:literal $(, $key:ident = $value:expr)* $(,)?)]
        $vis:vis struct $Name:ident;
    ) => {
        $(#[doc = $doc])*
        #[derive(Debug, Clone)]
        $vis struct $Name;

        impl $crate::command::Command for $Name {
            const NAME: &'static str = $name;
            const KIND: u8 = 4;
            const SCOPE: $crate::command::Scope = $crate::__bot_command_scope!($scope);

            fn definition() -> $crate::command::__private::Value {
                $crate::command::__private::definition(
                    4,
                    $name,
                    Vec::new(),
                    $crate::__bot_command_fields!($($key = $value),*),
                )
            }

            fn parse_data(
                data: &$crate::command::__private::CommandData,
            ) -> Option<Result<Self, $crate::command::ParseError>> {
                $crate::command::__private::matches(data, $name, 4).then_some(Ok(Self))
            }
        }
    };

    ($($tokens:tt)*) => {
        compile_error!(
            "expected `#[slash(scope = .., name = .., ..)]`, `#[user(..)]`, `#[message(..)]` or \
             `#[entry_point(..)]` followed by a struct (or an enum for slash commands)"
        );
    };
}

/// Declares an enum of subcommands, for a `#[group]` variant. See the
/// [module docs](crate::command).
#[macro_export]
macro_rules! bot_subcommands {
    ($(#[doc = $doc:literal])* $vis:vis enum $Name:ident { $($body:tt)* }) => {
        $crate::__bot_command_enum!(groups_denied [$(#[doc = $doc])*] $vis $Name { $($body)* });
    };
}

/// Declares fixed choices for a string or integer option. See the [module docs](crate::command).
#[macro_export]
macro_rules! bot_choices {
    (
        $(#[doc = $doc:literal])*
        $vis:vis enum $Name:ident : $repr:ty {
            $(
                $(#[doc = $variant_doc:literal])*
                #[choice(name = $choice_name:literal, value = $choice_value:literal $(,)?)]
                $Variant:ident
            ),* $(,)?
        }
    ) => {
        $(#[doc = $doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        $vis enum $Name {
            $( $(#[doc = $variant_doc])* $Variant, )*
        }

        #[allow(dead_code)]
        impl $Name {
            /// What Discord sends for this choice.
            #[allow(clippy::useless_conversion)]
            pub fn value(self) -> $repr {
                match self {
                    $( Self::$Variant => $choice_value.into(), )*
                }
            }
        }

        impl $crate::command::OptionValue for $Name {
            const TYPE: u8 = <$repr as $crate::command::OptionValue>::TYPE;

            fn extra_fields() -> $crate::command::__private::Map<String, $crate::command::__private::Value> {
                $crate::command::__private::object(vec![(
                    "choices",
                    $crate::command::__private::Value::Array(vec![
                        $( $crate::command::__private::choice($choice_name, $choice_value), )*
                    ]),
                )])
            }

            fn parse(
                option: &'static str,
                value: Option<&$crate::serenity::all::CommandDataOptionValue>,
            ) -> Result<Self, $crate::command::ParseError> {
                let raw = <$repr as $crate::command::OptionValue>::parse(option, value)?;
                $( if raw == $choice_value { return Ok(Self::$Variant); } )*
                Err($crate::command::ParseError::UnknownChoice { option, value: raw.to_string() })
            }
        }
    };
}

/// Groups a plugin's top-level commands for registration and parsing. See the
/// [module docs](crate::command).
#[macro_export]
macro_rules! bot_commands {
    ($(#[doc = $doc:literal])* $vis:vis enum $Name:ident { $( $Variant:ident($Command:ty) ),* $(,)? }) => {
        $(#[doc = $doc])*
        #[derive(Debug, Clone)]
        $vis enum $Name {
            $( $Variant($Command), )*
        }

        #[allow(dead_code)]
        impl $Name {
            /// Every command's scope and definition. Panics if two commands share a name and
            /// type.
            pub fn definitions() -> Vec<($crate::command::Scope, $crate::command::__private::Value)> {
                $crate::command::__private::check_unique(&[
                    $( (<$Command as $crate::command::Command>::NAME, <$Command as $crate::command::Command>::KIND), )*
                ]);
                vec![
                    $( (<$Command as $crate::command::Command>::SCOPE, <$Command as $crate::command::Command>::definition()), )*
                ]
            }

            /// Upserts every command with its scope's RPC. Guild commands need `guild_id`.
            pub async fn register(
                rpc: &$crate::rpc::ProtocolClient,
                guild_id: Option<$crate::command::__private::GuildId>,
            ) -> Result<(), $crate::command::RegisterError> {
                $crate::command::__private::register(rpc, guild_id, Self::definitions()).await
            }

            /// `None` if the interaction is for none of these commands.
            pub fn parse(
                interaction: &$crate::command::__private::CommandInteraction,
            ) -> Option<Result<Self, $crate::command::ParseError>> {
                Self::parse_data(&interaction.data)
            }

            pub fn parse_data(
                data: &$crate::command::__private::CommandData,
            ) -> Option<Result<Self, $crate::command::ParseError>> {
                $(
                    if let Some(result) = <$Command as $crate::command::Command>::parse_data(data) {
                        return Some(result.map(Self::$Variant));
                    }
                )*
                None
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __bot_command_scope {
    (guild) => {
        $crate::command::Scope::Guild
    };
    (global) => {
        $crate::command::Scope::Global
    };
    ($other:ident) => {
        compile_error!(concat!(
            "unknown scope `",
            stringify!($other),
            "`, expected `guild` or `global`"
        ))
    };
}

/// `key = value` pairs to a JSON map.
#[doc(hidden)]
#[macro_export]
macro_rules! __bot_command_fields {
    ($($key:ident = $value:expr),*) => {
        $crate::command::__private::object(vec![
            $( (stringify!($key), $crate::command::__private::to_json($value)), )*
        ])
    };
}

/// A `#[slash]` struct and its `OptionFields` impl.
#[doc(hidden)]
#[macro_export]
macro_rules! __bot_command_struct {
    ([$($attrs:tt)*] $vis:vis $Name:ident;) => {
        $($attrs)*
        #[derive(Debug, Clone)]
        $vis struct $Name;

        impl $crate::command::__private::OptionFields for $Name {
            fn option_definitions() -> Vec<$crate::command::__private::Value> {
                Vec::new()
            }

            fn parse_fields(
                _options: &[$crate::command::__private::CommandDataOption],
            ) -> Result<Self, $crate::command::ParseError> {
                Ok(Self)
            }
        }
    };

    (
        [$($attrs:tt)*] $vis:vis $Name:ident {
            $(
                $(#[doc = $field_doc:literal])*
                #[option(name = $option_name:literal $(, $key:ident = $value:expr)* $(,)?)]
                $field:ident : $ty:ty
            ),* $(,)?
        }
    ) => {
        $($attrs)*
        #[derive(Debug, Clone)]
        $vis struct $Name {
            $( $(#[doc = $field_doc])* pub $field: $ty, )*
        }

        impl $crate::command::__private::OptionFields for $Name {
            fn option_definitions() -> Vec<$crate::command::__private::Value> {
                vec![
                    $( $crate::command::__private::option_definition::<$ty>(
                        $option_name,
                        $crate::__bot_command_fields!($($key = $value),*),
                    ), )*
                ]
            }

            fn parse_fields(
                options: &[$crate::command::__private::CommandDataOption],
            ) -> Result<Self, $crate::command::ParseError> {
                let _ = options;
                Ok(Self {
                    $( $field: $crate::command::__private::parse_option::<$ty>(options, $option_name)?, )*
                })
            }
        }
    };

    ($($tokens:tt)*) => {
        compile_error!("expected `struct Name;` or `struct Name { #[option(name = .., ..)] field: Type, .. }`");
    };
}

/// A `#[user]` / `#[message]` struct and its `Command` impl.
#[doc(hidden)]
#[macro_export]
macro_rules! __bot_command_target {
    ($kind:literal [$($attrs:tt)*] $scope:ident $name:literal [$($key:ident = $value:expr),*] $vis:vis $Name:ident $field:ident $ty:ty) => {
        $($attrs)*
        #[derive(Debug, Clone)]
        $vis struct $Name {
            pub $field: $ty,
        }

        impl $crate::command::Command for $Name {
            const NAME: &'static str = $name;
            const KIND: u8 = $kind;
            const SCOPE: $crate::command::Scope = $crate::__bot_command_scope!($scope);

            fn definition() -> $crate::command::__private::Value {
                $crate::command::__private::definition(
                    $kind,
                    $name,
                    Vec::new(),
                    $crate::__bot_command_fields!($($key = $value),*),
                )
            }

            fn parse_data(
                data: &$crate::command::__private::CommandData,
            ) -> Option<Result<Self, $crate::command::ParseError>> {
                if !$crate::command::__private::matches(data, $name, $kind) {
                    return None;
                }
                Some(<$ty as $crate::command::Target>::from_data(data).map(|$field| Self { $field }))
            }
        }
    };
}

/// An enum of subcommands (and groups, for `#[slash]` enums) and its `Subcommands` impl.
#[doc(hidden)]
#[macro_export]
macro_rules! __bot_command_enum {
    (
        $groups:ident [$($attrs:tt)*] $vis:vis $Name:ident {
            $(
                $(#[doc = $variant_doc:literal])*
                #[$kind:ident(name = $variant_name:literal $(, $key:ident = $value:expr)* $(,)?)]
                $Variant:ident
                $( {
                    $(
                        $(#[doc = $field_doc:literal])*
                        #[option(name = $option_name:literal $(, $option_key:ident = $option_value:expr)* $(,)?)]
                        $field:ident : $ty:ty
                    ),* $(,)?
                } )?
                $( ( $Inner:ty ) )?
            ),* $(,)?
        }
    ) => {
        $($attrs)*
        #[derive(Debug, Clone)]
        $vis enum $Name {
            $(
                $(#[doc = $variant_doc])*
                $Variant $( { $( $(#[doc = $field_doc])* $field: $ty, )* } )? $( ($Inner) )?,
            )*
        }

        impl $crate::command::Subcommands for $Name {
            fn options() -> Vec<$crate::command::__private::Value> {
                $( $crate::__bot_command_check_variant!($groups $kind); )*
                vec![
                    $(
                        $crate::__bot_command_variant_definition!(
                            $kind $variant_name [$($key = $value),*]
                            $( { $( $option_name: $ty [$($option_key = $option_value),*] ),* } )?
                            $( ($Inner) )?
                        ),
                    )*
                ]
            }

            fn parse_options(
                options: &[$crate::command::__private::CommandDataOption],
            ) -> Result<Self, $crate::command::ParseError> {
                let Some(option) = options.first() else {
                    return Err($crate::command::ParseError::MissingSubcommand);
                };
                $(
                    if option.name == $variant_name {
                        return $crate::__bot_command_variant_parse!(
                            $kind $variant_name $Variant option
                            $( { $( $field: $ty = $option_name ),* } )?
                            $( ($Inner) )?
                        );
                    }
                )*
                Err($crate::command::ParseError::UnknownSubcommand(option.name.clone()))
            }
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __bot_command_check_variant {
    (groups_allowed subcommand) => {};
    (groups_allowed group) => {};
    (groups_denied subcommand) => {};
    (groups_denied group) => {
        compile_error!(
            "`#[group]` isn't allowed in `bot_subcommands!`: Discord allows at most command → group → subcommand"
        )
    };
    ($groups:ident $other:ident) => {
        compile_error!(concat!(
            "unknown variant kind `#[",
            stringify!($other),
            "]`, expected `#[subcommand]` or `#[group]`"
        ))
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __bot_command_variant_definition {
    (subcommand $name:literal [$($key:ident = $value:expr),*]) => {
        $crate::command::__private::definition(
            1,
            $name,
            Vec::new(),
            $crate::__bot_command_fields!($($key = $value),*),
        )
    };
    (subcommand $name:literal [$($key:ident = $value:expr),*] { $( $option_name:literal : $ty:ty [$($option_key:ident = $option_value:expr),*] ),* }) => {
        $crate::command::__private::definition(
            1,
            $name,
            vec![
                $( $crate::command::__private::option_definition::<$ty>(
                    $option_name,
                    $crate::__bot_command_fields!($($option_key = $option_value),*),
                ), )*
            ],
            $crate::__bot_command_fields!($($key = $value),*),
        )
    };
    (group $name:literal [$($key:ident = $value:expr),*] ($Inner:ty)) => {
        $crate::command::__private::definition(
            2,
            $name,
            <$Inner as $crate::command::Subcommands>::options(),
            $crate::__bot_command_fields!($($key = $value),*),
        )
    };
    ($($tokens:tt)*) => {
        compile_error!(
            "`#[subcommand]` variants must be unit or struct variants, `#[group]` variants must wrap one `bot_subcommands!` type"
        )
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __bot_command_variant_parse {
    (subcommand $name:literal $Variant:ident $option:ident) => {
        $crate::command::__private::subcommand_options($option, $name).map(|_| Self::$Variant)
    };
    (subcommand $name:literal $Variant:ident $option:ident { $( $field:ident : $ty:ty = $option_name:literal ),* }) => {
        $crate::command::__private::subcommand_options($option, $name).and_then(|options| {
            let _ = options;
            Ok(Self::$Variant {
                $( $field: $crate::command::__private::parse_option::<$ty>(options, $option_name)?, )*
            })
        })
    };
    (group $name:literal $Variant:ident $option:ident ($Inner:ty)) => {
        $crate::command::__private::group_options($option, $name).and_then(|options| {
            <$Inner as $crate::command::Subcommands>::parse_options(options).map(Self::$Variant)
        })
    };
}

#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use super::*;
    use serde_json::json;

    fn data(value: Value) -> CommandData {
        serde_json::from_value(value).unwrap()
    }

    crate::bot_command! {
        #[slash(scope = guild, name = "add-user-context", description = "Suggest some lore")]
        struct AddUserContext {
            #[option(name = "user", description = "Who the lore is about")]
            user: UserId,
            #[option(name = "context", description = "What to add", max_length = 500)]
            context: String,
            #[option(name = "times", description = "How many times", min_value = 1)]
            times: Option<i64>,
        }
    }

    crate::bot_choices! {
        enum Flavor: i64 {
            #[choice(name = "HD static slap", value = 0)]
            Static,
            #[choice(name = "Animated slap", value = 1)]
            Animated,
        }
    }

    crate::bot_choices! {
        enum Personality: String {
            #[choice(name = "Friendly assistant", value = "friendly")]
            Friendly,
            #[choice(name = "French", value = "french")]
            French,
        }
    }

    crate::bot_command! {
        #[slash(scope = global, name = "slap", description = "Slap someone")]
        struct Slap {
            #[option(name = "target", description = "Who")]
            target: UserId,
            #[option(name = "flavor", description = "How")]
            flavor: Option<Flavor>,
        }
    }

    crate::bot_command! {
        /// Settings, as a command with a group and a plain subcommand
        #[slash(scope = guild, name = "settings", description = "Configure things")]
        enum Settings {
            #[group(name = "personality", description = "Get or set the personality")]
            Personality(PersonalityCommand),
            #[subcommand(name = "reset", description = "Reset everything")]
            Reset,
        }
    }

    crate::bot_subcommands! {
        enum PersonalityCommand {
            #[subcommand(name = "show", description = "Show it")]
            Show,
            #[subcommand(name = "set", description = "Set it")]
            Set {
                #[option(name = "personality", description = "The new one")]
                personality: Personality,
            },
        }
    }

    crate::bot_command! {
        #[slash(scope = guild, name = "lore-trivia", description = "Guess whose lore this is")]
        struct LoreTrivia;
    }

    crate::bot_command! {
        #[user(scope = guild, name = "show art 🎨")]
        struct ShowArt {
            user: UserId,
        }
    }

    crate::bot_command! {
        #[entry_point(scope = global, name = "launch", description = "Play", handler = 1)]
        struct Launch;
    }

    crate::bot_commands! {
        enum TestCommands {
            AddUserContext(AddUserContext),
            Slap(Slap),
            Settings(Settings),
            LoreTrivia(LoreTrivia),
            ShowArt(ShowArt),
            Launch(Launch),
        }
    }

    #[test]
    fn struct_definition() {
        assert_eq!(
            AddUserContext::definition(),
            json!({
                "name": "add-user-context",
                "type": 1,
                "description": "Suggest some lore",
                "options": [
                    { "name": "user", "type": 6, "required": true, "description": "Who the lore is about" },
                    { "name": "context", "type": 3, "required": true, "description": "What to add", "max_length": 500 },
                    { "name": "times", "type": 4, "required": false, "description": "How many times", "min_value": 1 },
                ]
            })
        );
    }

    #[test]
    fn struct_parse() {
        let parsed = AddUserContext::parse_data(&data(json!({
            "id": "1", "name": "add-user-context", "type": 1,
            "options": [
                { "name": "user", "type": 6, "value": "42" },
                { "name": "context", "type": 3, "value": "Likes cats" },
            ]
        })))
        .unwrap()
        .unwrap();

        assert_eq!(parsed.user, UserId::new(42));
        assert_eq!(parsed.context, "Likes cats");
        assert_eq!(parsed.times, None);
    }

    #[test]
    fn missing_required_option() {
        let parsed = AddUserContext::parse_data(&data(json!({
            "id": "1", "name": "add-user-context", "type": 1,
            "options": [{ "name": "user", "type": 6, "value": "42" }]
        })));
        assert_eq!(parsed.unwrap().unwrap_err(), ParseError::MissingOption("context"));
    }

    #[test]
    fn other_commands_dont_match() {
        let other_name = data(json!({ "id": "1", "name": "roast", "type": 1 }));
        assert!(AddUserContext::parse_data(&other_name).is_none());

        let other_kind = data(json!({ "id": "1", "name": "add-user-context", "type": 3 }));
        assert!(AddUserContext::parse_data(&other_kind).is_none());
    }

    #[test]
    fn choices() {
        assert_eq!(
            Slap::definition()["options"][1],
            json!({
                "name": "flavor", "type": 4, "required": false, "description": "How",
                "choices": [
                    { "name": "HD static slap", "value": 0 },
                    { "name": "Animated slap", "value": 1 },
                ]
            })
        );

        let slap = |flavor: i64| {
            Slap::parse_data(&data(json!({
                "id": "1", "name": "slap", "type": 1,
                "options": [
                    { "name": "target", "type": 6, "value": "42" },
                    { "name": "flavor", "type": 4, "value": flavor },
                ]
            })))
            .unwrap()
        };
        assert_eq!(slap(1).unwrap().flavor, Some(Flavor::Animated));
        assert_eq!(
            slap(7).unwrap_err(),
            ParseError::UnknownChoice {
                option: "flavor",
                value: "7".to_string()
            }
        );
        assert_eq!(Personality::French.value(), "french");
    }

    #[test]
    fn subcommands_definition() {
        assert_eq!(
            Settings::definition(),
            json!({
                "name": "settings", "type": 1, "description": "Configure things",
                "options": [
                    {
                        "name": "personality", "type": 2, "description": "Get or set the personality",
                        "options": [
                            { "name": "show", "type": 1, "description": "Show it" },
                            {
                                "name": "set", "type": 1, "description": "Set it",
                                "options": [{
                                    "name": "personality", "type": 3, "required": true, "description": "The new one",
                                    "choices": [
                                        { "name": "Friendly assistant", "value": "friendly" },
                                        { "name": "French", "value": "french" },
                                    ]
                                }]
                            },
                        ]
                    },
                    { "name": "reset", "type": 1, "description": "Reset everything" },
                ]
            })
        );
    }

    #[test]
    fn subcommands_parse() {
        let parsed = Settings::parse_data(&data(json!({
            "id": "1", "name": "settings", "type": 1,
            "options": [{
                "name": "personality", "type": 2,
                "options": [{
                    "name": "set", "type": 1,
                    "options": [{ "name": "personality", "type": 3, "value": "french" }]
                }]
            }]
        })));
        assert!(matches!(
            parsed,
            Some(Ok(Settings::Personality(PersonalityCommand::Set {
                personality: Personality::French
            })))
        ));

        let reset = Settings::parse_data(&data(json!({
            "id": "1", "name": "settings", "type": 1,
            "options": [{ "name": "reset", "type": 1, "options": [] }]
        })));
        assert!(matches!(reset, Some(Ok(Settings::Reset))));

        let unknown = Settings::parse_data(&data(json!({
            "id": "1", "name": "settings", "type": 1,
            "options": [{ "name": "nuke", "type": 1, "options": [] }]
        })));
        assert_eq!(
            unknown.unwrap().unwrap_err(),
            ParseError::UnknownSubcommand("nuke".to_string())
        );
    }

    #[test]
    fn context_menu_and_entry_point() {
        assert_eq!(ShowArt::definition(), json!({ "name": "show art 🎨", "type": 2 }));
        let parsed = ShowArt::parse_data(&data(json!({
            "id": "1", "name": "show art 🎨", "type": 2, "target_id": "42"
        })));
        assert_eq!(parsed.unwrap().unwrap().user, UserId::new(42));

        assert_eq!(
            Launch::definition(),
            json!({ "name": "launch", "type": 4, "description": "Play", "handler": 1 })
        );
    }

    #[test]
    fn command_set() {
        let scopes = TestCommands::definitions()
            .into_iter()
            .map(|(scope, definition)| (definition["name"].as_str().unwrap().to_string(), scope))
            .collect::<Vec<_>>();
        assert_eq!(scopes[1], ("slap".to_string(), Scope::Global));
        assert_eq!(scopes[3], ("lore-trivia".to_string(), Scope::Guild));

        let trivia = TestCommands::parse_data(&data(json!({ "id": "1", "name": "lore-trivia", "type": 1 })));
        assert!(matches!(trivia, Some(Ok(TestCommands::LoreTrivia(LoreTrivia)))));

        let unknown = TestCommands::parse_data(&data(json!({ "id": "1", "name": "roast", "type": 1 })));
        assert!(unknown.is_none());
    }

    #[test]
    #[should_panic(expected = "two commands are named `slap`")]
    fn duplicate_commands_panic() {
        crate::bot_commands! {
            enum Duplicates {
                First(Slap),
                Second(Slap),
            }
        }
        Duplicates::definitions();
    }
}
