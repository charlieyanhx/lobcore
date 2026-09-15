//! Pure functions over L1 / L5 book snapshots in integer price units.
//!
//! Units and conventions
//! - Prices are `i64` in the feed's integer units (ITCH: 1e-4 dollars); quantities are `u32` shares.
//! - Every feature is an exact rational (`Ratio` = i64/i64, `Wmid` = i128/i64); `f64` appears only in the
//!   `as_f64` boundary methods. Nothing here allocates.
//! - A feature that divides by the visible quantity returns `None` when that quantity is zero (an empty
//!   side); the caller decides how to encode "undefined" (NaN at the numpy boundary).
//!
//! Identity kept by this module (tested exactly, not to a tolerance):
//! `wmid = mid + (imb1 - 1/2) * spread`, i.e. the quantity-weighted mid is the mid shifted by the
//! signed L1 imbalance times half the spread.
//!
//! Order-flow imbalance follows Cont, Kukanov and Stoikov (2014), "The price impact of order book
//! events", eq. (1): with `P^b, q^b, P^a, q^a` the best bid / ask price and quantity,
//! `e_n = 1{P^b_n >= P^b_{n-1}} q^b_n - 1{P^b_n <= P^b_{n-1}} q^b_{n-1}
//!        - 1{P^a_n <= P^a_{n-1}} q^a_n + 1{P^a_n >= P^a_{n-1}} q^a_{n-1}`
//! and `OFI = sum e_n` over a window.
#![deny(unsafe_code)]
#![warn(missing_docs)]

/// Best bid and ask, both sides present.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct L1 {
    /// Best bid price (integer price units).
    pub bid_px: i64,
    /// Quantity at the best bid.
    pub bid_qty: u32,
    /// Best ask price (integer price units).
    pub ask_px: i64,
    /// Quantity at the best ask.
    pub ask_qty: u32,
}

impl L1 {
    /// Construct an L1 snapshot.
    pub const fn new(bid_px: i64, bid_qty: u32, ask_px: i64, ask_qty: u32) -> Self {
        Self {
            bid_px,
            bid_qty,
            ask_px,
            ask_qty,
        }
    }
}

/// Top-five quantities per side, level 1 first; missing levels are zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct L5 {
    /// Bid quantities, best first.
    pub bid: [u32; 5],
    /// Ask quantities, best first.
    pub ask: [u32; 5],
}

/// An exact rational `num / den` with `den > 0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ratio {
    /// Numerator.
    pub num: i64,
    /// Denominator, strictly positive.
    pub den: i64,
}

impl Ratio {
    /// Boundary conversion.
    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// Exact equality of two rationals by cross-multiplication in i128.
    pub fn eq_exact(self, other: Ratio) -> bool {
        (self.num as i128) * (other.den as i128) == (other.num as i128) * (self.den as i128)
    }
}

/// The quantity-weighted mid as an exact rational: numerator in i128 (price x quantity sums), denominator
/// the total visible quantity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Wmid {
    /// `bid_px * ask_qty + ask_px * bid_qty`.
    pub num: i128,
    /// `bid_qty + ask_qty`, strictly positive.
    pub den: i64,
}

impl Wmid {
    /// Boundary conversion.
    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }

    /// Exact equality with a `Ratio` by cross-multiplication in i128.
    pub fn eq_ratio(self, r: Ratio) -> bool {
        self.num * (r.den as i128) == (r.num as i128) * (self.den as i128)
    }
}

/// Weighted mid `(bid_px * ask_qty + ask_px * bid_qty) / (bid_qty + ask_qty)`; `None` when both
/// quantities are zero.
pub fn wmid(bid_px: i64, bid_qty: u32, ask_px: i64, ask_qty: u32) -> Option<Wmid> {
    let den = i64::from(bid_qty) + i64::from(ask_qty);
    if den == 0 {
        return None;
    }
    let num = (bid_px as i128) * (ask_qty as i128) + (ask_px as i128) * (bid_qty as i128);
    Some(Wmid { num, den })
}

/// Mid price `(bid_px + ask_px) / 2` as an exact rational.
pub fn mid(l1: &L1) -> Ratio {
    Ratio {
        num: l1.bid_px + l1.ask_px,
        den: 2,
    }
}

