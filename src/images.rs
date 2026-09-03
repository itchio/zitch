//! Cover art. Downloading, decoding and scaling happen on worker threads,
//! which upload straight to the GPU; the interface only ever sees finished
//! textures, so a slow cover can never hold up a frame.
//!
//! Each cover is scaled once into JPEG variants on disk, one per width the
//! policy uses, so later runs decode a small file instead of the original.
//! Textures live in a byte-budgeted LRU and the disk cache is pruned by
//! age, both sized by the [`Policy`] in force.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use egui::{ColorImage, TextureHandle};

const WORKERS: usize = 4;
const JPEG_QUALITY: u8 = 85;
/// Failed loads try again after this, so covers fill in once the network
/// is back.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// Behind transparent cover pixels, which JPEG cannot keep.
const BACKDROP: [u8; 3] = [0x14, 0x12, 0x1a];

/// How much the covers may cost, chosen for the screen and the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub low_spec: bool,
    /// Width in pixels of the variant tiles draw.
    pub thumb_width: u32,
    /// Width in pixels of the variant the detail page draws.
    pub detail_width: u32,
    pub texture_budget: usize,
    pub disk_budget: u64,
    /// Largest original the loader will download.
    pub max_download: u64,
    /// Whether the focused tile plays animated covers at all.
    pub animate: bool,
    /// Frames beyond this many bytes leave a cover still.
    pub animation_budget: usize,
}

impl Policy {
    /// Screens shorter than this are treated as handhelds.
    const LOW_SPEC_HEIGHT: f32 = 600.0;

    /// The policy for a screen this many points tall, unless `low_spec`
    /// forces one.
    pub fn for_screen(height: f32, low_spec: Option<bool>) -> Self {
        if low_spec.unwrap_or(height < Self::LOW_SPEC_HEIGHT) {
            Self {
                low_spec: true,
                thumb_width: 200,
                detail_width: 400,
                texture_budget: 24 << 20,
                disk_budget: 50 << 20,
                max_download: 2 << 20,
                animate: false,
                animation_budget: 0,
            }
        } else {
            Self {
                low_spec: false,
                thumb_width: 400,
                detail_width: 630,
                texture_budget: 96 << 20,
                disk_budget: 200 << 20,
                max_download: 8 << 20,
                animate: true,
                animation_budget: 48 << 20,
            }
        }
    }

    fn widths(&self) -> [u32; 2] {
        [self.thumb_width, self.detail_width]
    }
}

/// Which scaled copy of a cover to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    Thumb,
    Detail,
}

/// Frames of an animated cover, with how long each stays up.
pub struct Animation {
    pub frames: Vec<Arc<ColorImage>>,
    pub delays: Vec<Duration>,
    pub total: Duration,
}

impl Animation {
    /// The frame showing `elapsed` into a looping playback, and how long
    /// until the next one.
    pub fn frame_at(&self, elapsed: Duration) -> (usize, Duration) {
        let mut t =
            Duration::from_nanos((elapsed.as_nanos() % self.total.as_nanos().max(1)) as u64);
        for (index, delay) in self.delays.iter().enumerate() {
            if t < *delay {
                return (index, *delay - t);
            }
            t -= *delay;
        }
        (self.frames.len() - 1, Duration::ZERO)
    }
}

enum Entry<T> {
    Pending,
    Ready(Arc<T>),
    Failed(String),
}

type Key = (String, u32);

struct Slot {
    handle: TextureHandle,
    bytes: usize,
    last_used: u64,
}

#[derive(Default)]
struct Textures {
    ready: HashMap<Key, Slot>,
    pending: HashSet<Key>,
    failed: HashMap<Key, Instant>,
    used: usize,
    frame: u64,
}

enum Job {
    Texture {
        key: Key,
        policy: Policy,
        ctx: egui::Context,
    },
    Animation {
        url: String,
        policy: Policy,
        ctx: egui::Context,
    },
}

