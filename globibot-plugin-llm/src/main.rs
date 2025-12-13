mod openrouter;
mod personality;

use openrouter::{ContentPart, ImageContentPart, Message as LlmMessage, Role, TextContentPart};
use parking_lot::Mutex;

use std::collections::{HashMap, VecDeque};

use globibot_core::{
    events::{Event, EventType},
    plugin::{Endpoints, HandleEvents, HasEvents, HasRpc, Plugin},
    rpc,
    serenity::all::{
        ChannelId, CommandDataOptionValue, CommandId, CommandInteraction, Message, UserId,
    },
    transport::Tcp,
};
use itertools::Itertools;

use crate::personality::Personality;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let subscriber_addr = std::env::var("SUBSCRIBER_ADDR")?;
    let rpc_addr = std::env::var("RPC_ADDR")?;

    let guild_id = std::env::var("LLM_INSTALL_COMMAND_GUILD_ID")?.parse()?;
    let desired_command: serde_json::Value =
        serde_json::from_str(include_str!("../llm-slash-command.json"))?;

    let events = [EventType::MessageCreate, EventType::InteractionCreate];
    let endpoints = Endpoints::new()
        .rpc(Tcp::new(rpc_addr))
        .events(Tcp::new(subscriber_addr), events);

    let plugin = LlmPlugin::connect_init(endpoints, async |rpc| {
        let command = rpc
            .upsert_guild_command(rpc::context::current(), guild_id, desired_command)
            .await??;

        LlmPlugin::from_env(command.id)
    })
    .await?;

    plugin.handle_events().await?;

    Ok(())
}

struct LlmPlugin {
    bot_id: UserId,
    admin_id: UserId,
    llm_client: Mutex<openrouter::Client>,

    contexts_by_channel: Mutex<HashMap<ChannelId, VecDeque<openrouter::Message>>>,
    command_id: CommandId,
}

impl LlmPlugin {
    fn from_env(command_id: CommandId) -> anyhow::Result<Self> {
        let bot_id = std::env::var("DISCORD_BOT_ID")?.parse()?;
        let admin_id = std::env::var("LLM_ADMIN_USER_ID")?.parse()?;
        let llm_client = Mutex::new(openrouter::Client::from_env()?);

        Ok(LlmPlugin {
            bot_id,
            admin_id,
            llm_client,
            contexts_by_channel: <_>::default(),
            command_id,
        })
    }

    async fn answer_message(
        &self,
        rpc: rpc::ProtocolClient,
        message: &Message,
        user_llm_message: LlmMessage,
    ) -> anyhow::Result<()> {
        let ctx = rpc::context::current();

        let mut parts = self.context_for_channel(message.channel_id);
        parts.push(user_llm_message.clone());

        let typing = rpc.start_typing(ctx, message.channel_id).await??;
        let completion = self.llm_client.lock().complete(parts);
        let completion_res = completion.await;
        rpc.stop_typing(ctx, typing).await??;
        self.register_message(message, user_llm_message);
        if let Ok(answer) = completion_res {
            rpc.send_reply(ctx, message.channel_id, answer.clone(), message.id)
                .await??;

            let bot_llm_message = LlmMessage {
                role: Role::Assistant,
                content: vec![ContentPart::Text(TextContentPart {
                    kind: "text",
                    text: answer,
                })],
            };
            self.register_message(message, bot_llm_message);
        } else {
            tracing::error!("Failed to get LLM completion: {:?}", completion_res.err());
            rpc.send_reply(
                ctx,
                message.channel_id,
                "Sorry, I lost my train of thought.".to_string(),
                message.id,
            )
            .await??;
        }

        Ok(())
    }

    fn register_message(&self, message: &Message, llm_message: LlmMessage) {
        let mut contexts_by_channel = self.contexts_by_channel.lock();
        let context = contexts_by_channel.entry(message.channel_id).or_default();

        context.push_back(llm_message);
        if context.len() > CONTEXT_WINDOW_SIZE {
            context.pop_front();
        }
    }

    fn context_for_channel(&self, chan_id: ChannelId) -> Vec<openrouter::Message> {
        let contexts_by_channel = self.contexts_by_channel.lock();
        let context = contexts_by_channel.get(&chan_id);

        if let Some(context) = context {
            context.iter().cloned().collect_vec()
        } else {
            vec![]
        }
    }