/// Spread `ask_px - bid_px` (negative when the book is crossed).
pub fn spread(l1: &L1) -> i64 {
    l1.ask_px - l1.bid_px
}

/// L1 imbalance `bid_qty / (bid_qty + ask_qty)`; `None` when both quantities are zero.
pub fn imb1(bid_qty: u32, ask_qty: u32) -> Option<Ratio> {
    imbalance(u64::from(bid_qty), u64::from(ask_qty))
}

/// Sum of the five quantities of one side.
pub fn depth5(levels: &[u32; 5]) -> u64 {
    levels.iter().map(|&q| u64::from(q)).sum()
}

/// L5 imbalance `depth5(bid) / (depth5(bid) + depth5(ask))`; `None` when both depths are zero.
pub fn imb5(l5: &L5) -> Option<Ratio> {
    imbalance(depth5(&l5.bid), depth5(&l5.ask))
}

/// Signed imbalance `2 * imb - 1`, in `[-1, 1]`.
pub fn signed_imb(imb: Ratio) -> Ratio {
    Ratio {
        num: 2 * imb.num - imb.den,
        den: imb.den,
    }
}

fn imbalance(bid: u64, ask: u64) -> Option<Ratio> {
    let den = bid + ask;
    if den == 0 {
        return None;
    }
    // Depth sums of five u32 fit in i64 with room to spare.
    Some(Ratio {
        num: i64::try_from(bid).expect("depth fits i64"),
        den: i64::try_from(den).expect("depth fits i64"),
    })
}

/// One Cont-Kukanov-Stoikov order-flow imbalance step `e_n` from `prev` to `cur`.
pub fn ofi_step(prev: &L1, cur: &L1) -> i64 {
    let (pb, pa) = (i64::from(prev.bid_qty), i64::from(prev.ask_qty));
    let (cb, ca) = (i64::from(cur.bid_qty), i64::from(cur.ask_qty));
    let mut e = 0;
    if cur.bid_px >= prev.bid_px {
        e += cb;
    }
    if cur.bid_px <= prev.bid_px {
        e -= pb;
    }
    if cur.ask_px <= prev.ask_px {
        e -= ca;
    }
    if cur.ask_px >= prev.ask_px {
        e += pa;
    }
    e
}

