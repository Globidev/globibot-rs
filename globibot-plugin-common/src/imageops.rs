use std::path::Path;

use image::{AnimationDecoder, GenericImageView, ImageResult, Rgba, RgbaImage};

use rayon::prelude::*;

#[derive(Debug, Clone, Copy)]
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

#[derive(Debug)]
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

pub struct GifBuilder {
    frames: Vec<RgbaImage>,
    dim: Dim<u16>,
}

impl GifBuilder {
    pub fn from_background_frames<Frames, D>(frames: Frames, dim: D) -> Self
    where
        Frames: IntoIterator<Item = RgbaImage>,
        D: Into<Dim<u16>>,
    {
        Self {
            frames: frames.into_iter().collect::<Vec<_>>(),
            dim: dim.into(),
        }
    }

    pub fn overlay<Frame, Pos>(&mut self, frames: &[Frame], positions: &[Pos]) -> &mut Self
    where
        Frame: GenericImageView<Pixel = Rgba<u8>> + Send + Sync,
        Pos: Into<(u32, u32)> + Copy + Send + Sync,
    {
        self.frames
            .par_iter_mut()
            .enumerate()
            .for_each(|(idx, frame)| {
                let overlay_frame = &frames[idx % frames.len()];
                let (x, y) = positions[idx % positions.len()].into();
                image::imageops::overlay(frame, overlay_frame, x, y)
            });

        self
    }

    pub fn finish(self) -> Result<Vec<u8>, gif::EncodingError> {
        let Dim { width, height } = self.dim;

        let gif_frames = self
            .frames
            .into_par_iter()
            .map(|mut bg_frame| {
                let mut frame = gif::Frame::from_rgba_speed(width, height, &mut bg_frame, 10);
                frame.dispose = gif::DisposalMethod::Background;
                frame
            })
            .collect::<Vec<_>>();

        let mut writer = std::io::BufWriter::new(Vec::with_capacity(1 << 20));
        {
            let mut encoder = gif::Encoder::new(&mut writer, width, height, &[])?;
            for frame in gif_frames {
                encoder.write_frame(&frame)?;
            }
            encoder.set_repeat(gif::Repeat::Infinite)?;
        }

        Ok(writer.into_inner().expect("Flushing a Vec cannot fail"))
    }
}
