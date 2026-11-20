use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, AddAssign, Neg, Sub, SubAssign};

use crate::constraint::Weight;

/// 三级分数。各级别都是罚分之和（<= 0），按 hard、medium、soft 依次比较，越大越好。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Score {
    pub hard: i64,
    pub medium: i64,
    pub soft: i64,
}

impl Score {
    pub const ZERO: Score = Score {
        hard: 0,
        medium: 0,
        soft: 0,
    };

    /// 匹配量乘权重后的罚分。
    #[inline]
    pub fn penalty(amount: i64, weight: Weight) -> Score {
        Score {
            hard: -(amount * weight.hard),
            medium: -(amount * weight.medium),
            soft: -(amount * weight.soft),
        }
    }

    pub fn is_feasible(self) -> bool {
        self.hard == 0
    }
}

impl Ord for Score {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.hard, self.medium, self.soft).cmp(&(other.hard, other.medium, other.soft))
    }
}

impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Add for Score {
    type Output = Score;
    #[inline]
    fn add(self, o: Score) -> Score {
        Score {
            hard: self.hard + o.hard,
            medium: self.medium + o.medium,
            soft: self.soft + o.soft,
        }
    }
}

impl Sub for Score {
    type Output = Score;
    #[inline]
    fn sub(self, o: Score) -> Score {
        Score {
            hard: self.hard - o.hard,
            medium: self.medium - o.medium,
            soft: self.soft - o.soft,
        }
    }
}

impl Neg for Score {
    type Output = Score;
    #[inline]
    fn neg(self) -> Score {
        Score {
            hard: -self.hard,
            medium: -self.medium,
            soft: -self.soft,
        }
    }
}

impl AddAssign for Score {
    #[inline]
    fn add_assign(&mut self, o: Score) {
        *self = *self + o;
    }
}

impl SubAssign for Score {
    #[inline]
    fn sub_assign(&mut self, o: Score) {
        *self = *self - o;
    }
}

impl std::iter::Sum for Score {
    fn sum<I: Iterator<Item = Score>>(iter: I) -> Score {
        iter.fold(Score::ZERO, Add::add)
    }
}

/// 与 v1 相同的文本格式，例如 `0hard/-5medium/-79909soft`。
impl fmt::Display for Score {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}hard/{}medium/{}soft",
            self.hard, self.medium, self.soft
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_lexicographic() {
        let a = Score {
            hard: 0,
            medium: -10,
            soft: -1_000_000,
        };
        let b = Score {
            hard: -1,
            medium: 0,
            soft: 0,
        };
        let c = Score {
            hard: 0,
            medium: -10,
            soft: -5,
        };
        assert!(a > b);
        assert!(c > a);
        assert_eq!(a.to_string(), "0hard/-10medium/-1000000soft");
    }

    #[test]
    fn penalty_multiplies_each_level() {
        let s = Score::penalty(
            3,
            Weight {
                hard: 1,
                medium: 2,
                soft: 5,
            },
        );
        assert_eq!(
            s,
            Score {
                hard: -3,
                medium: -6,
                soft: -15
            }
        );
    }
}
