//! Cover art. Downloading, decoding and scaling happen on worker threads,
//! which upload straight to the GPU; the interface only ever sees finished
//! textures, so a slow cover can never hold up a frame.
//!
//! Each cover is scaled once into JPEG variants on disk, one per width the
//! policy uses, so later runs decode a small file instead of the original.
//! Textures live in a byte-budgeted LRU and the disk cache is pruned by
//! age, both sized by the [`Policy`] in force.
//!
//! Screenshots for the game page go through the same workers but stay in
//! memory: nothing about them is written to disk, and their textures have
//! their own budget so they never push covers out.
//!
//! Work is asked for every frame by whatever is drawn. The workers take
//! the most important request first, and a request nothing has asked for
//! in the last couple of frames is dropped before it starts, so covers
//! scrolled away or filtered out stop costing anything.

use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use egui::{ColorImage, TextureHandle};

const WORKERS: usize = 4;
const JPEG_QUALITY: u8 = 85;
/// How much of a screenshot to read before trying to decode its first
/// frame; doubled until it decodes or the download limit is reached.
const STILL_CHUNK: u64 = 256 << 10;
/// Failed loads try again after this, so covers fill in once the network
/// is back.
const RETRY_AFTER: Duration = Duration::from_secs(60);
/// A screenshot over the download limit waits this long instead.
const TOO_LARGE_RETRY: Duration = Duration::from_secs(6 * 60 * 60);
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
    /// Width in pixels screenshots are scaled down to.
    pub screenshot_width: u32,
    pub screenshot_budget: usize,
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
                screenshot_width: 560,
                screenshot_budget: 8 << 20,
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
                screenshot_width: 1024,
                screenshot_budget: 32 << 20,
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

/// How soon a cover is needed; the workers take the highest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Priority {
    /// Just past the edge of the screen.
    Soon,
    /// On screen.
    Shown,
    /// The page's one large cover.
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
    Failed,
}

type Key = (String, u32);

struct Slot {
    handle: TextureHandle,
    bytes: usize,
    last_used: u64,
}

/// Finished textures by `K`, with failures waiting to be tried again.
struct Textures<K> {
    ready: HashMap<K, Slot>,
    /// When each failure may be tried again.
    failed: HashMap<K, Instant>,
    used: usize,
}

impl<K> Default for Textures<K> {
    fn default() -> Self {
        Self {
            ready: HashMap::new(),
            failed: HashMap::new(),
            used: 0,
        }
    }
}