    async fn show_model(
        &self,
        rpc: rpc::ProtocolClient,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<()> {
        rpc.create_interaction_response(
            rpc::context::current(),
            interaction.id,
            interaction.token.clone(),
            serde_json::json!({
                "type": 4,
                "data": {
                    "content": format!(
                        "Current model is set to `{}`",
                        self.llm_client.lock().model
                    )
                }
            }),
        )
        .await??;
        Ok(())
    }

    async fn set_model(
        &self,
        rpc: rpc::ProtocolClient,
        interaction: &CommandInteraction,
        value: &CommandDataOptionValue,
    ) -> anyhow::Result<()> {
        if interaction.user.id != self.admin_id {
            rpc.create_interaction_response(
                rpc::context::current(),
                interaction.id,
                interaction.token.clone(),
                serde_json::json!({
                    "type": 4,
                    "data": {
                        "content": format!("You do not have permission to change the model. ask <@{}>", self.admin_id)
                    }
                }),
            )
            .await??;
            return Ok(());
        }

        if let CommandDataOptionValue::SubCommand(opts) = value
            && let Some(opt) = opts.first()
            && opt.name == "model"
            && let Some(new_model) = opt.value.as_str()
        {
            self.llm_client.lock().model = new_model.trim().to_string();
            rpc.create_interaction_response(
                rpc::context::current(),
                interaction.id,
                interaction.token.clone(),
                serde_json::json!({
                    "type": 4,
                    "data": {
                        "content": format!("Model changed to `{new_model}`")
                    }
                }),
            )
            .await??;
        }

        Ok(())
    }

    async fn show_personality(
        &self,
        rpc: rpc::ProtocolClient,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<()> {
        rpc.create_interaction_response(
            rpc::context::current(),
            interaction.id,
            interaction.token.clone(),
            serde_json::json!({
                "type": 4,
                "data": {
                    "content": format!(
                        "Current personality is set to `{}`",
                        self.llm_client.lock().personality
                    )
                }
            }),
        )
        .await??;
        Ok(())
    }

    async fn set_personality(
        &self,
        rpc: rpc::ProtocolClient,
        interaction: &CommandInteraction,
        value: &CommandDataOptionValue,
    ) -> anyhow::Result<()> {
        if let CommandDataOptionValue::SubCommand(opts) = value
            && let Some(opt) = opts.first()
            && opt.name == "personality"
            && let Some(new_personality) = opt.value.as_str()
        {
            let Ok(new_personality) = Personality::try_from(new_personality) else {
                rpc.create_interaction_response(
                    rpc::context::current(),
                    interaction.id,
                    interaction.token.clone(),
                    serde_json::json!({
                        "type": 4,
                        "data": {
                            "content": format!("Unknown personality `{new_personality}`")
                        }
                    }),
                )
                .await??;
                return Ok(());
            };

            self.llm_client.lock().personality = new_personality;
            self.contexts_by_channel
                .lock()
                .remove(&interaction.channel_id);

            rpc.create_interaction_response(
                rpc::context::current(),
                interaction.id,
                interaction.token.clone(),
                serde_json::json!({
                    "type": 4,
                    "data": {
                        "content": format!("Personality changed to `{new_personality}` (+ memory wiped)")
                    }
                }),
            )
            .await??;
        }

        Ok(())
    }
}

const CONTEXT_WINDOW_SIZE: usize = 200;

impl Plugin for LlmPlugin {
    const ID: &'static str = "LLM";

    type RpcPolicy = HasRpc<true>;
    type EventsPolicy = HasEvents<true>;
}

impl HandleEvents for LlmPlugin {
    type Err = anyhow::Error;

    async fn on_event(&self, rpc: rpc::ProtocolClient, event: Event) -> Result<(), Self::Err> {
        match event {
            Event::InteractionCreate { interaction } if interaction.data.id == self.command_id => {
                // dbg!(&interaction);
                use CommandDataOptionValue::*;
                let Some(sub_cmd) = interaction.data.options.first() else {
                    return Ok(());
                };

                match (sub_cmd.name.as_str(), &sub_cmd.value) {
                    ("model", SubCommandGroup(opts)) => match opts.first() {
                        Some(opt) if opt.name == "show" => {
                            self.show_model(rpc, &interaction).await?
                        }
                        Some(opt) if opt.name == "set" => {
                            self.set_model(rpc, &interaction, &opt.value).await?
                        }
                        _ => {}
                    },
                    ("personality", SubCommandGroup(opts)) => match opts.first() {
                        Some(opt) if opt.name == "show" => {
                            self.show_personality(rpc, &interaction).await?
                        }
                        Some(opt) if opt.name == "set" => {
                            self.set_personality(rpc, &interaction, &opt.value).await?
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }

            Event::MessageCreate { message } if !message.author.bot => {
                let user_name = &message.author.name;
                let user_id = message.author.id.get();

                let content_safe = rpc
                    .content_safe(
                        rpc::context::current(),
                        message.content.clone(),
                        message.guild_id,
                    )
                    .await??
                    .replace("@rust-bot", "@globibot");

                let user_llm_message = {
                    let mut content = vec![ContentPart::Text(TextContentPart {
                        kind: "text",
                        text: format!("{user_name} (<@{user_id}>): {content_safe}"),
                    })];

                    if false {
                        content.extend(message.attachments.iter().filter_map(|att| {
                            let _dims = att.dimensions()?;
                            Some(ContentPart::Image(ImageContentPart {
                                kind: "image_url",
                                image_url: openrouter::ImageUrl {
                                    url: att.url.clone(),
                                },
                            }))
                        }));
                    }

                    LlmMessage {
                        role: Role::User,
                        content,
                    }
                };

                if message.mentions_user_id(self.bot_id) {
                    self.answer_message(rpc, &message, user_llm_message).await?;
                } else {
                    self.register_message(&message, user_llm_message);
                }
            }

            _ => {}
        }

        Ok(())
    }
}