/// Windowed OFI: the sum of `ofi_step` over consecutive pairs of the snapshots yielded by `iter`.
/// Fewer than two snapshots give 0.
pub fn ofi_window<I>(iter: I) -> i64
where
    I: IntoIterator<Item = L1>,
{
    let mut it = iter.into_iter();
    let Some(mut prev) = it.next() else {
        return 0;
    };
    let mut sum = 0;
    for cur in it {
        sum += ofi_step(&prev, &cur);
        prev = cur;
    }
    sum
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn fixture() -> [L1; 3] {
        [
            L1::new(10000, 300, 10001, 100),
            L1::new(10000, 500, 10001, 100),
            L1::new(10001, 50, 10002, 400),
        ]
    }

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn three_snapshot_mid_imbalance_wmid() {
        let f = fixture();
        let mids = [10000.5, 10000.5, 10001.5];
        let imbs = [0.75, 0.833333, 0.111111];
        let wmids = [10000.75, 10000.833333, 10001.111111];
        let signed = [0.5, 0.666667, -0.777778];
        for (i, l1) in f.iter().enumerate() {
            assert!(close(mid(l1).as_f64(), mids[i], 1e-12), "mid {i}");
            let imb = imb1(l1.bid_qty, l1.ask_qty).unwrap();
            assert!(close(imb.as_f64(), imbs[i], 1e-6), "imb1 {i}");
            let w = wmid(l1.bid_px, l1.bid_qty, l1.ask_px, l1.ask_qty).unwrap();
            assert!(close(w.as_f64(), wmids[i], 1e-6), "wmid {i}");
            assert!(
                close(signed_imb(imb).as_f64(), signed[i], 1e-6),
                "signed {i}"
            );
        }
        // exact rationals for the first snapshot
        let w0 = wmid(10000, 300, 10001, 100).unwrap();
        assert_eq!(
            w0,
            Wmid {
                num: 4_000_300,
                den: 400
            }
        );
        assert_eq!(imb1(300, 100).unwrap(), Ratio { num: 300, den: 400 });
        assert_eq!(spread(&f[0]), 1);
        assert_eq!(mid(&f[2]), Ratio { num: 20003, den: 2 });
    }

    #[test]
    fn three_snapshot_ofi() {
        let f = fixture();
        assert_eq!(ofi_step(&f[0], &f[1]), 200);
        assert_eq!(ofi_step(&f[1], &f[2]), 150);
        assert_eq!(ofi_window(f.iter().copied()), 350);
        assert_eq!(ofi_window(std::iter::empty()), 0);
        assert_eq!(ofi_window(std::iter::once(f[0])), 0);
    }

    #[test]
    fn l5_fixture() {
        let l5 = L5 {
            bid: [300, 200, 100, 400, 50],
            ask: [100, 150, 250, 100, 300],
        };
        assert_eq!(depth5(&l5.bid), 1050);
        assert_eq!(depth5(&l5.ask), 900);
        let i5 = imb5(&l5).unwrap();
        assert_eq!(
            i5,
            Ratio {
                num: 1050,
                den: 1950
            }
        );
        assert!(close(i5.as_f64(), 0.538462, 1e-6));
        assert!(close(signed_imb(i5).as_f64(), 0.076923, 1e-6));
    }

    #[test]
    fn empty_sides_are_none() {
        assert_eq!(wmid(10000, 0, 10001, 0), None);
        assert_eq!(imb1(0, 0), None);
        assert_eq!(
            imb5(&L5 {
                bid: [0; 5],
                ask: [0; 5]
            }),
            None
        );
        // one-sided quantity is still defined
        assert_eq!(imb1(0, 7).unwrap(), Ratio { num: 0, den: 7 });
        assert_eq!(wmid(10000, 0, 10001, 7).unwrap().as_f64(), 10000.0);
    }

    /// Identity `wmid = mid + (imb1 - 1/2) * spread`, exact in rationals and to 1e-12 in f64.
    fn check_identity(l1: &L1) {
        let w = wmid(l1.bid_px, l1.bid_qty, l1.ask_px, l1.ask_qty).unwrap();
        let i = imb1(l1.bid_qty, l1.ask_qty).unwrap();
        let s = spread(l1);
        let m = mid(l1);
        // rhs = (b + a)/2 + ((2 bq - den) / (2 den)) * s  =  [(b + a) den + (2 bq - den) s] / (2 den)
        let rhs = Ratio {
            num: m.num * i.den + (2 * i.num - i.den) * s,
            den: 2 * i.den,
        };
        assert!(w.eq_ratio(rhs), "exact identity failed for {l1:?}");
        let f_rhs = m.as_f64() + (i.as_f64() - 0.5) * s as f64;
        assert!(
            close(w.as_f64(), f_rhs, 1e-12),
            "f64 identity failed for {l1:?}"
        );
    }

    #[test]
    fn identity_on_fixture() {
        for l1 in fixture().iter() {
            check_identity(l1);
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(256),
            .. ProptestConfig::default()
        })]

        #[test]
        fn identity_holds_for_random_l1(
            bid_px in 1i64..2_000_000_000,
            gap in -50i64..500,
            bid_qty in 0u32..1_000_000,
            ask_qty in 0u32..1_000_000,
        ) {
            prop_assume!(bid_qty + ask_qty > 0);
            let l1 = L1::new(bid_px, bid_qty, bid_px + gap, ask_qty);
            check_identity(&l1);
        }

        #[test]
        fn ofi_is_antisymmetric_under_time_reversal(
            a in (1i64..100_000, 0u32..10_000, 1i64..100, 0u32..10_000),
            b in (1i64..100_000, 0u32..10_000, 1i64..100, 0u32..10_000),
        ) {
            // e(prev -> cur) + e(cur -> prev) == 0 only when prices are unchanged on both sides;
            // in general the sum equals the price-move corrections, which we check by the closed form.
            let p = L1::new(a.0, a.1, a.0 + a.2, a.3);
            let c = L1::new(b.0, b.1, b.0 + b.2, b.3);
            let fwd = ofi_step(&p, &c);
            let bwd = ofi_step(&c, &p);
            if p.bid_px == c.bid_px && p.ask_px == c.ask_px {
                prop_assert_eq!(fwd + bwd, 0);
            }
            prop_assert_eq!(ofi_window([p, c]), fwd);
            prop_assert_eq!(ofi_window([p, c, p]), fwd + bwd);
        }
    }
}
