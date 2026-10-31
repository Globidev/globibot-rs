use globibot_core::{
    bot_choices, bot_command, bot_commands, bot_subcommands,
    serenity::all::{Message, UserId},
};

bot_commands! {
    pub enum LlmCommands {
        Llm(Llm),
        AddUserContext(AddUserContext),
        AddToLore(AddToLore),
        Roast(Roast),
        Hype(Hype),
        Tldr(Tldr),
        LoreTrivia(LoreTrivia),
    }
}

bot_command! {
    #[slash(scope = guild, name = "llm", description = "Configure LLM settings")]
    pub enum Llm {
        #[group(name = "model", description = "Get or set the underlying model used")]
        Model(ModelCommand),
        #[group(name = "personality", description = "Get or set the LLM personality")]
        Personality(PersonalityCommand),
    }
}

bot_subcommands! {
    pub enum ModelCommand {
        #[subcommand(name = "show", description = "Display the underlying model used")]
        Show,
        #[subcommand(name = "set", description = "Set the underlying model used")]
        Set {
            #[option(name = "model", description = "The model to set")]
            model: String,
        },
    }
}

bot_subcommands! {
    pub enum PersonalityCommand {
        #[subcommand(name = "show", description = "Display the LLM personality")]
        Show,
        #[subcommand(name = "set", description = "Set the LLM personality")]
        Set {
            #[option(name = "personality", description = "The personality to set")]
            personality: PersonalityChoice,
        },
    }
}

bot_choices! {
    /// Keys of the personality registry (loaded from Redis)
    pub enum PersonalityChoice: String {
        #[choice(name = "Friendly assistant", value = "friendly")]
        Friendly,
        #[choice(name = "French", value = "french")]
        French,
        #[choice(name = "American", value = "american")]
        American,
        #[choice(name = "Zoomer", value = "zoomer")]
        Zoomer,
        #[choice(name = "Scottish", value = "scottish")]
        Scottish,
        #[choice(name = "Aussie", value = "aussie")]
        Aussie,
    }
}

bot_command! {
    #[slash(scope = guild, name = "add-user-context",
            description = "Suggest something to add to someone's lore")]
    pub struct AddUserContext {
        #[option(name = "user", description = "Who the lore is about")]
        user: UserId,
        #[option(name = "context", description = "What to add to their lore", max_length = 500)]
        context: String,
    }
}

bot_command! {
    #[message(scope = guild, name = "Add to lore")]
    pub struct AddToLore {
        message: Message,
    }
}

bot_command! {
    #[slash(scope = guild, name = "roast", description = "Have Globibot playfully roast someone")]
    pub struct Roast {
        #[option(name = "user", description = "Who to roast")]
        user: UserId,
    }
}

bot_command! {
    #[slash(scope = guild, name = "hype", description = "Have Globibot hype someone up")]
    pub struct Hype {
        #[option(name = "user", description = "Who to hype up")]
        user: UserId,
    }
}

bot_command! {
    #[slash(scope = guild, name = "tldr",
            description = "Summarize recent messages in this channel (only you will see it)")]
    pub struct Tldr {
        #[option(name = "messages", description = "How many recent messages to cover (default 50)",
                 min_value = 5, max_value = 200)]
        messages: Option<i64>,
    }
}

bot_command! {
    #[slash(scope = guild, name = "lore-trivia", description = "Guess whose lore this is")]
    pub struct LoreTrivia;
}

