//! Answering command interactions.
//!
//! [`CommandCtx`] bundles a command interaction with the RPC client, so handlers take one
//! argument and replies are one call:
//!
//! ```ignore
//! use globibot_core::interaction::CommandInteractionExt;
//!
//! let ctx = interaction.ctx(&rpc);
//! ctx.reply(format!("{} walks angrily towards {}", author.mention(), target.mention()))
//!     .await?;
//! ```
//!
//! Every interaction needs exactly one first response, within 3 seconds: [`reply`], [`defer`] or
//! [`launch_activity`] (or [`respond`] for anything else). After that, [`edit`] changes it.
//! Anything slower than 3 seconds defers first, then edits:
//!
//! ```ignore
//! ctx.defer_ephemeral().await?;
//! let answer = slow_work().await?;
//! ctx.edit(answer).await?;
//! ```
//!
//! [`reply`] and [`edit`] keep Discord's default of pinging everyone they mention. For other
//! mention rules, embeds and the like, build the response with serenity's builders and use
//! [`respond`] / [`edit_with`]:
//!
//! ```ignore
//! use globibot_core::serenity::all::{
//!     CreateAllowedMentions, CreateInteractionResponse, CreateInteractionResponseMessage,
//! };
//!
//! // Don't ping anyone
//! ctx.respond(CreateInteractionResponse::Message(
//!     CreateInteractionResponseMessage::new()
//!         .content(clue)
//!         .allowed_mentions(CreateAllowedMentions::new()),
//! ))
//! .await?;
//! ```
//!
//! Attachments set on the builders are not sent: the RPC carries JSON only.
//!
//! [`reply`]: CommandCtx::reply
//! [`defer`]: CommandCtx::defer
//! [`launch_activity`]: CommandCtx::launch_activity
//! [`respond`]: CommandCtx::respond
//! [`edit`]: CommandCtx::edit
//! [`edit_with`]: CommandCtx::edit_with

use serde::Serialize;
use serde_json::Value;
use serenity::all::{
    CommandInteraction, CreateInteractionResponse, CreateInteractionResponseMessage,
    EditInteractionResponse, Message,
};

use crate::rpc::{CallError, ProtocolClient, context};

/// Interaction response type that opens the application's Activity. Serenity 0.12 has no
/// builder for it.
const LAUNCH_ACTIVITY: u8 = 12;

/// A command interaction and the RPC client to answer it with. See the [module docs](self).
#[derive(Clone, Copy)]
pub struct CommandCtx<'a> {
    pub rpc: &'a ProtocolClient,
    pub interaction: &'a CommandInteraction,
}

pub trait CommandInteractionExt {
    fn ctx<'a>(&'a self, rpc: &'a ProtocolClient) -> CommandCtx<'a>;
}

impl CommandInteractionExt for CommandInteraction {
    fn ctx<'a>(&'a self, rpc: &'a ProtocolClient) -> CommandCtx<'a> {
        CommandCtx {
            rpc,
            interaction: self,
        }
    }
}

impl CommandCtx<'_> {
    /// Replies with a message everyone in the channel sees.
    pub async fn reply(&self, content: impl Into<String>) -> Result<(), CallError> {
        self.respond(message(content, false)).await
    }

    /// Replies with a message only the user who ran the command sees.
    pub async fn reply_ephemeral(&self, content: impl Into<String>) -> Result<(), CallError> {
        self.respond(message(content, true)).await
    }

    /// Shows "Globibot is thinking…" until [`edit`](Self::edit) is called.
    pub async fn defer(&self) -> Result<(), CallError> {
        self.respond(defer(false)).await
    }

    /// Like [`defer`](Self::defer), and the eventual answer is only visible to the user.
    pub async fn defer_ephemeral(&self) -> Result<(), CallError> {
        self.respond(defer(true)).await
    }

    /// Opens the application's Activity for the user.
    pub async fn launch_activity(&self) -> Result<(), CallError> {
        self.respond_raw(serde_json::json!({ "type": LAUNCH_ACTIVITY }))
            .await
    }

    /// Sends any first response built with serenity's builders.
    pub async fn respond(&self, response: CreateInteractionResponse) -> Result<(), CallError> {
        self.respond_raw(to_json(response)).await
    }

    /// Replaces the content of the reply, or answers a deferred interaction.
    pub async fn edit(&self, content: impl Into<String>) -> Result<Message, CallError> {
        self.edit_with(EditInteractionResponse::new().content(content))
            .await
    }

    /// Edits the reply with any change built with serenity's builder.
    pub async fn edit_with(&self, edit: EditInteractionResponse) -> Result<Message, CallError> {
        let token = self.interaction.token.clone();
        Ok(self
            .rpc
            .edit_interaction_response(context::current(), token, to_json(edit))
            .await??)
    }

    async fn respond_raw(&self, response: Value) -> Result<(), CallError> {
        let (id, token) = (self.interaction.id, self.interaction.token.clone());
        self.rpc
            .create_interaction_response(context::current(), id, token, response)
            .await??;
        Ok(())
    }
}

fn message(content: impl Into<String>, ephemeral: bool) -> CreateInteractionResponse {
    CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(content)
            .ephemeral(ephemeral),
    )
}

fn defer(ephemeral: bool) -> CreateInteractionResponse {
    CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(ephemeral))
}

fn to_json(value: impl Serialize) -> Value {
    serde_json::to_value(value).expect("serenity builders serialize to JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Serenity always adds `attachments` and `flags`, as in the requests it sends itself
    #[test]
    fn reply_payloads() {
        assert_eq!(
            to_json(message("hi", false)),
            json!({ "type": 4, "data": { "content": "hi", "flags": 0, "attachments": [] } })
        );
        assert_eq!(
            to_json(message("hi", true)),
            json!({ "type": 4, "data": { "content": "hi", "flags": 64, "attachments": [] } })
        );
    }

    #[test]
    fn defer_payloads() {
        assert_eq!(
            to_json(defer(false)),
            json!({ "type": 5, "data": { "flags": 0, "attachments": [] } })
        );
        assert_eq!(
            to_json(defer(true)),
            json!({ "type": 5, "data": { "flags": 64, "attachments": [] } })
        );
    }

    #[test]
    fn edit_payload() {
        assert_eq!(
            to_json(EditInteractionResponse::new().content("hi")),
            json!({ "content": "hi" })
        );
    }
}
