#![no_std]

use allocator::{AllocError, AllocResult, BaseAllocator, ByteAllocator, PageAllocator};
use core::{alloc::Layout, ptr::NonNull};

/// Bytes grow upwards, pages downwards. Byte storage is reclaimed once all
/// byte allocations are freed; pages are permanent during early boot.
pub struct EarlyAllocator<const SIZE: usize> {
    start: usize,
    end: usize,
    b_pos: usize,
    p_pos: usize,
    count: usize,
}

impl<const SIZE: usize> EarlyAllocator<SIZE> {
    pub const fn new() -> Self {
        Self {
            start: 0,
            end: 0,
            b_pos: 0,
            p_pos: 0,
            count: 0,
        }
    }
}

impl<const SIZE: usize> BaseAllocator for EarlyAllocator<SIZE> {
    fn init(&mut self, start: usize, size: usize) {
        assert!(SIZE.is_power_of_two());
        self.end = start.checked_add(size).expect("allocator region overflow");
        self.start = start;
        self.b_pos = start;
        self.p_pos = self.end;
        self.count = 0;
    }

    fn add_memory(&mut self, start: usize, size: usize) -> AllocResult {
        let end = start.checked_add(size).ok_or(AllocError::InvalidParam)?;
        if start < self.end && end > self.start {
            return Err(AllocError::MemoryOverlap);
        }
        // A bump allocator can only extend its single contiguous free range.
        if start == self.end && self.p_pos == self.end {
            self.end = end;
            self.p_pos = end;
            Ok(())
        } else {
            Err(AllocError::NoMemory)
        }
    }
}

impl<const SIZE: usize> ByteAllocator for EarlyAllocator<SIZE> {
    fn alloc(&mut self, layout: Layout) -> AllocResult<NonNull<u8>> {
        let start = self
            .b_pos
            .checked_add(layout.align() - 1)
            .ok_or(AllocError::NoMemory)?
            & !(layout.align() - 1);
        let end = start
            .checked_add(layout.size().max(1))
            .ok_or(AllocError::NoMemory)?;
        if end > self.p_pos {
            return Err(AllocError::NoMemory);
        }
        let ptr = NonNull::new(start as *mut u8).ok_or(AllocError::InvalidParam)?;
        self.b_pos = end;
        self.count += 1;
        Ok(ptr)
    }

    fn dealloc(&mut self, _pos: NonNull<u8>, _layout: Layout) {
        assert!(self.count > 0, "unbalanced byte deallocation");
        self.count -= 1;
        if self.count == 0 {
            self.b_pos = self.start;
        }
    }

    fn total_bytes(&self) -> usize {
        self.p_pos - self.start
    }
    fn used_bytes(&self) -> usize {
        self.b_pos - self.start
    }
    fn available_bytes(&self) -> usize {
        self.p_pos - self.b_pos
    }
}

impl<const SIZE: usize> PageAllocator for EarlyAllocator<SIZE> {
    const PAGE_SIZE: usize = SIZE;

    fn alloc_pages(&mut self, num_pages: usize, align_pow2: usize) -> AllocResult<usize> {
        if num_pages == 0 || !align_pow2.is_power_of_two() || align_pow2 < SIZE {
            return Err(AllocError::InvalidParam);
        }
        let bytes = num_pages.checked_mul(SIZE).ok_or(AllocError::NoMemory)?;
        let pos = self.p_pos.checked_sub(bytes).ok_or(AllocError::NoMemory)? & !(align_pow2 - 1);
        if pos < self.b_pos {
            return Err(AllocError::NoMemory);
        }
        self.p_pos = pos;
        Ok(pos)
    }

    fn dealloc_pages(&mut self, _pos: usize, _num_pages: usize) {
        // Early boot page allocations intentionally live for the whole boot.
    }

    fn total_pages(&self) -> usize {
        (self.end - self.start) / SIZE
    }
    fn used_pages(&self) -> usize {
        (self.end - self.p_pos + SIZE - 1) / SIZE
    }
    fn available_pages(&self) -> usize {
        let start = self.b_pos.saturating_add(SIZE - 1) & !(SIZE - 1);
        (self.p_pos & !(SIZE - 1)).saturating_sub(start) / SIZE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alignment_collision_and_reclaim() {
        let mut allocator = EarlyAllocator::<4096>::new();
        allocator.init(0x1003, 0x10000);
        let layout = Layout::from_size_align(17, 64).unwrap();
        let first = allocator.alloc(layout).unwrap();
        let second = allocator.alloc(layout).unwrap();
        assert_eq!(first.as_ptr() as usize % 64, 0);
        assert_ne!(first, second);
        let page = allocator.alloc_pages(2, 8192).unwrap();
        assert_eq!(page % 8192, 0);
        assert!(page >= second.as_ptr() as usize + layout.size());
        assert!(allocator
            .alloc(Layout::from_size_align(0x10000, 8).unwrap())
            .is_err());
        allocator.dealloc(first, layout);
        assert!(allocator.used_bytes() > 0);
        allocator.dealloc(second, layout);
        assert_eq!(allocator.used_bytes(), 0);
        assert_eq!(allocator.alloc(layout).unwrap(), first);
        let used_pages = allocator.used_pages();
        allocator.dealloc_pages(page, 2);
        assert_eq!(allocator.used_pages(), used_pages);
    }

    #[test]
    fn invalid_and_overflow_requests_do_not_consume_memory() {
        let mut allocator = EarlyAllocator::<4096>::new();
        allocator.init(0x1000, 0x8000);
        for (count, align) in [(0, 4096), (1, 3), (1, 2048), (usize::MAX, 4096)] {
            assert!(allocator.alloc_pages(count, align).is_err());
        }
        assert_eq!(allocator.used_pages(), 0);
        assert!(allocator.add_memory(0x2000, 4096).is_err());
        allocator.add_memory(0x9000, 4096).unwrap();
        assert_eq!(allocator.total_bytes(), 0x9000);
    }
}
