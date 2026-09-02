//! Cover art for egui's image pipeline: `egui::Image::new("https://...")`
//! asks this loader for pixels. Downloading, decoding, and scaling all
//! happen on a few worker threads; the interface thread only ever receives a
//! finished `ColorImage`, so a slow cover can never hold up a frame.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use egui::ColorImage;
use egui::load::{ImageLoadResult, ImageLoader, ImagePoll, LoadError, SizeHint};

const WORKERS: usize = 4;
const MAX_BYTES: u64 = 8 * 1024 * 1024;
/// Covers are shown at most a couple of hundred points wide; anything larger
/// is wasted texture memory and upload time.
const MAX_DIMENSION: u32 = 630;

enum Entry {
    Pending,
    Ready(Arc<ColorImage>),
    Failed(String),
}

struct Job {
    url: String,
    ctx: egui::Context,
}

struct Inner {
    entries: Mutex<HashMap<String, Entry>>,
    queue: Mutex<mpsc::Sender<Job>>,
    cache_dir: PathBuf,
}

#[derive(Clone)]
pub struct CoverLoader {
    inner: Arc<Inner>,
}

impl CoverLoader {
    pub const ID: &'static str = "zitch::CoverLoader";

    pub fn new(cache_dir: PathBuf) -> Self {
        if let Err(error) = std::fs::create_dir_all(&cache_dir) {
            log::warn!("no cover cache at {}: {error}", cache_dir.display());
        }
        let (tx, rx) = mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        let loader = Self {
            inner: Arc::new(Inner {
                entries: Mutex::default(),
                queue: Mutex::new(tx),
                cache_dir,
            }),
        };
        for n in 0..WORKERS {
            let rx = Arc::clone(&rx);
            let inner = Arc::clone(&loader.inner);
            std::thread::Builder::new()
                .name(format!("cover-{n}"))
                .spawn(move || {
                    loop {
                        let job = rx.lock().unwrap_or_else(|p| p.into_inner()).recv();
                        let Ok(job) = job else { break };
                        inner.complete(job);
                    }
                })
                .expect("spawning cover worker");
        }
        loader
    }

    pub fn install(&self, ctx: &egui::Context) {
        ctx.add_image_loader(Arc::new(self.clone()));
    }
}

impl Inner {
    fn complete(&self, job: Job) {
        let outcome = self.fetch(&job.url).and_then(|bytes| decode(&bytes));
        let entry = match outcome {
            Ok(image) => Entry::Ready(Arc::new(image)),
            Err(error) => {
                log::debug!("cover {}: {error}", job.url);
                Entry::Failed(error)
            }
        };
        self.entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(job.url, entry);
        job.ctx.request_repaint();
    }

    fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        let path = self.cache_dir.join(cache_name(url));
        if let Ok(bytes) = std::fs::read(&path) {
            return Ok(bytes);
        }
        let response = ureq::get(url).call().map_err(|e| e.to_string())?;
        let bytes = response
            .into_body()
            .with_config()
            .limit(MAX_BYTES)
            .read_to_vec()
            .map_err(|e| e.to_string())?;
        // Write beside, then rename, so a reader never sees a partial file.
        let tmp = path.with_extension("part");
        if std::fs::write(&tmp, &bytes)
            .and_then(|()| std::fs::rename(&tmp, &path))
            .is_err()
        {
            let _ = std::fs::remove_file(&tmp);
        }
        Ok(bytes)
    }
}

/// The first frame, scaled down to at most `MAX_DIMENSION`, as egui pixels.
fn decode(bytes: &[u8]) -> Result<ColorImage, String> {
    use image::AnimationDecoder;
    let format = image::guess_format(bytes).map_err(|e| e.to_string())?;
    let decoded = if format == image::ImageFormat::Gif {
        // Animated covers can run to megabytes and hundreds of frames; one
        // frame is all a tile needs.
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes))
            .map_err(|e| e.to_string())?;
        let frame = decoder
            .into_frames()
            .next()
            .ok_or("gif has no frames")?
            .map_err(|e| e.to_string())?;
        image::DynamicImage::ImageRgba8(frame.into_buffer())
    } else {
        image::load_from_memory_with_format(bytes, format).map_err(|e| e.to_string())?
    };
    let decoded = if decoded.width() > MAX_DIMENSION || decoded.height() > MAX_DIMENSION {
        decoded.resize(
            MAX_DIMENSION,
            MAX_DIMENSION,
            image::imageops::FilterType::Triangle,
        )
    } else {
        decoded
    };
    let rgba = decoded.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
}

/// A filename for the cache that is unique to the URL and safe everywhere.
fn cache_name(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    let ext = url
        .rsplit('.')
        .next()
        .filter(|ext| ext.len() <= 4 && ext.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("img");
    format!("{:016x}.{ext}", hasher.finish())
}

impl ImageLoader for CoverLoader {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn load(&self, ctx: &egui::Context, uri: &str, _size_hint: SizeHint) -> ImageLoadResult {
        if !(uri.starts_with("https://") || uri.starts_with("http://")) {
            return Err(LoadError::NotSupported);
        }
        let mut entries = self.inner.entries.lock().unwrap_or_else(|p| p.into_inner());
        match entries.get(uri) {
            Some(Entry::Ready(image)) => Ok(ImagePoll::Ready {
                image: Arc::clone(image),
            }),
            Some(Entry::Pending) => Ok(ImagePoll::Pending { size: None }),
            Some(Entry::Failed(error)) => Err(LoadError::Loading(error.clone())),
            None => {
                entries.insert(uri.to_string(), Entry::Pending);
                drop(entries);
                let job = Job {
                    url: uri.to_string(),
                    ctx: ctx.clone(),
                };
                let _ = self
                    .inner
                    .queue
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .send(job);
                Ok(ImagePoll::Pending { size: None })
            }
        }
    }

    fn forget(&self, uri: &str) {
        self.inner
            .entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(uri);
    }

    fn forget_all(&self) {
        self.inner
            .entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
    }

    fn byte_size(&self) -> usize {
        self.inner
            .entries
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .map(|entry| match entry {
                Entry::Ready(image) => image.pixels.len() * 4,
                _ => 0,
            })
            .sum()
    }
}
