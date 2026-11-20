/// xoshiro256++，用 splitmix64 从种子展开状态。确定性、无依赖。
#[derive(Clone, Debug)]
pub struct Rng {
    s: [u64; 4],
}

impl Rng {
    pub fn new(seed: u64) -> Rng {
        let mut sm = seed;
        let mut next = || {
            sm = sm.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = sm;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        };
        Rng {
            s: [next(), next(), next(), next()],
        }
    }

    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.s;
        let result = s[0].wrapping_add(s[3]).rotate_left(23).wrapping_add(s[0]);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// [0, n) 内的均匀整数（乘法映射，偏差可忽略）。`n` 必须大于 0。
    pub fn below(&mut self, n: usize) -> usize {
        debug_assert!(n > 0);
        ((self.next_u64() as u128 * n as u128) >> 64) as usize
    }

    /// [lo, hi] 内的整数。
    pub fn range(&mut self, lo: i64, hi: i64) -> i64 {
        debug_assert!(lo <= hi);
        lo + self.below((hi - lo + 1) as usize) as i64
    }

    /// 以 num/den 的概率返回 true。
    pub fn chance(&mut self, num: usize, den: usize) -> bool {
        self.below(den) < num
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn deterministic_for_same_seed() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert!((0..1000).all(|_| a.below(10) < 10));
    }
}
