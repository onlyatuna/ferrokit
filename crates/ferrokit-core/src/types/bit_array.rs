// SPDX-License-Identifier: LGPL-3.0-only
//! Dynamic bit array container matching Qt's `QBitArray`.

use std::fmt;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, BitXor, BitXorAssign, Not};

/// A dynamic array of bits, modeled after Qt's `QBitArray`.
///
/// Stores bits compactly using 64-bit blocks (`u64`) and supports bitwise operations,
/// resizing, counting, and individual bit inspection/mutation.
#[derive(Clone, PartialEq, Eq, Hash, Default)]
pub struct BitArray {
    len: usize,
    blocks: Vec<u64>,
}

impl BitArray {
    const BITS_PER_BLOCK: usize = 64;

    /// Creates an empty bit array with 0 bits.
    pub fn new() -> Self {
        Self {
            len: 0,
            blocks: Vec::new(),
        }
    }

    /// Creates a bit array with the specified number of bits, all initialized to `value`.
    pub fn with_size(size: usize, value: bool) -> Self {
        let num_blocks = size.div_ceil(Self::BITS_PER_BLOCK);
        let fill_word = if value { u64::MAX } else { 0 };
        let mut blocks = vec![fill_word; num_blocks];

        // Mask off unused high bits in the last block
        if value && size > 0 {
            let rem = size % Self::BITS_PER_BLOCK;
            if rem != 0 {
                let mask = (1u64 << rem) - 1;
                if let Some(last) = blocks.last_mut() {
                    *last &= mask;
                }
            }
        }

        Self { len: size, blocks }
    }

    /// Number of bits held; the same as [`size`](Self::size).
    pub fn len(&self) -> usize {
        self.len
    }

    /// Number of bits held (`QBitArray::size`).
    pub fn size(&self) -> usize {
        self.len
    }

    /// Returns `true` if the bit array contains 0 bits, matching `QBitArray::isEmpty`.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns `true` if the bit at `index` is 1, matching `QBitArray::testBit`.
    pub fn test_bit(&self, index: usize) -> bool {
        assert!(
            index < self.len,
            "bit index out of bounds: {} >= {}",
            index,
            self.len
        );
        let block_idx = index / Self::BITS_PER_BLOCK;
        let bit_idx = index % Self::BITS_PER_BLOCK;
        (self.blocks[block_idx] & (1u64 << bit_idx)) != 0
    }

    /// Alias for [`test_bit`], matching `QBitArray::at`.
    pub fn at(&self, index: usize) -> bool {
        self.test_bit(index)
    }

    /// Sets the bit at `index` to `value`, matching `QBitArray::setBit`.
    pub fn set_bit(&mut self, index: usize, value: bool) {
        assert!(
            index < self.len,
            "bit index out of bounds: {} >= {}",
            index,
            self.len
        );
        let block_idx = index / Self::BITS_PER_BLOCK;
        let bit_idx = index % Self::BITS_PER_BLOCK;
        if value {
            self.blocks[block_idx] |= 1u64 << bit_idx;
        } else {
            self.blocks[block_idx] &= !(1u64 << bit_idx);
        }
    }

    /// Clears the bit at `index` (sets to 0), matching `QBitArray::clearBit`.
    pub fn clear_bit(&mut self, index: usize) {
        self.set_bit(index, false);
    }

    /// Inverts the bit at `index` and returns the new value, matching `QBitArray::toggleBit`.
    pub fn toggle_bit(&mut self, index: usize) -> bool {
        let current = self.test_bit(index);
        let new_val = !current;
        self.set_bit(index, new_val);
        new_val
    }

    /// Counts the number of bits set to `value` (1s or 0s), matching `QBitArray::count`.
    pub fn count(&self, value: bool) -> usize {
        let ones: usize = self.blocks.iter().map(|b| b.count_ones() as usize).sum();
        if value {
            ones
        } else {
            self.len - ones
        }
    }

    /// Fills all bits in the array with `value`, matching `QBitArray::fill`.
    pub fn fill(&mut self, value: bool) {
        let fill_word = if value { u64::MAX } else { 0 };
        self.blocks.fill(fill_word);
        self.sanitize_high_bits();
    }