struct Inner {
    textures: Mutex<Textures>,
    /// Only the focused tile animates, so this holds one finished
    /// animation at a time plus whatever is being decoded.
    animations: Mutex<HashMap<String, Entry<Animation>>>,
    queue: Mutex<mpsc::Sender<Job>>,
    policy: Mutex<Policy>,
    cache_dir: PathBuf,
    /// Bytes on disk, from a scan at startup plus every write since.
    disk_used: AtomicU64,
    /// Held while the cache directory changes, so the count and the files
    /// agree and a prune never runs against a half-written file.
    disk: Mutex<()>,
}

#[derive(Clone)]
pub struct CoverLoader {
    inner: Arc<Inner>,
}

impl CoverLoader {
    pub fn new(cache_dir: PathBuf, policy: Policy) -> Self {
        if let Err(error) = std::fs::create_dir_all(&cache_dir) {
            log::warn!("no cover cache at {}: {error}", cache_dir.display());
        }
        let (tx, rx) = mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        let loader = Self {
            inner: Arc::new(Inner {
                textures: Mutex::default(),
                animations: Mutex::default(),
                queue: Mutex::new(tx),
                policy: Mutex::new(policy),
                cache_dir,
                disk_used: AtomicU64::new(0),
                disk: Mutex::new(()),
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
        let inner = Arc::clone(&loader.inner);
        std::thread::Builder::new()
            .name("cover-prune".into())
            .spawn(move || inner.scan_disk())
            .expect("spawning cover scan");
        loader
    }

    pub fn policy(&self) -> Policy {
        *self.inner.policy.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Changes the budgets; textures over the new budget go at the end of
    /// the frame.
    pub fn set_policy(&self, policy: Policy) {
        let mut current = self.inner.policy.lock().unwrap_or_else(|p| p.into_inner());
        if *current != policy {
            log::info!("cover policy: {policy:?}");
            *current = policy;
        }
    }

    /// The cover scaled for `variant`, once loaded. Asking starts the work.
    pub fn texture(
        &self,
        ctx: &egui::Context,
        url: &str,
        variant: Variant,
    ) -> Option<TextureHandle> {
        let policy = self.policy();
        let width = match variant {
            Variant::Thumb => policy.thumb_width,
            Variant::Detail => policy.detail_width,
        };
        let key = (url.to_string(), width);
        let mut textures = self
            .inner
            .textures
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let frame = textures.frame;
        if let Some(slot) = textures.ready.get_mut(&key) {
            slot.last_used = frame;
            return Some(slot.handle.clone());
        }
        if textures.pending.contains(&key) {
            return None;
        }
        if let Some(failed_at) = textures.failed.get(&key)
            && failed_at.elapsed() < RETRY_AFTER
        {
            return None;
        }
        textures.failed.remove(&key);
        textures.pending.insert(key.clone());
        drop(textures);
        self.enqueue(Job::Texture {
            key,
            policy,
            ctx: ctx.clone(),
        });
        None
    }

    /// Drops the least recently drawn textures until the budget holds. Call
    /// once per frame after drawing.
    pub fn end_frame(&self) {
        let budget = self.policy().texture_budget;
        let mut textures = self
            .inner
            .textures
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        textures.frame += 1;
        if textures.used <= budget {
            return;
        }
        let frame = textures.frame;
        let mut by_age: Vec<(u64, Key)> = textures
            .ready
            .iter()
            // Anything drawn this frame or the last stays: it is on screen.
            .filter(|(_, slot)| slot.last_used + 2 < frame)
            .map(|(key, slot)| (slot.last_used, key.clone()))
            .collect();
        by_age.sort_unstable();
        for (_, key) in by_age {
            if textures.used <= budget {
                break;
            }
            if let Some(slot) = textures.ready.remove(&key) {
                textures.used -= slot.bytes;
            }
        }
        log::debug!(
            "covers: {} textures, {} MB",
            textures.ready.len(),
            textures.used >> 20
        );
    }

    /// All frames of `url`, once decoded. Asking starts the work and forgets
    /// every other finished animation, since only one plays at a time.
    /// `None` for good when the policy does not animate.
    pub fn animation(&self, ctx: &egui::Context, url: &str) -> Option<Arc<Animation>> {
        let policy = self.policy();
        if !policy.animate {
            return None;
        }
        let mut animations = self
            .inner
            .animations
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        animations.retain(|key, entry| key == url || matches!(entry, Entry::Pending));
        match animations.get(url) {
            Some(Entry::Ready(animation)) => return Some(Arc::clone(animation)),
            Some(_) => return None,
            None => {}
        }
        animations.insert(url.to_string(), Entry::Pending);
        drop(animations);
        self.enqueue(Job::Animation {
            url: url.to_string(),
            policy,
            ctx: ctx.clone(),
        });
        None
    }

    fn enqueue(&self, job: Job) {
        let _ = self
            .inner
            .queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .send(job);
    }
}

impl Inner {
    fn complete(&self, job: Job) {
        match job {
            Job::Texture { key, policy, ctx } => {
                let (url, width) = &key;
                let outcome = self.variant(url, *width, &policy);
                let mut textures = self.textures.lock().unwrap_or_else(|p| p.into_inner());
                textures.pending.remove(&key);
                match outcome {
                    Ok(image) => {
                        let bytes = image.pixels.len() * 4;
                        let handle = ctx.load_texture(
                            format!("{url}@{width}"),
                            image,
                            egui::TextureOptions::LINEAR,
                        );
                        textures.used += bytes;
                        let frame = textures.frame;
                        textures.ready.insert(
                            key,
                            Slot {
                                handle,
                                bytes,
                                last_used: frame,
                            },
                        );
                    }
                    Err(error) => {
                        log::debug!("cover {url}: {error}");
                        textures.failed.insert(key, Instant::now());
                    }
                }
                drop(textures);
                ctx.request_repaint();
            }
            Job::Animation { url, policy, ctx } => {
                let started = Instant::now();
                let outcome = self
                    .fetch(&url, &policy)
                    .and_then(|bytes| decode_animation(&bytes, &policy));
                let entry = match outcome {
                    Ok(animation) => {
                        log::debug!(
                            "decoded {} frames of {url} in {:?}",
                            animation.frames.len(),
                            started.elapsed()
                        );
                        Entry::Ready(Arc::new(animation))
                    }
                    Err(error) => {
                        log::debug!("animated cover {url}: {error}");
                        Entry::Failed(error)
                    }
                };
                let mut animations = self.animations.lock().unwrap_or_else(|p| p.into_inner());
                // Focus may have moved on; a forgotten request stays forgotten.
                if let Some(slot) = animations.get_mut(&url) {
                    *slot = entry;
                    ctx.request_repaint();
                }
            }
        }
    }

    /// The cover scaled to `width`: from its file when there is one, else
    /// made from the original along with every other width the policy
    /// uses, so the original is read once.
    fn variant(&self, url: &str, width: u32, policy: &Policy) -> Result<ColorImage, String> {
        let path = self.variant_path(url, width);
        if let Ok(bytes) = std::fs::read(&path) {
            touch(&path);
            let decoded = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
                .map_err(|e| e.to_string())?;
            return Ok(to_color_image(decoded));
        }
        let bytes = self.fetch(url, policy)?;
        let first = first_frame(&bytes)?;
        let mut wanted = None;
        for w in policy.widths() {
            let scaled = scale(&first, w);
            let jpeg = encode_jpeg(&scaled)?;
            self.write(&self.variant_path(url, w), &jpeg);
            if w == width {
                wanted = Some(scaled);
            }
        }
        // The original only earns its space if it can still animate.
        let keeps_original = policy.animate
            && image::guess_format(&bytes).is_ok_and(|f| f == image::ImageFormat::Gif);
        if !keeps_original {
            self.remove(&self.original_path(url));
        }
        let scaled = match wanted {
            Some(scaled) => scaled,
            None => scale(&first, width),
        };
        Ok(to_color_image(scaled))
    }

    fn fetch(&self, url: &str, policy: &Policy) -> Result<Vec<u8>, String> {
        let path = self.original_path(url);
        if let Ok(bytes) = std::fs::read(&path) {
            touch(&path);
            return Ok(bytes);
        }
        let response = ureq::get(url).call().map_err(|e| e.to_string())?;
        let bytes = response
            .into_body()
            .with_config()
            .limit(policy.max_download)
            .read_to_vec()
            .map_err(|e| e.to_string())?;
        self.write(&path, &bytes);
        Ok(bytes)
    }

    fn original_path(&self, url: &str) -> PathBuf {
        self.cache_dir.join(cache_name(url))
    }

    fn variant_path(&self, url: &str, width: u32) -> PathBuf {
        self.cache_dir
            .join(format!("v2-{}-w{width}.jpg", hash(url)))
    }

    /// Writes beside, then renames, so a reader never sees a partial file.
    fn write(&self, path: &Path, bytes: &[u8]) {
        let _guard = self.disk.lock().unwrap_or_else(|p| p.into_inner());
        let replaced = std::fs::metadata(path).map_or(0, |meta| meta.len());
        let tmp = path.with_extension("part");
        if std::fs::write(&tmp, bytes)
            .and_then(|()| std::fs::rename(&tmp, path))
            .is_err()
        {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        self.disk_used.fetch_sub(replaced, Ordering::Relaxed);
        let used = self
            .disk_used
            .fetch_add(bytes.len() as u64, Ordering::Relaxed)
            + bytes.len() as u64;
        let budget = self
            .policy
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .disk_budget;
        if used > budget {
            self.prune_disk(budget);
        }
    }

    fn remove(&self, path: &Path) {
        let _guard = self.disk.lock().unwrap_or_else(|p| p.into_inner());
        if let Ok(meta) = std::fs::metadata(path)
            && std::fs::remove_file(path).is_ok()
        {
            self.disk_used.fetch_sub(meta.len(), Ordering::Relaxed);
        }
    }

    /// Sizes the cache at startup and prunes if a smaller budget applies.
    fn scan_disk(&self) {
        let _guard = self.disk.lock().unwrap_or_else(|p| p.into_inner());
        let total = self.cache_files().iter().map(|(_, len, _)| len).sum();
        self.disk_used.store(total, Ordering::Relaxed);
        let budget = self
            .policy
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .disk_budget;
        log::info!("cover cache: {} MB of {} MB", total >> 20, budget >> 20);
        if total > budget {
            self.prune_disk(budget);
        }
    }

    /// Finished cache files with their modification time and size.
    fn cache_files(&self) -> Vec<(SystemTime, u64, PathBuf)> {
        let Ok(entries) = std::fs::read_dir(&self.cache_dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| entry.path().extension().is_none_or(|ext| ext != "part"))
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                let modified = meta.modified().ok()?;
                Some((modified, meta.len(), entry.path()))
            })
            .collect()
    }

    /// Deletes the least recently used files until the cache is well under
    /// budget, so pruning does not run again on the next write. The caller
    /// holds the disk lock.
    fn prune_disk(&self, budget: u64) {
        let mut files = self.cache_files();
        files.sort();
        let target = budget / 10 * 9;
        let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
        let mut removed = 0;
        for (_, len, path) in files {
            if total <= target {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total -= len;
                removed += 1;
            }
        }
        self.disk_used.store(total, Ordering::Relaxed);
        log::info!(
            "cover cache: pruned {removed} files, {} MB left",
            total >> 20
        );
    }
}

/// Marks a cache file as just used, for pruning by age.
fn touch(path: &Path) {
    if let Ok(file) = std::fs::File::options().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}

/// The first frame of any supported format. Animated covers can run to
/// megabytes and hundreds of frames; one frame is all a still needs.
fn first_frame(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    use image::AnimationDecoder;
    let format = image::guess_format(bytes).map_err(|e| e.to_string())?;
    if format == image::ImageFormat::Gif {
        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes))
            .map_err(|e| e.to_string())?;
        let frame = decoder
            .into_frames()
            .next()
            .ok_or("gif has no frames")?
            .map_err(|e| e.to_string())?;
        Ok(image::DynamicImage::ImageRgba8(frame.into_buffer()))
    } else {
        image::load_from_memory_with_format(bytes, format).map_err(|e| e.to_string())
    }
}

/// Every frame of a gif at thumb size, within the animation budget. Other
/// formats yield one frame that never changes.
fn decode_animation(bytes: &[u8], policy: &Policy) -> Result<Animation, String> {
    use image::AnimationDecoder;
    let format = image::guess_format(bytes).map_err(|e| e.to_string())?;
    if format != image::ImageFormat::Gif {
        let frame = to_color_image(scale(&first_frame(bytes)?, policy.thumb_width));
        return Ok(Animation {
            frames: vec![Arc::new(frame)],
            delays: vec![Duration::from_secs(1)],
            total: Duration::from_secs(1),
        });
    }
    let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes))
        .map_err(|e| e.to_string())?;
    let mut frames = Vec::new();
    let mut delays = Vec::new();
    let mut total = Duration::ZERO;
    let mut used = 0;
    for frame in decoder.into_frames() {
        let frame = frame.map_err(|e| e.to_string())?;
        let (numer, denom) = frame.delay().numer_denom_ms();
        // Browsers treat very short delays as 100 ms, so do the same.
        let ms = numer as f64 / denom.max(1) as f64;
        let delay = if ms < 20.0 { 100.0 } else { ms };
        let delay = Duration::from_secs_f64(delay / 1000.0);
        let image = to_color_image(scale(
            &image::DynamicImage::ImageRgba8(frame.into_buffer()),
            policy.thumb_width,
        ));
        used += image.pixels.len() * 4;
        if used > policy.animation_budget {
            return Err(format!(
                "over the animation budget after {} frames",
                frames.len()
            ));
        }
        frames.push(Arc::new(image));
        delays.push(delay);
        total += delay;
    }
    if frames.is_empty() {
        return Err("gif has no frames".into());
    }
    Ok(Animation {
        frames,
        delays,
        total,
    })
}

