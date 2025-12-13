use std::{iter::repeat_n, path::Path, time::Instant};

use globibot_plugin_common::{gif, image, imageops::Avatar};
use image::{DynamicImage, GenericImageView, imageops};
use rand::{Rng, prelude::SliceRandom, thread_rng};
use rayon::prelude::*;

pub type Dimension = (u16, u16);

pub mod rate;

pub fn load_rating_images(
    base_path: impl AsRef<Path>,
    (w, h): Dimension,
) -> Result<Vec<image::DynamicImage>, image::ImageError> {
    let paths = rate::Rate::all().map(|rate| {
        let mut path = base_path.as_ref().to_owned();
        path.push(rate.file_name());
        path
    });

    paths
        .map(|file_path| {
            let dyn_img = image::open(file_path)?;
            let scaled_img = dyn_img.thumbnail(w.into(), h.into());
            Ok(scaled_img)
        })
        .collect()
}

pub fn paste_rates_on_avatar(
    avatar: Avatar,
    small_frames: Vec<DynamicImage>,
    final_frame: &DynamicImage,
) -> Result<Vec<u8>, gif::EncodingError> {
    use std::iter::repeat;

    let (w, h): Dimension = (75, 75);
    let w32: u32 = w.into();
    let h32: u32 = h.into();

    let mut rng = thread_rng();

    let frame_samples = {
        let mut samples = repeat(small_frames.iter())
            .flatten()
            .cycle()
            .take(1000)
            .collect::<Vec<_>>();

        samples.shuffle(&mut rng);
        samples.dedup_by_key(|img| *img as *const _);
        samples.truncate(100);
        samples.push(final_frame);

        samples
    };

    let initial_delay = 1;
    let delays = repeat_n(0, 30)
        .chain(repeat([1, 0, 0, 0, 0]).flatten().take(55))
        .chain(repeat_n(1, 10))
        .chain([3, 4, 5, 6, 7])
        .scan(initial_delay, |delay, delta| {
            *delay += delta;
            Some(*delay)
        })
        .chain(std::iter::once(1));

    let bottom_frames = match avatar {
        Avatar::Animated(frames) => frames
            .into_iter()
            .map(|f| imageops::thumbnail(&f, w32, h32))
            .collect::<Vec<_>>(),
        Avatar::Fixed(frame) => vec![imageops::thumbnail(&frame, w32, h32)],
    };

    let mut frames_to_paste = frame_samples
        .into_iter()
        .zip(delays)
        .flat_map(|(frame, delay)| {
            let x = rng.gen_range(0..w32 - frame.width());
            let y = rng.gen_range(0..h32 - frame.height());
            repeat_n((frame, (x, y)), delay)
        })
        .enumerate()
        .map(|(idx, frame)| (&bottom_frames[idx % bottom_frames.len()], frame))
        .collect::<Vec<_>>();

    let (last_bg_frame, (_, (last_x, last_y))) = frames_to_paste.last_mut().unwrap();

    *last_bg_frame = &bottom_frames[0];
    *last_x = w32 - final_frame.width();
    *last_y = h32 - final_frame.height();

    let t0 = Instant::now();
    let frames = frames_to_paste
        .into_par_iter()
        .map(|(bottom_frame, (top_frame, (x, y)))| {
            let mut bg_frame = bottom_frame.clone();

            imageops::overlay(&mut bg_frame, top_frame, x, y);
            let mut frame = gif::Frame::from_rgba_speed(w as _, h as _, &mut bg_frame, 5);
            frame.dispose = gif::DisposalMethod::Background;
            frame.delay = 2; // 50 FPS …
            frame
        })
        .collect::<Vec<_>>();

    tracing::info!("paste frames: {}ms", t0.elapsed().as_millis());

    let t0 = Instant::now();
    let mut writer = std::io::BufWriter::new(Vec::with_capacity(1 << 22));
    {
        let mut encoder = gif::Encoder::new(&mut writer, w, h, &[])?;
        for frame in frames {
            encoder.write_frame(&frame)?;
        }
    }

    tracing::info!("encoded frames: {}ms", t0.elapsed().as_millis());

    Ok(writer.into_inner().expect("Flushing a Vec cannot fail"))
}
