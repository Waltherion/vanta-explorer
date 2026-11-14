//! The order a file manager lists names in, shared so that every app agrees on it.
//!
//! Case-insensitive, with digit runs compared as numbers. Files named for where they should land in
//! that listing (numbered wallpapers, say) would be reordered by a plain byte sort, which puts
//! every capitalised name before every lowercase one, and the wallpaper daemon and the selector would disagree about which
//! one comes first.

use std::cmp::Ordering;

/// Case-insensitive compare with numeric runs compared as numbers, so "file2" < "file10".
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.chars().flat_map(|c| c.to_lowercase()).peekable();
    let mut bi = b.chars().flat_map(|c| c.to_lowercase()).peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut xn: u64 = 0;
                let mut yn: u64 = 0;
                while let Some(d) = ai.peek().and_then(|c| c.to_digit(10)) {
                    xn = xn.saturating_mul(10).saturating_add(d as u64);
                    ai.next();
                }
                while let Some(d) = bi.peek().and_then(|c| c.to_digit(10)) {
                    yn = yn.saturating_mul(10).saturating_add(d as u64);
                    bi.next();
                }
                if xn != yn {
                    return xn.cmp(&yn);
                }
            }
            (Some(x), Some(y)) => match x.cmp(&y) {
                Ordering::Equal => {
                    ai.next();
                    bi.next();
                }
                ord => return ord,
            },
        }
    }
}

/// Sorts names in that order. Names equal but for case fall back to the raw name, so the order
/// is total and the same on every run.
pub fn sort_natural(names: &mut [String]) {
    names.sort_by(|a, b| natural_cmp(a, b).then_with(|| a.cmp(b)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_count_as_numbers_and_case_does_not_count() {
        let mut v: Vec<String> = ["file10.png", "File2.png", "file1.png", "ZZZ-gray.png", "eclipse.mp4"].iter().map(|s| s.to_string()).collect();
        sort_natural(&mut v);
        assert_eq!(v, ["eclipse.mp4", "file1.png", "File2.png", "file10.png", "ZZZ-gray.png"]);
    }
}
