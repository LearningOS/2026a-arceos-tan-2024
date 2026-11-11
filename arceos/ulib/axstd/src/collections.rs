//! Allocation-backed collections, including a hash table for no_std kernels.
pub use alloc::collections::*;
use alloc::vec::Vec;
use core::{
    borrow::Borrow,
    hash::{Hash, Hasher},
};

// FNV-1a needs neither an operating-system random source nor extra crates.
struct FnvHasher(u64);
impl Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= *byte as u64;
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
}

fn hash<Q: Hash + ?Sized>(key: &Q) -> usize {
    let mut hasher = FnvHasher(0xcbf29ce484222325);
    key.hash(&mut hasher);
    hasher.finish() as usize
}

/// A growing hash table with separate chaining for colliding keys.
/// Its deterministic hasher is suitable for trusted kernel application keys.
pub struct HashMap<K, V> {
    buckets: Vec<Vec<(K, V)>>,
    len: usize,
}

impl<K, V> HashMap<K, V> {
    pub const fn new() -> Self {
        Self {
            buckets: Vec::new(),
            len: 0,
        }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn clear(&mut self) {
        for bucket in &mut self.buckets {
            bucket.clear();
        }
        self.len = 0;
    }
    pub fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.buckets
            .iter()
            .flat_map(|b| b.iter().map(|(k, v)| (k, v)))
    }
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&K, &mut V)> {
        self.buckets
            .iter_mut()
            .flat_map(|b| b.iter_mut().map(|(k, v)| (&*k, v)))
    }
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.iter().map(|(k, _)| k)
    }
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.iter().map(|(_, v)| v)
    }
}

impl<K: Eq + Hash, V> HashMap<K, V> {
    pub fn with_capacity(capacity: usize) -> Self {
        let mut map = Self::new();
        if capacity != 0 {
            map.resize(capacity.max(16).next_power_of_two());
        }
        map
    }
    fn resize(&mut self, size: usize) {
        let mut buckets = Vec::with_capacity(size);
        buckets.resize_with(size, Vec::new);
        for bucket in core::mem::replace(&mut self.buckets, buckets) {
            for (key, value) in bucket {
                let index = hash(&key) & (size - 1);
                self.buckets[index].push((key, value));
            }
        }
    }
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        if self.buckets.is_empty() {
            self.resize(16);
        }
        let index = hash(&key) & (self.buckets.len() - 1);
        if let Some((_, old)) = self.buckets[index].iter_mut().find(|(k, _)| k == &key) {
            return Some(core::mem::replace(old, value));
        }
        if self.len >= self.buckets.len() {
            self.resize(
                self.buckets
                    .len()
                    .checked_mul(2)
                    .expect("hash table too large"),
            );
        }
        let index = hash(&key) & (self.buckets.len() - 1);
        self.buckets[index].push((key, value));
        self.len += 1;
        None
    }
    pub fn get<Q: Eq + Hash + ?Sized>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
    {
        if self.buckets.is_empty() {
            return None;
        }
        self.buckets[hash(key) & (self.buckets.len() - 1)]
            .iter()
            .find(|(k, _)| k.borrow() == key)
            .map(|(_, v)| v)
    }
    pub fn get_mut<Q: Eq + Hash + ?Sized>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
    {
        if self.buckets.is_empty() {
            return None;
        }
        let index = hash(key) & (self.buckets.len() - 1);
        self.buckets[index]
            .iter_mut()
            .find(|(k, _)| k.borrow() == key)
            .map(|(_, v)| v)
    }
    pub fn contains_key<Q: Eq + Hash + ?Sized>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
    {
        self.get(key).is_some()
    }
    pub fn remove<Q: Eq + Hash + ?Sized>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
    {
        if self.buckets.is_empty() {
            return None;
        }
        let index = hash(key) & (self.buckets.len() - 1);
        let bucket = &mut self.buckets[index];
        let pos = bucket.iter().position(|(k, _)| k.borrow() == key)?;
        self.len -= 1;
        Some(bucket.swap_remove(pos).1)
    }
}

impl<K, V> Default for HashMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;

    #[test]
    fn grow_update_borrow_and_remove() {
        let mut map = HashMap::new();
        for i in 0..50_000 {
            assert_eq!(map.insert(alloc::format!("key_{i}"), i), None);
        }
        assert_eq!(map.len(), 50_000);
        assert_eq!(map.insert(String::from("key_17"), 99), Some(17));
        assert_eq!(map.get("key_17"), Some(&99));
        *map.get_mut("key_17").unwrap() = 100;
        assert_eq!(map.remove("key_17"), Some(100));
        assert!(!map.contains_key("key_17"));
        assert_eq!(map.len(), 49_999);
        assert_eq!(map.iter().count(), map.len());
        map.clear();
        assert!(map.is_empty());
    }

    #[derive(Eq, PartialEq)]
    struct Collision(u32);
    impl Hash for Collision {
        fn hash<H: Hasher>(&self, hasher: &mut H) {
            hasher.write_u8(0);
        }
    }

    #[test]
    fn colliding_keys_remain_distinct() {
        let mut map = HashMap::new();
        for i in 0..64 {
            map.insert(Collision(i), i);
        }
        for i in 0..64 {
            assert_eq!(map.get(&Collision(i)), Some(&i));
        }
        assert_eq!(map.remove(&Collision(23)), Some(23));
        assert_eq!(map.get(&Collision(24)), Some(&24));
        assert_eq!(map.insert(Collision(24), 99), Some(24));
    }
}
