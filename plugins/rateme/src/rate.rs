use rand::{
    distr::StandardUniform,
    prelude::{Distribution, IteratorRandom, Rng},
};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Rate {
    Zero = 0,
    One,
    Two,
    Three,
    Four,
    Five,
    Six,
    Seven,
    Eight,
    Nine,
    Ten,
}

impl Rate {
    pub fn emote(self) -> &'static str {
        match self {
            Rate::Zero => "<:WutFace:816023380363968523>",
            Rate::One => "<:DansGame:556441414678872084>",
            Rate::Two => "<:EleGiggle:373119668401209344>",
            Rate::Three => "<:4Head:373119664907223060>",
            Rate::Four => "<:MingLee:783371880756543498>",
            Rate::Five => "😐",
            Rate::Six => "🙂",
            Rate::Seven => "<:SeemsGood:373119675296382986>",
            Rate::Eight => "👌",
            Rate::Nine => "😏",
            Rate::Ten => "<:PogChamp:236129471076237322>",
        }
    }

    pub fn file_name(&self) -> &'static str {
        match self {
            Rate::Zero => "wutface.png",
            Rate::One => "dansgame.png",
            Rate::Two => "elegiggle.png",
            Rate::Three => "4head.png",
            Rate::Four => "minglee.png",
            Rate::Five => "neutralface.png",
            Rate::Six => "slight_smile.png",
            Rate::Seven => "seemsgood.png",
            Rate::Eight => "ok_hand.png",
            Rate::Nine => "smirk.png",
            Rate::Ten => "pogchamp.png",
        }
    }

    pub fn all() -> impl Iterator<Item = Self> {
        use Rate::*;
        <_>::into_iter([
            Zero, One, Two, Three, Four, Five, Six, Seven, Eight, Nine, Ten,
        ])
    }
}

impl Distribution<Rate> for StandardUniform {
    fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> Rate {
        Rate::all()
            .choose(rng)
            .expect("Should be at least one value")
    }
}
