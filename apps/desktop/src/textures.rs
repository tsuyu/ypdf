//! Rendered-page texture cache.
//!
//! Rasters are expensive to produce and large to keep, so the cache is bounded
//! by bytes rather than by count — one 4K page costs as much as fifty
//! thumbnails, and only a byte budget notices that (spec §25, §34).
//!
//! Keyed by page *and* pixel width *and* rotation: a page rendered for one zoom
//! level is a different picture from the same page at another.

use std::collections::HashMap;

use egui::{ColorImage, Context, TextureHandle, TextureOptions};
use ypdf_render::{PageIndex, RenderedPage};

/// What makes one raster distinct from another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PageKey {
    /// Page index.
    pub page: PageIndex,
    /// Rendered pixel width.
    pub width: u32,
    /// Quarter turns applied.
    pub rotation: u8,
    /// Whether the raster was inverted for dark reading.
    pub invert: bool,
}

struct Entry {
    texture: TextureHandle,
    bytes: usize,
    last_used: u64,
}

/// Byte-bounded LRU cache of page textures.
pub struct PageCache {
    entries: HashMap<PageKey, Entry>,
    budget_bytes: usize,
    bytes: usize,
    clock: u64,
}

impl PageCache {
    /// A cache holding at most `budget_mb` megabytes of textures.
    #[must_use]
    pub fn new(budget_mb: u64) -> Self {
        Self {
            entries: HashMap::new(),
            budget_bytes: usize::try_from(budget_mb.saturating_mul(1024 * 1024))
                .unwrap_or(usize::MAX),
            bytes: 0,
            clock: 0,
        }
    }

    /// Look a texture up, marking it as recently used.
    pub fn get(&mut self, key: PageKey) -> Option<&TextureHandle> {
        self.clock += 1;
        let clock = self.clock;
        let entry = self.entries.get_mut(&key)?;
        entry.last_used = clock;
        Some(&entry.texture)
    }

    /// Is this raster already held? Does not affect LRU order.
    #[must_use]
    pub fn contains(&self, key: PageKey) -> bool {
        self.entries.contains_key(&key)
    }

    /// Upload a finished raster, evicting older entries to stay in budget.
    ///
    /// Inversion happens here rather than in the engine: the raster PDFium
    /// produced is the document as it really is, and dark reading is a property
    /// of this viewer, not of the file.
    pub fn insert(&mut self, ctx: &Context, page: &RenderedPage, invert: bool) -> PageKey {
        let key = PageKey {
            page: page.page,
            width: page.width,
            rotation: page.rotation.normalized(),
            invert,
        };

        let size = [page.width as usize, page.height as usize];
        let image = if invert {
            let mut rgba = page.rgba.clone();
            // Alpha is left alone; inverting it would turn the page transparent.
            for px in rgba.chunks_exact_mut(4) {
                px[0] = 255 - px[0];
                px[1] = 255 - px[1];
                px[2] = 255 - px[2];
            }
            ColorImage::from_rgba_unmultiplied(size, &rgba)
        } else {
            ColorImage::from_rgba_unmultiplied(size, &page.rgba)
        };
        let texture = ctx.load_texture(
            format!(
                "page-{}-{}-{}-{}",
                key.page,
                key.width,
                key.rotation,
                u8::from(invert)
            ),
            image,
            TextureOptions::LINEAR,
        );

        self.clock += 1;
        let bytes = page.rgba.len();
        if let Some(old) = self.entries.insert(
            key,
            Entry {
                texture,
                bytes,
                last_used: self.clock,
            },
        ) {
            self.bytes = self.bytes.saturating_sub(old.bytes);
        }
        self.bytes += bytes;
        self.evict_to_budget();
        key
    }

    /// Drop everything. Used when a document closes.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    /// Bytes currently held.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Number of cached rasters.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The best available raster for a page, whatever its width.
    ///
    /// Lets the viewer show a scaled stand-in during a zoom instead of a blank
    /// rectangle, which is the difference between "smooth" and "flickering".
    pub fn nearest(
        &mut self,
        page: PageIndex,
        rotation: u8,
        invert: bool,
        target_width: u32,
    ) -> Option<PageKey> {
        let best = self
            .entries
            .keys()
            .filter(|k| k.page == page && k.rotation == rotation && k.invert == invert)
            .min_by_key(|k| k.width.abs_diff(target_width))
            .copied()?;
        self.clock += 1;
        if let Some(entry) = self.entries.get_mut(&best) {
            entry.last_used = self.clock;
        }
        Some(best)
    }

    fn evict_to_budget(&mut self) {
        while self.bytes > self.budget_bytes && self.entries.len() > 1 {
            let Some(victim) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| *k)
            else {
                break;
            };
            if let Some(entry) = self.entries.remove(&victim) {
                self.bytes = self.bytes.saturating_sub(entry.bytes);
            }
        }
    }
}

impl std::fmt::Debug for PageCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageCache")
            .field("entries", &self.entries.len())
            .field("bytes", &self.bytes)
            .field("budget_bytes", &self.budget_bytes)
            .finish()
    }
}
