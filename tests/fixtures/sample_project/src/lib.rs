//! Sample crate used by cargo-crap's integration tests.
//!
//! Functions are chosen to span the interesting score regions:
//! - `trivial` should score ≈ 1 (CC=1, 100% covered).
//! - `moderate` should be borderline (CC≈4, partial coverage).
//! - `crappy` should be well above threshold (high CC, no coverage).

pub fn trivial() -> i32 {
    42
}

pub fn moderate(x: i32) -> &'static str {
    if x < 0 {
        "neg"
    } else if x == 0 {
        "zero"
    } else if x > 100 {
        "big"
    } else {
        "small"
    }
}

pub fn crappy(a: i32, b: i32, c: i32, mode: i32) -> i32 {
    let mut result = 0;
    if mode == 0 {
        if a > 0 {
            result += a;
        } else {
            result -= a;
        }
        if b > 0 {
            result += b * 2;
        } else if b < -10 {
            result -= b / 2;
        }
    } else if mode == 1 {
        for i in 0..a {
            if i % 2 == 0 {
                result += b;
            } else if i % 3 == 0 {
                result += c;
            } else {
                result -= 1;
            }
        }
    } else if mode == 2 {
        while result < c {
            result += 1;
            if result > 1000 {
                break;
            }
        }
    } else {
        result = -1;
    }
    result
}