    /// Resizes the bit array to `new_size`, initializing any added bits with `value`,
    /// matching `QBitArray::resize`.
    pub fn resize(&mut self, new_size: usize, value: bool) {
        let num_blocks = new_size.div_ceil(Self::BITS_PER_BLOCK);
        let old_size = self.len;

        if new_size > old_size {
            let fill_word = if value { u64::MAX } else { 0 };
            self.blocks.resize(num_blocks, fill_word);
            // If new bits start within an existing block, fill remaining bits
            if value && !old_size.is_multiple_of(Self::BITS_PER_BLOCK) {
                let start_block = old_size / Self::BITS_PER_BLOCK;
                let start_bit = old_size % Self::BITS_PER_BLOCK;
                let mask = !((1u64 << start_bit) - 1);
                self.blocks[start_block] |= mask;
            }
        } else {
            self.blocks.truncate(num_blocks);
        }

        self.len = new_size;
        self.sanitize_high_bits();
    }

    /// Truncates the array to `pos` bits, matching `QBitArray::truncate`.
    pub fn truncate(&mut self, pos: usize) {
        if pos < self.len {
            self.resize(pos, false);
        }
    }

    /// Clears the bit array, resetting size to 0, matching `QBitArray::clear`.
    pub fn clear(&mut self) {
        self.len = 0;
        self.blocks.clear();
    }

    fn sanitize_high_bits(&mut self) {
        if self.len > 0 {
            let rem = self.len % Self::BITS_PER_BLOCK;
            if rem != 0 {
                let mask = (1u64 << rem) - 1;
                if let Some(last) = self.blocks.last_mut() {
                    *last &= mask;
                }
            }
        }
    }
}

impl BitAnd for BitArray {
    type Output = BitArray;

    fn bitand(mut self, rhs: Self) -> Self::Output {
        self &= rhs;
        self
    }
}

impl BitAndAssign for BitArray {
    fn bitand_assign(&mut self, rhs: Self) {
        assert_eq!(
            self.len, rhs.len,
            "BitArray lengths must match for bitwise AND"
        );
        for (a, b) in self.blocks.iter_mut().zip(rhs.blocks.iter()) {
            *a &= *b;
        }
    }
}

impl BitOr for BitArray {
    type Output = BitArray;

    fn bitor(mut self, rhs: Self) -> Self::Output {
        self |= rhs;
        self
    }
}

impl BitOrAssign for BitArray {
    fn bitor_assign(&mut self, rhs: Self) {
        assert_eq!(
            self.len, rhs.len,
            "BitArray lengths must match for bitwise OR"
        );
        for (a, b) in self.blocks.iter_mut().zip(rhs.blocks.iter()) {
            *a |= *b;
        }
    }
}

impl BitXor for BitArray {
    type Output = BitArray;

    fn bitxor(mut self, rhs: Self) -> Self::Output {
        self ^= rhs;
        self
    }
}

impl BitXorAssign for BitArray {
    fn bitxor_assign(&mut self, rhs: Self) {
        assert_eq!(
            self.len, rhs.len,
            "BitArray lengths must match for bitwise XOR"
        );
        for (a, b) in self.blocks.iter_mut().zip(rhs.blocks.iter()) {
            *a ^= *b;
        }
    }
}

impl Not for BitArray {
    type Output = BitArray;

    fn not(mut self) -> Self::Output {
        for b in &mut self.blocks {
            *b = !*b;
        }
        self.sanitize_high_bits();
        self
    }
}

impl fmt::Debug for BitArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BitArray(len={}, bits=\"", self.len)?;
        for i in 0..self.len {
            write!(f, "{}", if self.test_bit(i) { '1' } else { '0' })?;
        }
        write!(f, "\")")
    }
}

impl fmt::Display for BitArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for i in 0..self.len {
            write!(f, "{}", if self.test_bit(i) { '1' } else { '0' })?;
        }
        Ok(())
    }
}