/// Scales down to `width` wide, keeping the aspect; never scales up.
fn scale(image: &image::DynamicImage, width: u32) -> image::DynamicImage {
    if image.width() <= width {
        return image.clone();
    }
    let height = (u64::from(image.height()) * u64::from(width) / u64::from(image.width())).max(1);
    image.resize_exact(width, height as u32, image::imageops::FilterType::Triangle)
}

/// JPEG bytes, with transparent pixels laid over the page background.
fn encode_jpeg(image: &image::DynamicImage) -> Result<Vec<u8>, String> {
    let rgba = image.to_rgba8();
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (dst, src) in rgb.pixels_mut().zip(rgba.pixels()) {
        let alpha = u32::from(src[3]);
        for c in 0..3 {
            let over = u32::from(src[c]) * alpha + u32::from(BACKDROP[c]) * (255 - alpha);
            dst[c] = (over / 255) as u8;
        }
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY)
        .encode_image(&rgb)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

fn to_color_image(decoded: image::DynamicImage) -> ColorImage {
    let rgba = decoded.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    ColorImage::from_rgba_unmultiplied(size, rgba.as_raw())
}

fn hash(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// A filename for an original that is unique to the URL and safe everywhere.
fn cache_name(url: &str) -> String {
    let ext = url
        .rsplit('.')
        .next()
        .filter(|ext| ext.len() <= 4 && ext.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("img");
    format!("{}.{ext}", hash(url))
}
