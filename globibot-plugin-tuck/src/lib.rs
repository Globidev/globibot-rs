use std::path::Path;

use image::{
    imageops, AnimationDecoder, DynamicImage, GenericImageView, ImageResult, Rgba, RgbaImage,
};
use rayon::prelude::*;

pub type Dimension = (u16, u16);

pub fn load_gif(path: impl AsRef<Path>, (width, height): Dimension) -> ImageResult<Vec<RgbaImage>> {
    let gif_file = std::fs::File::open(path)?;
    let decoder = image::codecs::gif::GifDecoder::new(gif_file)?;

    decoder
        .into_frames()
        .map(|f| {
            Ok(imageops::thumbnail(
                f?.buffer(),
                width.into(),
                height.into(),
            ))
        })
        .collect()
}

pub enum Avatar {
    Animated(Vec<RgbaImage>),
    Fixed(DynamicImage),
}

#[derive(Debug, thiserror::Error)]
pub enum LoadAvatarError {
    #[error("Network error while fetching avatar: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Decoding error while trying to load avatar: {0}")]
    ImageFormat(#[from] image::ImageError),
}

pub async fn load_avatar(url: &str, (width, height): Dimension) -> Result<Avatar, LoadAvatarError> {
    let raw_avatar = reqwest::get(url).await?.bytes().await?;

    let img = if let Ok(decoder) = image::codecs::gif::GifDecoder::new(&*raw_avatar) {
        Avatar::Animated(
            decoder
                .into_frames()
                .map(|f| f.map(|f| imageops::thumbnail(f.buffer(), width.into(), height.into())))
                .collect::<Result<_, _>>()?,
        )
    } else {
        Avatar::Fixed(
            libwebp_image::webp_load_from_memory(&raw_avatar)
                .or_else(|_e| image::load_from_memory(&raw_avatar))?
                .thumbnail(width.into(), height.into()),
        )
    };

    Ok(img)
}

pub struct PasteAvatarPositions {
    pub tucked_position: Option<(u32, u32)>,
    pub tucker_position: Option<(u32, u32)>,
}

pub type AvatarPositions = fn(u32) -> PasteAvatarPositions;

pub fn paste_avatar(
    background: (Vec<RgbaImage>, Dimension),
    avatars: (Avatar, Avatar),
    positions: AvatarPositions,
) -> Result<Vec<u8>, gif::EncodingError> {
    pub fn paste_avatar_impl(
        (background_frames, (w, h)): (Vec<RgbaImage>, Dimension),
        tucker_frames: &[impl GenericImageView<Pixel = Rgba<u8>> + Send + Sync],
        tucked_frames: &[impl GenericImageView<Pixel = Rgba<u8>> + Send + Sync],
        positions: AvatarPositions,
    ) -> Result<Vec<u8>, gif::EncodingError> {
        let gif_frames = background_frames
            .into_par_iter()
            .enumerate()
            .map(|(idx, mut bg_frame)| {
                let PasteAvatarPositions {
                    tucker_position,
                    tucked_position,
                } = positions(idx as u32);

                if let Some((x, y)) = tucker_position {
                    let top_frame = &tucker_frames[idx % tucker_frames.len()];
                    imageops::overlay(&mut bg_frame, top_frame, x, y);
                }
                if let Some((x, y)) = tucked_position {
                    let top_frame = &tucked_frames[idx % tucked_frames.len()];
                    imageops::overlay(&mut bg_frame, top_frame, x, y);
                }

                let mut frame = gif::Frame::from_rgba_speed(w, h, &mut bg_frame, 10);
                frame.dispose = gif::DisposalMethod::Background;
                frame
            })
            .collect::<Vec<_>>();

        let mut writer = std::io::BufWriter::new(Vec::with_capacity(1 << 20));
        {
            let mut encoder = gif::Encoder::new(&mut writer, w, h, &[])?;
            for frame in gif_frames {
                encoder.write_frame(&frame)?;
            }
            encoder.set_repeat(gif::Repeat::Infinite)?;
        }

        Ok(writer.into_inner().expect("Flushing a Vec cannot fail"))
    }

    match avatars {
        (Avatar::Animated(tucker_frames), Avatar::Animated(tucked_frames)) => {
            paste_avatar_impl(background, &tucker_frames, &tucked_frames, positions)
        }
        (Avatar::Fixed(tucker_frame), Avatar::Animated(tucked_frames)) => {
            paste_avatar_impl(background, &[tucker_frame], &tucked_frames, positions)
        }
        (Avatar::Animated(tucker_frames), Avatar::Fixed(tucked_frame)) => {
            paste_avatar_impl(background, &tucker_frames, &[tucked_frame], positions)
        }
        (Avatar::Fixed(tucker_frame), Avatar::Fixed(tucked_frame)) => {
            paste_avatar_impl(background, &[tucker_frame], &[tucked_frame], positions)
        }
    }
}
