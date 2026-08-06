//! Litematica 的 `LitematicaBitArray`：**跨 long 边界**的定宽位打包。
//!
//! 注意这跟 MC 1.16+ 的区块 palette 打包不是一回事——后者每个 long 内部
//! 不跨边界、末尾留空位。搞混会读出完全错误的方块。
//!
//! 已用 2138 个真实蓝图验证：`longs == ceil(volume * bits / 64)` 对全部样本成立，
//! 而 `ceil(volume / floor(64/bits))`（不跨边界）无一独有匹配。

/// 按调色板大小推 bits/entry，对齐 Litematica 的算法：
/// `max(2, 32 - leading_zeros(palette_size - 1))`
pub fn bits_per_entry(palette_size: usize) -> u32 {
    if palette_size <= 1 {
        return 2;
    }
    let n = (palette_size - 1) as u32;
    (32 - n.leading_zeros()).max(2)
}

/// 打包 `count` 个 `bits` 位条目所需的 long 数。
pub fn required_longs(count: u64, bits: u32) -> usize {
    ((count * bits as u64 + 63) / 64) as usize
}

pub struct BitArray<'a> {
    longs: &'a [i64],
    bits: u32,
    mask: u64,
}

impl<'a> BitArray<'a> {
    pub fn new(longs: &'a [i64], bits: u32) -> Self {
        debug_assert!((1..=32).contains(&bits));
        let mask = if bits >= 64 { u64::MAX } else { (1u64 << bits) - 1 };
        Self { longs, bits, mask }
    }

    #[inline]
    pub fn get(&self, index: u64) -> u32 {
        let bits = self.bits as u64;
        let start_offset = index * bits;
        let start_idx = (start_offset >> 6) as usize;
        let end_idx = (((index + 1) * bits - 1) >> 6) as usize;
        let start_bit = (start_offset & 63) as u32;

        if start_idx >= self.longs.len() {
            return 0;
        }
        let lo = self.longs[start_idx] as u64;
        if start_idx == end_idx {
            ((lo >> start_bit) & self.mask) as u32
        } else {
            if end_idx >= self.longs.len() {
                return ((lo >> start_bit) & self.mask) as u32;
            }
            let hi = self.longs[end_idx] as u64;
            let end_shift = 64 - start_bit;
            (((lo >> start_bit) | (hi << end_shift)) & self.mask) as u32
        }
    }

    /// 顺序遍历前 `count` 个条目。比逐个 `get` 快——省掉重复的除法与边界判断。
    pub fn for_each(&self, count: u64, mut f: impl FnMut(u64, u32)) {
        let bits = self.bits as u64;
        let n = self.longs.len();
        let mut bit_pos: u64 = 0;
        for i in 0..count {
            let start_idx = (bit_pos >> 6) as usize;
            if start_idx >= n {
                break;
            }
            let start_bit = (bit_pos & 63) as u32;
            let lo = self.longs[start_idx] as u64;
            let v = if start_bit as u64 + bits <= 64 {
                (lo >> start_bit) & self.mask
            } else {
                let end_idx = start_idx + 1;
                if end_idx >= n {
                    (lo >> start_bit) & self.mask
                } else {
                    let hi = self.longs[end_idx] as u64;
                    ((lo >> start_bit) | (hi << (64 - start_bit))) & self.mask
                }
            };
            f(i, v as u32);
            bit_pos += bits;
        }
    }

    /// 从任意位置开始遍历 `count` 个条目。
    ///
    /// Litematica 的索引顺序是 y 最外层，所以「只要第 y 层」可以直接算出
    /// 索引区间，不必扫全部方块——对 5 亿体积的蓝图这是 600 倍的差距。
    pub fn for_each_range(&self, start: u64, count: u64, mut f: impl FnMut(u64, u32)) {
        let bits = self.bits as u64;
        let n = self.longs.len();
        let mut bit_pos: u64 = start * bits;
        for i in 0..count {
            let start_idx = (bit_pos >> 6) as usize;
            if start_idx >= n {
                break;
            }
            let start_bit = (bit_pos & 63) as u32;
            let lo = self.longs[start_idx] as u64;
            let v = if start_bit as u64 + bits <= 64 {
                (lo >> start_bit) & self.mask
            } else {
                let end_idx = start_idx + 1;
                if end_idx >= n {
                    (lo >> start_bit) & self.mask
                } else {
                    let hi = self.longs[end_idx] as u64;
                    ((lo >> start_bit) | (hi << (64 - start_bit))) & self.mask
                }
            };
            f(start + i, v as u32);
            bit_pos += bits;
        }
    }

