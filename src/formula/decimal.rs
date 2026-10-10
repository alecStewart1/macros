//! Decimal rounding on Excel's 15-significant-digit view of a number.
//!
//! Excel rounds the *decimal* form of a number, not its binary value:
//! `ROUND(2.675, 2)` is 2.68 although the double nearest 2.675 is
//! 2.67499999999999982236431605997495353221893310546875. So every rounding
//! here first takes the number's 15 significant digits (what Excel shows),
//! then rounds that digit string, then parses the result back — which is the
//! double nearest the exact decimal answer, so integral results are exact.

/// A non-negative decimal: `digits` (no leading or trailing zeros) times
/// `10^(point - digits.len())`. Zero has no digits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dec {
    pub digits: Vec<u8>,
    /// How many of the digits sit before the decimal point (may be ≤ 0 or
    /// exceed `digits.len()`).
    pub point: i32,
}

/// How to treat the dropped digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Half away from zero (`ROUND`).
    Half,
    /// Away from zero when anything non-zero is dropped (`ROUNDUP`).
    Up,
    /// Toward zero (`ROUNDDOWN`, `TRUNC`).
    Down,
}

impl Dec {
    /// The 15-significant-digit decimal of `|x|`.
    pub fn from_f64(x: f64) -> Dec {
        let a = x.abs();
        if a == 0.0 || !a.is_finite() {
            return Dec {
                digits: Vec::new(),
                point: 0,
            };
        }
        let s = format!("{:.14e}", a);
        let (mant, exp) = s.split_once('e').unwrap_or((&s, "0"));
        let exp: i32 = exp.parse().unwrap_or(0);
        let digits: Vec<u8> = mant
            .bytes()
            .filter(u8::is_ascii_digit)
            .map(|b| b - b'0')
            .collect();
        let mut d = Dec {
            digits,
            point: exp + 1,
        };
        d.normalize();
        d
    }

    fn normalize(&mut self) {
        while self.digits.last() == Some(&0) {
            self.digits.pop();
        }
        while self.digits.first() == Some(&0) {
            self.digits.remove(0);
            self.point -= 1;
        }
        if self.digits.is_empty() {
            self.point = 0;
        }
    }

    pub fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    /// Round to `nd` decimal places (negative: to tens, hundreds, …).
    pub fn round(&self, nd: i32, mode: Mode) -> Dec {
        let len = self.digits.len() as i32;
        let keep = self.point.saturating_add(nd);
        if keep >= len || self.is_zero() {
            return self.clone();
        }
        let up = match mode {
            Mode::Down => false,
            Mode::Up => true, // something non-zero is dropped (no trailing zeros)
            Mode::Half => keep >= 0 && self.digits[keep as usize] >= 5,
        };
        if keep < 0 || (keep == 0 && !up) {
            return if up {
                Dec {
                    digits: vec![1],
                    point: 1 - nd,
                }
            } else {
                Dec {
                    digits: Vec::new(),
                    point: 0,
                }
            };
        }
        let mut kept = self.digits[..keep as usize].to_vec();
        let mut point = self.point;
        if up {
            let mut i = kept.len();
            loop {
                if i == 0 {
                    kept.insert(0, 1);
                    point += 1;
                    break;
                }
                i -= 1;
                if kept[i] == 9 {
                    kept[i] = 0;
                } else {
                    kept[i] += 1;
                    break;
                }
            }
        }
        let mut d = Dec {
            digits: kept,
            point,
        };
        d.normalize();
        d
    }

    /// The double nearest this decimal.
    pub fn to_f64(&self) -> f64 {
        if self.is_zero() {
            return 0.0;
        }
        let s: String = self.digits.iter().map(|d| (b'0' + d) as char).collect();
        format!("0.{s}e{}", self.point).parse().unwrap_or(0.0)
    }

