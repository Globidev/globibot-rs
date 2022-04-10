use std::path::Path;

use image::{AnimationDecoder, ImageResult, RgbaImage};

pub struct Dim<T> {
    width: T,
    height: T,
}

impl<T, U, V> From<(U, V)> for Dim<T>
where
    U: Into<T>,
    V: Into<T>,
{
    fn from((width, height): (U, V)) -> Self {
        let width = width.into();
        let height = height.into();

        Self { width, height }
    }
}

pub fn load_gif(path: impl AsRef<Path>, dim: impl Into<Dim<u32>>) -> ImageResult<Vec<RgbaImage>> {
    let dim = dim.into();

    let gif_file = std::fs::File::open(path)?;
    let decoder = image::codecs::gif::GifDecoder::new(gif_file)?;

    decoder
        .into_frames()
        .map(|f| {
            Ok(image::imageops::thumbnail(
                f?.buffer(),
                dim.width,
                dim.height,
            ))
        })
        .collect()
}

pub enum Avatar {
    Animated(Vec<image::RgbaImage>),
    Fixed(image::DynamicImage),
}

#[derive(Debug, thiserror::Error)]
pub enum LoadAvatarError {
    #[error("Network error while fetching avatar: {0}")]
    Network(#[from] reqwest::Error),

    #[error("Decoding error while trying to load avatar: {0}")]
    ImageFormat(#[from] image::ImageError),
}

pub async fn load_avatar(url: &str, dim: impl Into<Dim<u32>>) -> Result<Avatar, LoadAvatarError> {
    let Dim { width, height } = dim.into();

    let avatar_data = reqwest::get(url).await?.bytes().await?;

    let avatar = if let Ok(decoder) = image::codecs::gif::GifDecoder::new(&*avatar_data) {
        let frames = decoder
            .into_frames()
            .map(|f| f.map(|f| image::imageops::thumbnail(f.buffer(), width, height)))
            .collect::<Result<_, _>>()?;

        Avatar::Animated(frames)
    } else {
        let image = libwebp_image::webp_load_from_memory(&avatar_data)
            .or_else(|_e| image::load_from_memory(&avatar_data))?
            .thumbnail(width, height);

        Avatar::Fixed(image)
    };

    Ok(avatar)
}
