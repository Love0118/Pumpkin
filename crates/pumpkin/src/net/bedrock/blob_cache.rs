use bytes::Bytes;
use lru::LruCache;

use crate::net::outbound::{
    add_blob_cache_bytes, record_blob_cache_eviction, record_blob_cache_hit,
    remove_blob_cache_bytes,
};

pub(super) const DEFAULT_BLOB_CACHE_BYTE_LIMIT: usize = 64 * 1024 * 1024;

pub(super) struct BlobCache {
    entries: LruCache<u64, Bytes>,
    retained_bytes: usize,
    byte_limit: usize,
}

impl Default for BlobCache {
    fn default() -> Self {
        Self::new(DEFAULT_BLOB_CACHE_BYTE_LIMIT)
    }
}

impl BlobCache {
    pub(super) fn new(byte_limit: usize) -> Self {
        assert!(byte_limit > 0);
        Self {
            entries: LruCache::unbounded(),
            retained_bytes: 0,
            byte_limit,
        }
    }

    pub(super) fn insert(&mut self, hash: u64, payload: Vec<u8>) -> bool {
        if payload.len() > self.byte_limit {
            return false;
        }

        if let Some(previous) = self.entries.pop(&hash) {
            self.retained_bytes -= previous.len();
            remove_blob_cache_bytes(previous.len());
        }

        while self.retained_bytes + payload.len() > self.byte_limit {
            let Some((_hash, evicted)) = self.entries.pop_lru() else {
                break;
            };
            self.retained_bytes -= evicted.len();
            remove_blob_cache_bytes(evicted.len());
            record_blob_cache_eviction();
        }

        self.retained_bytes += payload.len();
        add_blob_cache_bytes(payload.len());
        self.entries.put(hash, Bytes::from(payload));
        true
    }

    pub(super) fn get(&mut self, hash: u64) -> Option<Bytes> {
        let payload = self.entries.get(&hash).cloned();
        record_blob_cache_hit(payload.is_some());
        payload
    }

    #[cfg(test)]
    const fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }
}

impl Drop for BlobCache {
    fn drop(&mut self) {
        remove_blob_cache_bytes(self.retained_bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::BlobCache;
    use crate::net::outbound::outbound_metrics_snapshot;

    #[test]
    fn insert_evicts_least_recently_used_blobs_to_the_byte_limit() {
        let before = outbound_metrics_snapshot();
        let mut cache = BlobCache::new(8);
        assert!(cache.insert(1, vec![1; 4]));
        assert!(cache.insert(2, vec![2; 4]));
        assert!(cache.get(1).is_some());

        assert!(cache.insert(3, vec![3; 4]));

        assert!(cache.retained_bytes() <= 8);
        assert!(cache.get(1).is_some());
        assert!(cache.get(2).is_none());
        assert!(cache.get(3).is_some());
        let after = outbound_metrics_snapshot();
        assert!(after.blob_cache_hits >= before.blob_cache_hits + 3);
        assert!(after.blob_cache_misses > before.blob_cache_misses);
        assert!(after.blob_cache_evictions > before.blob_cache_evictions);
        assert!(after.peak_blob_cache_bytes >= before.blob_cache_bytes + 8);
    }

    #[test]
    fn oversized_blob_is_rejected_without_evicting_existing_data() {
        let mut cache = BlobCache::new(8);
        assert!(cache.insert(1, vec![1; 4]));

        assert!(!cache.insert(2, vec![2; 9]));

        assert_eq!(cache.retained_bytes(), 4);
        assert!(cache.get(1).is_some());
    }
}