    /// The integer digits (empty for a value below 1) and exactly `nd`
    /// fraction digits, without rounding (round first).
    pub fn fixed_parts(&self, nd: usize) -> (String, String) {
        let digit = |i: i32| -> char {
            if i >= 0 && (i as usize) < self.digits.len() {
                (b'0' + self.digits[i as usize]) as char
            } else {
                '0'
            }
        };
        let int: String = (0..self.point.max(0)).map(digit).collect();
        let frac: String = (0..nd as i32).map(|k| digit(self.point + k)).collect();
        (int, frac)
    }
}

/// Round `x` to `nd` decimal places on its 15-digit decimal form.
pub fn round(x: f64, nd: i32, mode: Mode) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let d = Dec::from_f64(x);
    let len = d.digits.len() as i32;
    let keep = d.point.saturating_add(nd);
    if keep >= 15 {
        // Asking for more digits than Excel keeps: the exact binary value.
        return x;
    }
    let r = if keep >= len {
        // Nothing past the 15 digits to drop: the decimal value itself
        // (`ROUNDUP(0.1+0.2, 1)` is 0.3, not 0.4).
        d.to_f64()
    } else {
        d.round(nd, mode).to_f64()
    };
    if x < 0.0 { -r } else { r }
}

/// `x` cut to 15 significant digits (what Excel displays and compares).
pub fn to_15_digits(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    let r = Dec::from_f64(x).to_f64();
    if x < 0.0 { -r } else { r }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::approx_constant)] // Excel's documented examples
    fn rounds_like_excel() {
        assert_eq!(round(2.675, 2, Mode::Half), 2.68);
        assert_eq!(round(2.15, 1, Mode::Half), 2.2);
        assert_eq!(round(2.149, 1, Mode::Half), 2.1);
        assert_eq!(round(-1.475, 2, Mode::Half), -1.48);
        assert_eq!(round(21.5, -1, Mode::Half), 20.0);
        assert_eq!(round(626.3, -3, Mode::Half), 1000.0);
        assert_eq!(round(1.98, -1, Mode::Half), 0.0);
        assert_eq!(round(-50.55, -2, Mode::Half), -100.0);
        assert_eq!(round(-2.5, 0, Mode::Half), -3.0);
        assert_eq!(round(0.5, 0, Mode::Half), 1.0);
        assert_eq!(round(0.4, 0, Mode::Half), 0.0);
        assert_eq!(round(9.995, 2, Mode::Half), 10.0);
        assert_eq!(round(1.005, 2, Mode::Half), 1.01);
        assert_eq!(round(3.2, 0, Mode::Up), 4.0);
        assert_eq!(round(76.9, 0, Mode::Up), 77.0);
        assert_eq!(round(3.14159, 3, Mode::Up), 3.142);
        assert_eq!(round(-3.14159, 1, Mode::Up), -3.2);
        assert_eq!(round(31415.92654, -2, Mode::Up), 31500.0);
        assert_eq!(round(0.001, -2, Mode::Up), 100.0);
        assert_eq!(round(3.14159, 3, Mode::Down), 3.141);
        assert_eq!(round(-3.14159, 1, Mode::Down), -3.1);
        assert_eq!(round(31415.92654, -2, Mode::Down), 31400.0);
        assert_eq!(round(0.1 + 0.2, 1, Mode::Up), 0.3);
        assert_eq!(round(123.0, 5, Mode::Half), 123.0);
        assert_eq!(round(0.0, 2, Mode::Half), 0.0);
        assert_eq!(round(1e300, -301, Mode::Half), 0.0);
    }

    #[test]
    fn fixed_parts() {
        let d = Dec::from_f64(1234.5678).round(2, Mode::Half);
        assert_eq!(d.fixed_parts(2), ("1234".into(), "57".into()));
        let d = Dec::from_f64(0.05).round(1, Mode::Half);
        assert_eq!(d.fixed_parts(1), ("".into(), "1".into()));
        let d = Dec::from_f64(100.0);
        assert_eq!(d.fixed_parts(3), ("100".into(), "000".into()));
        assert_eq!(to_15_digits(0.1 + 0.2), 0.3);
    }
}