impl<K: Hash + Eq + Clone> Textures<K> {
    /// The texture for `key` when it is ready, marked as drawn this frame.
    fn get<Q>(&mut self, key: &Q, frame: u64) -> Option<TextureHandle>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let slot = self.ready.get_mut(key)?;
        slot.last_used = frame;
        Some(slot.handle.clone())
    }

    /// False while a failure for `key` has not waited out its retry.
    fn should_load<Q>(&mut self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        match self.failed.get(key) {
            Some(retry_at) if Instant::now() < *retry_at => false,
            Some(_) => {
                self.failed.remove(key);
                true
            }
            None => true,
        }
    }

    fn clear(&mut self) {
        *self = Self::default();
    }

    fn insert(&mut self, key: K, handle: TextureHandle, bytes: usize, frame: u64) {
        self.used += bytes;
        if let Some(old) = self.ready.insert(
            key,
            Slot {
                handle,
                bytes,
                last_used: frame,
            },
        ) {
            self.used -= old.bytes;
        }
    }

    /// Drops the least recently drawn textures until `budget` holds.
    fn evict(&mut self, budget: usize, frame: u64) {
        if self.used <= budget {
            return;
        }
        let mut by_age: Vec<(u64, K)> = self
            .ready
            .iter()
            // Anything drawn this frame or the last stays: it is on screen.
            .filter(|(_, slot)| slot.last_used + 2 < frame)
            .map(|(key, slot)| (slot.last_used, key.clone()))
            .collect();
        by_age.sort_unstable_by_key(|(age, _)| *age);
        for (_, key) in by_age {
            if self.used <= budget {
                break;
            }
            if let Some(slot) = self.ready.remove(&key) {
                self.used -= slot.bytes;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Job {
    Texture(Key),
    Animation(String),
    /// Scaled to the policy's screenshot width.
    Screenshot(String),
}

struct Request {
    priority: Priority,
    /// The last frame that asked.
    frame: u64,
    /// Order of first asking, so equal priorities go in drawing order.
    seq: u64,
    ctx: egui::Context,
}

#[derive(Default)]
struct Queue {
    waiting: HashMap<Job, Request>,
    /// Taken by a worker and not finished.
    running: HashSet<Job>,
    seq: u64,
}

impl Queue {
    /// Asks for `job`, raising its priority if it is already waiting.
    /// False when it is running, so nothing new was queued.
    fn ask(&mut self, job: Job, priority: Priority, frame: u64, ctx: &egui::Context) -> bool {
        if self.running.contains(&job) {
            return false;
        }
        if let Some(request) = self.waiting.get_mut(&job) {
            request.priority = request.priority.max(priority);
            request.frame = frame;
            return false;
        }
        self.seq += 1;
        self.waiting.insert(
            job,
            Request {
                priority,
                frame,
                seq: self.seq,
                ctx: ctx.clone(),
            },
        );
        true
    }

    /// The most important waiting job, moved to running.
    fn take(&mut self) -> Option<(Job, egui::Context)> {
        let job = self
            .waiting
            .iter()
            .max_by_key(|(_, r)| (r.priority, r.frame, std::cmp::Reverse(r.seq)))
            .map(|(job, _)| job.clone())?;
        let request = self.waiting.remove(&job)?;
        self.running.insert(job.clone());
        Some((job, request.ctx))
    }
}

struct Inner {
    textures: Mutex<Textures<Key>>,
    /// By URL; all at the policy's screenshot width.
    screenshots: Mutex<Textures<String>>,
    /// Only the focused tile animates, so this holds one finished
    /// animation at a time plus whatever is being decoded.
    animations: Mutex<HashMap<String, Entry<Animation>>>,
    queue: Mutex<Queue>,
    /// Wakes a worker when a job is queued.
    queued: Condvar,
    /// Frames drawn so far, for recency.
    frame: AtomicU64,
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
        let loader = Self {
            inner: Arc::new(Inner {
                textures: Mutex::default(),
                screenshots: Mutex::default(),
                animations: Mutex::default(),
                queue: Mutex::default(),
                queued: Condvar::new(),
                frame: AtomicU64::new(0),
                policy: Mutex::new(policy),
                cache_dir,
                disk_used: AtomicU64::new(0),
                disk: Mutex::new(()),
            }),
        };
        for n in 0..WORKERS {
            let inner = Arc::clone(&loader.inner);
            std::thread::Builder::new()
                .name(format!("cover-{n}"))
                .spawn(move || inner.work())
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
            if current.screenshot_width != policy.screenshot_width {
                self.inner
                    .screenshots
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clear();
            }
            *current = policy;
        }
    }

    /// The cover scaled for `variant`, once loaded. Asking starts the work,
    /// and asking again each frame keeps it wanted.
    pub fn texture(
        &self,
        ctx: &egui::Context,
        url: &str,
        variant: Variant,
    ) -> Option<TextureHandle> {
        let priority = match variant {
            Variant::Thumb => Priority::Shown,
            Variant::Detail => Priority::Detail,
        };
        self.request(ctx, url, variant, priority)
    }

    /// Loads a thumb about to scroll into view, behind everything shown.
    pub fn prefetch(&self, ctx: &egui::Context, url: &str) {
        self.request(ctx, url, Variant::Thumb, Priority::Soon);
    }

    fn request(
        &self,
        ctx: &egui::Context,
        url: &str,
        variant: Variant,
        priority: Priority,
    ) -> Option<TextureHandle> {
        let policy = self.policy();
        let width = match variant {
            Variant::Thumb => policy.thumb_width,
            Variant::Detail => policy.detail_width,
        };
        let key = (url.to_string(), width);
        let frame = self.inner.frame.load(Ordering::Relaxed);
        let mut textures = self
            .inner
            .textures
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(handle) = textures.get(&key, frame) {
            return Some(handle);
        }
        if !textures.should_load(&key) {
            return None;
        }
        drop(textures);
        self.enqueue(Job::Texture(key), priority, frame, ctx);
        None
    }

    /// A screenshot scaled to the policy's screenshot width, once loaded.
    /// Asking starts the work, and asking again each frame keeps it wanted.
    pub fn screenshot(&self, ctx: &egui::Context, url: &str) -> Option<TextureHandle> {
        self.request_screenshot(ctx, url, Priority::Shown)
    }

    /// The size of each screenshot already loaded, without asking for any.
    pub fn screenshot_sizes(&self, urls: &[String]) -> Vec<Option<egui::Vec2>> {
        let screenshots = self
            .inner
            .screenshots
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        urls.iter()
            .map(|url| {
                screenshots
                    .ready
                    .get(url.as_str())
                    .map(|slot| slot.handle.size_vec2())
            })
            .collect()
    }

    /// Loads a screenshot about to scroll into view, behind everything shown.
    pub fn prefetch_screenshot(&self, ctx: &egui::Context, url: &str) {
        self.request_screenshot(ctx, url, Priority::Soon);
    }

    fn request_screenshot(
        &self,
        ctx: &egui::Context,
        url: &str,
        priority: Priority,
    ) -> Option<TextureHandle> {
        let frame = self.inner.frame.load(Ordering::Relaxed);
        let mut screenshots = self
            .inner
            .screenshots
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(handle) = screenshots.get(url, frame) {
            return Some(handle);
        }
        if !screenshots.should_load(url) {
            return None;
        }
        drop(screenshots);
        self.enqueue(Job::Screenshot(url.to_string()), priority, frame, ctx);
        None
    }

    /// Drops the least recently drawn textures until the budget holds. Call
    /// once per frame after drawing.
    pub fn end_frame(&self) {
        let frame = self.inner.frame.fetch_add(1, Ordering::Relaxed) + 1;
        self.drop_unwanted(frame);
        let policy = self.policy();
        self.inner
            .screenshots
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .evict(policy.screenshot_budget, frame);
        let mut textures = self
            .inner
            .textures
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if textures.used <= policy.texture_budget {
            return;
        }
        textures.evict(policy.texture_budget, frame);
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
            Some(Entry::Failed) => return None,
            // Asked again below, so it stays wanted while focused.
            Some(Entry::Pending) => {}
            None => {
                animations.insert(url.to_string(), Entry::Pending);
            }
        }
        drop(animations);
        let frame = self.inner.frame.load(Ordering::Relaxed);
        self.enqueue(Job::Animation(url.to_string()), Priority::Shown, frame, ctx);
        None
    }

    fn enqueue(&self, job: Job, priority: Priority, frame: u64, ctx: &egui::Context) {
        let mut queue = self.inner.queue.lock().unwrap_or_else(|p| p.into_inner());
        if queue.ask(job, priority, frame, ctx) {
            self.inner.queued.notify_one();
        }
    }

    /// Forgets requests nothing asked for this frame or the last.
    fn drop_unwanted(&self, frame: u64) {
        let mut dropped = Vec::new();
        let mut queue = self.inner.queue.lock().unwrap_or_else(|p| p.into_inner());
        let before = queue.waiting.len();
        queue.waiting.retain(|job, request| {
            let keep = request.frame + 2 >= frame;
            if !keep && let Job::Animation(url) = job {
                dropped.push(url.clone());
            }
            keep
        });
        if queue.waiting.len() < before {
            log::debug!(
                "covers: dropped {} no longer shown, {} waiting",
                before - queue.waiting.len(),
                queue.waiting.len()
            );
        }
        drop(queue);
        // A dropped animation is asked for afresh when focus comes back.
        if !dropped.is_empty() {
            let mut animations = self
                .inner
                .animations
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            for url in dropped {
                if matches!(animations.get(&url), Some(Entry::Pending)) {
                    animations.remove(&url);
                }
            }
        }
    }
}

impl Inner {
    fn work(&self) {
        loop {
            let mut queue = self.queue.lock().unwrap_or_else(|p| p.into_inner());
            let (job, ctx) = loop {
                if let Some(next) = queue.take() {
                    break next;
                }
                queue = self.queued.wait(queue).unwrap_or_else(|p| p.into_inner());
            };
            drop(queue);
            self.complete(job.clone(), ctx);
            // After the result is stored, so an asker sees one or the other.
            self.queue
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .running
                .remove(&job);
        }
    }

    fn complete(&self, job: Job, ctx: egui::Context) {
        let policy = *self.policy.lock().unwrap_or_else(|p| p.into_inner());
        match job {
            Job::Texture(key) => {
                let (url, width) = &key;
                // Asked for again between finishing and leaving the running
                // set; it is here already.
                if self
                    .textures
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .ready
                    .contains_key(&key)
                {
                    return;
                }
                let name = format!("{url}@{width}");
                let outcome = self
                    .variant(url, *width, &policy)
                    .map_err(|e| (e, RETRY_AFTER));
                self.store(&self.textures, key, &name, outcome, &ctx);
            }
            Job::Screenshot(url) => {
                if self
                    .screenshots
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .ready
                    .contains_key(&url)
                {
                    return;
                }
                let width = policy.screenshot_width;
                let name = format!("{url}@{width}");
                let outcome = self
                    .fetch_still(&url, &policy)
                    .map(|first| to_color_image(scale(&first, width)));
                self.store(&self.screenshots, url, &name, outcome, &ctx);
            }
            Job::Animation(url) => {
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
                        Entry::Failed
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

    /// Uploads a finished image, or notes the failure with how long it
    /// waits before being tried again.
    fn store<K: Hash + Eq + Clone>(
        &self,
        textures: &Mutex<Textures<K>>,
        key: K,
        name: &str,
        outcome: Result<ColorImage, (String, Duration)>,
        ctx: &egui::Context,
    ) {
        let mut textures = textures.lock().unwrap_or_else(|p| p.into_inner());
        match outcome {
            Ok(image) => {
                let bytes = image.pixels.len() * 4;
                let handle = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
                let frame = self.frame.load(Ordering::Relaxed);
                textures.insert(key, handle, bytes, frame);
            }
            Err((error, wait)) => {
                log::debug!("image {name}: {error}");
                textures.failed.insert(key, Instant::now() + wait);
            }
        }
        drop(textures);
        ctx.request_repaint();
    }

    /// The first frame of `url`, downloaded without touching the disk. A
    /// gif is read only as far as its first frame needs, so a large
    /// animated screenshot costs about as much as a still one. Fails with
    /// how long to wait before trying again.
    fn fetch_still(
        &self,
        url: &str,
        policy: &Policy,
    ) -> Result<image::DynamicImage, (String, Duration)> {
        use std::io::Read;
        let retry = |e: String| (e, RETRY_AFTER);
        let response = crate::http::agent()
            .get(url)
            .call()
            .map_err(|e| retry(e.to_string()))?;
        // One byte past the limit tells a file at the limit from one over it.
        let cap = policy.max_download + 1;
        let mut body = response.into_body().into_reader().take(cap);
        let mut bytes = Vec::new();
        let mut want = STILL_CHUNK.min(cap);
        loop {
            let before = bytes.len();
            (&mut body)
                .take(want - before as u64)
                .read_to_end(&mut bytes)
                .map_err(|e| retry(e.to_string()))?;
            if (bytes.len() as u64) < want {
                return first_frame(&bytes).map_err(retry);
            }
            if bytes.starts_with(b"GIF8")
                && let Ok(first) = first_frame(&bytes)
            {
                return Ok(first);
            }
            if bytes.len() as u64 >= cap {
                // It will not get smaller; trying every minute only
                // downloads it again.
                return Err(("larger than the download limit".into(), TOO_LARGE_RETRY));
            }
            want = (want * 2).min(cap);
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
        let response = crate::http::agent()
            .get(url)
            .call()
            .map_err(|e| e.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn thumb(url: &str) -> Job {
        Job::Texture((url.to_string(), 200))
    }

    #[test]
    fn queue_takes_detail_before_shown_before_soon() {
        let ctx = egui::Context::default();
        let mut queue = Queue::default();
        queue.ask(thumb("soon"), Priority::Soon, 1, &ctx);
        queue.ask(thumb("a"), Priority::Shown, 1, &ctx);
        queue.ask(thumb("b"), Priority::Shown, 1, &ctx);
        queue.ask(thumb("detail"), Priority::Detail, 1, &ctx);
        let order: Vec<Job> = std::iter::from_fn(|| queue.take().map(|(job, _)| job)).collect();
        assert_eq!(
            order,
            vec![thumb("detail"), thumb("a"), thumb("b"), thumb("soon")]
        );
    }

    #[test]
    fn queue_prefers_what_was_asked_for_last() {
        let ctx = egui::Context::default();
        let mut queue = Queue::default();
        queue.ask(thumb("old"), Priority::Shown, 1, &ctx);
        queue.ask(thumb("new"), Priority::Shown, 5, &ctx);
        assert_eq!(queue.take().map(|(job, _)| job), Some(thumb("new")));
    }

    #[test]
    fn queue_raises_priority_and_skips_running_jobs() {
        let ctx = egui::Context::default();
        let mut queue = Queue::default();
        assert!(queue.ask(thumb("a"), Priority::Soon, 1, &ctx));
        assert!(!queue.ask(thumb("a"), Priority::Shown, 1, &ctx));
        assert_eq!(queue.waiting[&thumb("a")].priority, Priority::Shown);
        queue.take();
        assert!(!queue.ask(thumb("a"), Priority::Shown, 2, &ctx));
        assert!(queue.waiting.is_empty());
    }
}
