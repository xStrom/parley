//! Cache for font data.

use super::source::{SourceId, SourceInfo, SourceKind};
use hashbrown::HashMap;
use peniko::{Blob, WeakBlob};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

/// Options for a source cache.
#[derive(Copy, Clone, Default, Debug)]
pub struct SourceCacheOptions {
    /// If true, the source cache will use a secondary shared cache
    /// guaranteeing that all clones will use the same backing store.
    ///
    /// This is useful for ensuring that only one copy of font data is
    /// loaded into memory in multi-threaded scenarios.
    ///
    /// The default value is false.
    pub shared: bool,
}

/// Cache for font data loaded from the file system.
#[derive(Clone, Default)]
pub struct SourceCache {
    cache: HashMap<SourceId, Entry<Blob<u8>>>,
    serial: u64,
    shared: Option<Arc<Mutex<Shared>>>,
}

impl SourceCache {
    /// Creates an empty cache with the given options.
    pub fn new(options: SourceCacheOptions) -> Self {
        if options.shared {
            Self {
                cache: Default::default(),
                serial: 0,
                shared: Some(Arc::new(Mutex::new(Shared::default()))),
            }
        } else {
            Self::default()
        }
    }

    /// Creates an emtpy cache that is suitable for multi-threaded use.
    ///
    /// A cache created with this function maintains a synchronized internal
    /// store that is shared among all clones.
    pub fn new_shared() -> Self {
        Self {
            cache: Default::default(),
            serial: 0,
            shared: Some(Arc::new(Mutex::new(Shared::default()))),
        }
    }

    /// Returns the blob for the given font data, attempting to load
    /// it from the file system if not already present.
    ///
    /// Returns `None` if loading failed.
    pub fn get(&mut self, source: &SourceInfo) -> Option<Blob<u8>> {
        let path = match &source.kind {
            SourceKind::Memory(memory) => return Some(memory.clone()),
            SourceKind::Path(path) => &**path,
        };
        use hashbrown::hash_map::Entry as HashEntry;
        match self.cache.entry(source.id()) {
            HashEntry::Vacant(vacant) => {
                if let Some(mut shared) = self.shared.as_ref().and_then(|shared| shared.lock().ok())
                {
                    // If we have a backing cache, try to load it there first
                    // and then propagate the result here.
                    if let Some(blob) = shared.get(source.id(), path) {
                        vacant.insert(Entry::Loaded(EntryData {
                            font_data: blob.clone(),
                            serial: self.serial,
                        }));
                        Some(blob)
                    } else {
                        vacant.insert(Entry::Failed);
                        None
                    }
                } else {
                    // Otherwise, load it ourselves.
                    if let Some(blob) = load_blob(path) {
                        vacant.insert(Entry::Loaded(EntryData {
                            font_data: blob.clone(),
                            serial: self.serial,
                        }));
                        Some(blob)
                    } else {
                        vacant.insert(Entry::Failed);
                        None
                    }
                }
            }
            HashEntry::Occupied(mut occupied) => {
                let entry = occupied.get_mut();
                match entry {
                    Entry::Loaded(data) => {
                        data.serial = self.serial;
                        Some(data.font_data.clone())
                    }
                    Entry::Failed => None,
                }
            }
        }
    }

    /// Removes all cached blobs that have not been accessed in the last
    /// `max_age` times `prune` has been called.
    pub fn prune(&mut self, max_age: u64, prune_failed: bool) {
        self.cache.retain(|_, entry| match entry {
            Entry::Failed => !prune_failed,
            Entry::Loaded(data) => self.serial.saturating_sub(data.serial) < max_age,
        });
        self.serial = self.serial.saturating_add(1);
    }
}

/// Shared backing store for a font data cache.
#[derive(Default)]
struct Shared {
    cache: HashMap<SourceId, Entry<WeakBlob<u8>>>,
}

impl Shared {
    pub fn get(&mut self, id: SourceId, path: &Path) -> Option<Blob<u8>> {
        use hashbrown::hash_map::Entry as HashEntry;
        match self.cache.entry(id) {
            HashEntry::Vacant(vacant) => {
                if let Some(blob) = load_blob(path) {
                    vacant.insert(Entry::Loaded(EntryData {
                        font_data: blob.clone().downgrade(),
                        serial: 0,
                    }));
                    Some(blob)
                } else {
                    vacant.insert(Entry::Failed);
                    None
                }
            }
            HashEntry::Occupied(mut occupied) => {
                let entry = occupied.get_mut();
                match entry {
                    Entry::Loaded(data) => {
                        if let Some(blob) = data.font_data.upgrade() {
                            // The weak ref is still valid.
                            Some(blob)
                        } else if let Some(blob) = load_blob(path) {
                            // Otherwise, try to reload it.
                            data.font_data = blob.downgrade();
                            Some(blob)
                        } else {
                            // We failed for some reason.. don't try again.
                            *entry = Entry::Failed;
                            None
                        }
                    }
                    Entry::Failed => None,
                }
            }
        }
    }
}

#[derive(Clone, Default)]
enum Entry<T> {
    Loaded(EntryData<T>),
    #[default]
    Failed,
}

#[derive(Clone)]
struct EntryData<T> {
    font_data: T,
    serial: u64,
}

pub(crate) fn load_blob(path: &Path) -> Option<Blob<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mapped = unsafe { memmap2::Mmap::map(&file).ok()? };
    Some(Blob::new(Arc::new(mapped)))
}