    /// 只要调色板索引的直方图（材料清单用）。不需要知道方块在哪，
    /// 因此内存 O(palette)，可以处理 5 亿体积的巨型蓝图。
    pub fn histogram(&self, count: u64, palette_size: usize) -> Vec<u64> {
        let mut hist = vec![0u64; palette_size.max(1)];
        self.for_each(count, |_, v| {
            let v = v as usize;
            if v < hist.len() {
                hist[v] += 1;
            }
        });
        hist
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_matches_litematica() {
        // 对照真实蓝图实测值
        assert_eq!(bits_per_entry(2), 2); // 石头1xn
        assert_eq!(bits_per_entry(30), 5); // 垃圾收集加分类
        assert_eq!(bits_per_entry(172), 8); // 2075k双模式刷石机
        assert_eq!(bits_per_entry(255), 8); // 流萤
        assert_eq!(bits_per_entry(1233), 11); // 璃月港
        assert_eq!(bits_per_entry(525), 10); // 故宫角楼
        assert_eq!(bits_per_entry(4828), 13); // 实测最大调色板
        // 边界：恰好 2 的幂不应多占一位
        assert_eq!(bits_per_entry(16), 4);
        assert_eq!(bits_per_entry(17), 5);
        assert_eq!(bits_per_entry(1), 2);
    }

    #[test]
    fn long_count_matches_real_files() {
        assert_eq!(required_longs(19, 2), 1); // 石头1xn
        assert_eq!(required_longs(1470, 5), 115); // 垃圾收集加分类
        assert_eq!(required_longs(121_835, 8), 15_230); // 2075k刷石机
        assert_eq!(required_longs(515_206_048, 8), 64_400_756); // 流萤
        assert_eq!(required_longs(333_491_800, 11), 57_318_904); // 璃月港
        assert_eq!(required_longs(301_866_180, 10), 47_166_591); // 故宫角楼
    }

    /// 用跨 long 边界的方式打包，再读回来，必须一致。
    fn roundtrip(values: &[u32], bits: u32) {
        let total_bits = values.len() as u64 * bits as u64;
        let mut longs = vec![0i64; ((total_bits + 63) / 64) as usize];
        for (i, &v) in values.iter().enumerate() {
            let start = i as u64 * bits as u64;
            let idx = (start >> 6) as usize;
            let off = (start & 63) as u32;
            let val = v as u64;
            longs[idx] |= ((val << off) as i64) as i64;
            if off as u64 + bits as u64 > 64 {
                longs[idx + 1] |= (val >> (64 - off)) as i64;
            }
        }
        let ba = BitArray::new(&longs, bits);
        for (i, &v) in values.iter().enumerate() {
            assert_eq!(ba.get(i as u64), v, "get 不一致 @{i} bits={bits}");
        }
        let mut seq = Vec::new();
        ba.for_each(values.len() as u64, |_, v| seq.push(v));
        assert_eq!(seq, values, "for_each 与 get 不一致 bits={bits}");
    }

    #[test]
    fn cross_boundary_roundtrip() {
        // 5 位是最常见的 bits（实测 599 个 region），且必然跨边界
        for bits in [2u32, 3, 4, 5, 6, 7, 8, 9, 10, 11, 13] {
            let max = (1u64 << bits) - 1;
            let vals: Vec<u32> = (0..500).map(|i| ((i * 7 + 3) as u64 % (max + 1)) as u32).collect();
            roundtrip(&vals, bits);
        }
    }

    #[test]
    fn range_matches_full_scan() {
        let bits = 5u32;
        let vals: Vec<u32> = (0..300).map(|i| (i * 11 % 32) as u32).collect();
        let total_bits = vals.len() as u64 * bits as u64;
        let mut longs = vec![0i64; ((total_bits + 63) / 64) as usize];
        for (i, &v) in vals.iter().enumerate() {
            let start = i as u64 * bits as u64;
            let idx = (start >> 6) as usize;
            let off = (start & 63) as u32;
            longs[idx] |= ((v as u64) << off) as i64;
            if off as u64 + bits as u64 > 64 {
                longs[idx + 1] |= ((v as u64) >> (64 - off)) as i64;
            }
        }
        let ba = BitArray::new(&longs, bits);
        // 任取一段，逐项与全量扫描的结果对齐（含索引）
        for (start, count) in [(0u64, 300u64), (7, 50), (64, 100), (299, 1), (250, 50)] {
            let mut got = Vec::new();
            ba.for_each_range(start, count, |i, v| got.push((i, v)));
            let want: Vec<(u64, u32)> = (start..start + count)
                .map(|i| (i, vals[i as usize]))
                .collect();
            assert_eq!(got, want, "range({start},{count}) 不一致");
        }
    }

    #[test]
    fn handles_truncated_data() {
        // 数据被截断时不应 panic
        let longs = vec![0i64; 2];
        let ba = BitArray::new(&longs, 5);
        for i in 0..1000 {
            let _ = ba.get(i);
        }
        let mut n = 0;
        ba.for_each(1000, |_, _| n += 1);
        assert!(n <= 1000);
    }
}
