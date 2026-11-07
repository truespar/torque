//! Roaring containers (Chambi, Lemire et al.): each holds the low 16 bits of the values that share one
//! high 16-bit key. Array: sorted u16, at most 4096 values. Bitset: 1024 x u64. Run: sorted, disjoint,
//! non-adjacent (start, len - 1) pairs.
//!
//! Every operation works on `CRef`, a borrowed view, so owned containers and containers read in place
//! from a mapped segment file mix freely. Results are always owned `Store`s.

pub const ARRAY_MAX: usize = 4096;
pub const WORDS: usize = 1024;

pub type Words = [u64; WORDS];

#[derive(Clone, Debug)]
pub enum Store {
    Array(Vec<u16>),
    Bitset(Box<Words>, u32),
    Run(Vec<[u16; 2]>, u32),
}

#[derive(Clone, Copy, Debug)]
pub enum CRef<'a> {
    Array(&'a [u16]),
    Bitset(&'a Words, u32),
    Run(&'a [[u16; 2]], u32),
}

#[inline]
fn empty_words() -> Box<Words> {
    // vec! + try_into avoids an 8 KB array on the stack.
    vec![0u64; WORDS].into_boxed_slice().try_into().unwrap()
}

#[inline]
fn set_range(words: &mut Words, start: u32, end_inclusive: u32) {
    let (sw, ew) = ((start >> 6) as usize, (end_inclusive >> 6) as usize);
    let first = !0u64 << (start & 63);
    let last = !0u64 >> (63 - (end_inclusive & 63));
    if sw == ew {
        words[sw] |= first & last;
    } else {
        words[sw] |= first;
        for w in &mut words[sw + 1..ew] {
            *w = !0;
        }
        words[ew] |= last;
    }
}

#[inline]
fn clear_range(words: &mut Words, start: u32, end_inclusive: u32) {
    let (sw, ew) = ((start >> 6) as usize, (end_inclusive >> 6) as usize);
    let first = !0u64 << (start & 63);
    let last = !0u64 >> (63 - (end_inclusive & 63));
    if sw == ew {
        words[sw] &= !(first & last);
    } else {
        words[sw] &= !first;
        for w in &mut words[sw + 1..ew] {
            *w = 0;
        }
        words[ew] &= !last;
    }
}

#[inline]
fn popcount(words: &Words) -> u32 {
    words.iter().map(|w| w.count_ones()).sum()
}

/// Owned container from a bitset: an array when it holds at most 4096 values.
pub fn from_words(words: Box<Words>, card: u32) -> Option<Store> {
    if card == 0 {
        None
    } else if card as usize <= ARRAY_MAX {
        let mut v = Vec::with_capacity(card as usize);
        for (i, &w) in words.iter().enumerate() {
            let mut w = w;
            while w != 0 {
                v.push(((i << 6) as u32 + w.trailing_zeros()) as u16);
                w &= w - 1;
            }
        }
        Some(Store::Array(v))
    } else {
        Some(Store::Bitset(words, card))
    }
}

fn from_sorted_vec(v: Vec<u16>) -> Option<Store> {
    if v.is_empty() {
        None
    } else if v.len() <= ARRAY_MAX {
        Some(Store::Array(v))
    } else {
        let mut words = empty_words();
        for &x in &v {
            words[(x >> 6) as usize] |= 1u64 << (x & 63);
        }
        let card = v.len() as u32;
        Some(Store::Bitset(words, card))
    }
}

impl Store {
    #[inline]
    pub fn as_ref(&self) -> CRef<'_> {
        match self {
            Store::Array(v) => CRef::Array(v),
            Store::Bitset(w, c) => CRef::Bitset(w, *c),
            Store::Run(r, c) => CRef::Run(r, *c),
        }
    }

    pub fn single(low: u16) -> Store {
        Store::Array(vec![low])
    }

    pub fn full_range(lo: u16, hi: u16) -> Store {
        Store::Run(vec![[lo, hi - lo]], u32::from(hi - lo) + 1)
    }

    /// Insert; returns true when the value was new.
    pub fn insert(&mut self, low: u16) -> bool {
        match self {
            Store::Array(v) => {
                if v.last().is_none_or(|&l| l < low) {
                    v.push(low);
                } else {
                    match v.binary_search(&low) {
                        Ok(_) => return false,
                        Err(pos) => v.insert(pos, low),
                    }
                }
                if v.len() > ARRAY_MAX {
                    *self = from_sorted_vec(std::mem::take(v)).unwrap();
                }
                true
            }
            Store::Bitset(w, c) => {
                let (i, bit) = ((low >> 6) as usize, 1u64 << (low & 63));
                if w[i] & bit != 0 {
                    return false;
                }
                w[i] |= bit;
                *c += 1;
                true
            }
            Store::Run(..) => {
                if self.as_ref().contains(low) {
                    return false;
                }
                let mut words = self.as_ref().to_words();
                words[(low >> 6) as usize] |= 1u64 << (low & 63);
                let card = self.as_ref().card() + 1;
                *self = from_words(words, card).unwrap();
                true
            }
        }
    }

    /// Remove; returns true when the value was present. The container may become empty.
    pub fn remove(&mut self, low: u16) -> bool {
        match self {
            Store::Array(v) => match v.binary_search(&low) {
                Ok(pos) => {
                    v.remove(pos);
                    true
                }
                Err(_) => false,
            },
            Store::Bitset(w, c) => {
                let (i, bit) = ((low >> 6) as usize, 1u64 << (low & 63));
                if w[i] & bit == 0 {
                    return false;
                }
                w[i] &= !bit;
                *c -= 1;
                if *c as usize <= ARRAY_MAX {
                    let words = std::mem::replace(w, empty_words());
                    let card = *c;
                    *self = from_words(words, card).unwrap_or(Store::Array(Vec::new()));
                }
                true
            }
            Store::Run(..) => {
                if !self.as_ref().contains(low) {
                    return false;
                }
                let mut words = self.as_ref().to_words();
                words[(low >> 6) as usize] &= !(1u64 << (low & 63));
                let card = self.as_ref().card() - 1;
                *self = from_words(words, card).unwrap_or(Store::Array(Vec::new()));
                true
            }
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.as_ref().card() == 0
    }

    /// Heap bytes held by this container.
    pub fn heap_bytes(&self) -> usize {
        match self {
            Store::Array(v) => v.capacity() * 2,
            Store::Bitset(..) => WORDS * 8,
            Store::Run(r, _) => r.capacity() * 4,
        }
    }

    /// The smallest of array, bitset and run for these values.
    pub fn optimized(&self) -> Store {
        let c = self.as_ref();
        let card = c.card() as usize;
        let runs = c.count_runs();
        let run_bytes = 2 + 4 * runs;
        let array_bytes = 2 * card;
        let best_flat = if card <= ARRAY_MAX { array_bytes } else { WORDS * 8 };
        if run_bytes < best_flat {
            Store::Run(c.to_runs(), card as u32)
        } else if card <= ARRAY_MAX {
            Store::Array(c.to_vec())
        } else {
            Store::Bitset(c.to_words(), card as u32)
        }
    }
}

impl<'a> CRef<'a> {
    #[inline]
    pub fn card(&self) -> u32 {
        match *self {
            CRef::Array(v) => v.len() as u32,
            CRef::Bitset(_, c) | CRef::Run(_, c) => c,
        }
    }

    #[inline]
    pub fn contains(&self, low: u16) -> bool {
        match *self {
            CRef::Array(v) => v.binary_search(&low).is_ok(),
            CRef::Bitset(w, _) => w[(low >> 6) as usize] & (1u64 << (low & 63)) != 0,
            CRef::Run(r, _) => {
                let i = r.partition_point(|run| run[0] <= low);
                i > 0 && u32::from(low) <= u32::from(r[i - 1][0]) + u32::from(r[i - 1][1])
            }
        }
    }

    /// Number of values <= low.
    pub fn rank(&self, low: u16) -> u32 {
        match *self {
            CRef::Array(v) => v.partition_point(|&x| x <= low) as u32,
            CRef::Bitset(w, _) => {
                let i = (low >> 6) as usize;
                let below: u32 = w[..i].iter().map(|x| x.count_ones()).sum();
                let mask = if low & 63 == 63 { !0u64 } else { (1u64 << ((low & 63) + 1)) - 1 };
                below + (w[i] & mask).count_ones()
            }
            CRef::Run(r, _) => {
                let mut n = 0u32;
                for run in r {
                    let (s, e) = (u32::from(run[0]), u32::from(run[0]) + u32::from(run[1]));
                    if s > u32::from(low) {
                        break;
                    }
                    n += e.min(u32::from(low)) - s + 1;
                }
                n
            }
        }
    }

    pub fn min(&self) -> Option<u16> {
        match *self {
            CRef::Array(v) => v.first().copied(),
            CRef::Bitset(w, _) => w.iter().enumerate().find(|(_, x)| **x != 0).map(|(i, x)| ((i << 6) as u32 + x.trailing_zeros()) as u16),
            CRef::Run(r, _) => r.first().map(|run| run[0]),
        }
    }

    pub fn max(&self) -> Option<u16> {
        match *self {
            CRef::Array(v) => v.last().copied(),
            CRef::Bitset(w, _) => w.iter().enumerate().rev().find(|(_, x)| **x != 0).map(|(i, x)| ((i << 6) as u32 + 63 - x.leading_zeros()) as u16),
            CRef::Run(r, _) => r.last().map(|run| run[0] + run[1]),
        }
    }

    pub fn to_words(&self) -> Box<Words> {
        let mut words = empty_words();
        self.or_into(&mut words);
        words
    }

    /// OR these values into a bitset.
    pub fn or_into(&self, words: &mut Words) {
        match *self {
            CRef::Array(v) => {
                for &x in v {
                    words[(x >> 6) as usize] |= 1u64 << (x & 63);
                }
            }
            CRef::Bitset(w, _) => {
                for (d, s) in words.iter_mut().zip(w.iter()) {
                    *d |= *s;
                }
            }
            CRef::Run(r, _) => {
                for run in r {
                    set_range(words, u32::from(run[0]), u32::from(run[0]) + u32::from(run[1]));
                }
            }
        }
    }

    pub fn to_vec(&self) -> Vec<u16> {
        match *self {
            CRef::Array(v) => v.to_vec(),
            _ => {
                let mut out = Vec::with_capacity(self.card() as usize);
                out.extend(self.iter());
                out
            }
        }
    }

    pub fn to_runs(&self) -> Vec<[u16; 2]> {
        if let CRef::Run(r, _) = *self {
            return r.to_vec();
        }
        let mut runs: Vec<[u16; 2]> = Vec::new();
        for x in self.iter() {
            match runs.last_mut() {
                Some(last) if u32::from(last[0]) + u32::from(last[1]) + 1 == u32::from(x) => last[1] += 1,
                _ => runs.push([x, 0]),
            }
        }
        runs
    }

    pub fn count_runs(&self) -> usize {
        match *self {
            CRef::Run(r, _) => r.len(),
            CRef::Array(v) => {
                if v.is_empty() {
                    0
                } else {
                    1 + v.windows(2).filter(|p| p[1] != p[0] + 1).count()
                }
            }
            CRef::Bitset(w, _) => {
                // A run starts at every set bit whose predecessor bit is clear.
                let mut runs = 0u32;
                let mut carry = 0u64;
                for &x in w.iter() {
                    runs += (x & !((x << 1) | carry)).count_ones();
                    carry = x >> 63;
                }
                runs as usize
            }
        }
    }

    pub fn to_store(&self) -> Store {
        match *self {
            CRef::Array(v) => Store::Array(v.to_vec()),
            CRef::Bitset(w, c) => Store::Bitset(Box::new(*w), c),
            CRef::Run(r, c) => Store::Run(r.to_vec(), c),
        }
    }

    #[inline]
    pub fn iter(&self) -> CIter<'a> {
        match *self {
            CRef::Array(v) => CIter::Array(v.iter()),
            CRef::Bitset(w, _) => CIter::Bitset { words: w, idx: 0, cur: w[0] },
            CRef::Run(r, _) => CIter::Run { runs: r, idx: 0, next: r.first().map_or(0, |x| u32::from(x[0])) },
        }
    }

    /// The values from the largest down, while `f` returns true; false when `f` stopped it.
    pub fn for_each_rev(&self, mut f: impl FnMut(u16) -> bool) -> bool {
        match *self {
            CRef::Array(v) => v.iter().rev().all(|&x| f(x)),
            CRef::Bitset(w, _) => {
                for (i, &word) in w.iter().enumerate().rev() {
                    let mut x = word;
                    while x != 0 {
                        let bit = 63 - x.leading_zeros();
                        if !f(((i << 6) as u32 + bit) as u16) {
                            return false;
                        }
                        x &= !(1u64 << bit);
                    }
                }
                true
            }
            CRef::Run(r, _) => r.iter().rev().all(|run| (u32::from(run[0])..=u32::from(run[0]) + u32::from(run[1])).rev().all(|x| f(x as u16))),
        }
    }
}

pub enum CIter<'a> {
    Array(std::slice::Iter<'a, u16>),
    Bitset { words: &'a Words, idx: usize, cur: u64 },
    Run { runs: &'a [[u16; 2]], idx: usize, next: u32 },
}

impl Iterator for CIter<'_> {
    type Item = u16;

    #[inline]
    fn next(&mut self) -> Option<u16> {
        match self {
            CIter::Array(it) => it.next().copied(),
            CIter::Bitset { words, idx, cur } => loop {
                if *cur != 0 {
                    let v = ((*idx << 6) as u32 + cur.trailing_zeros()) as u16;
                    *cur &= *cur - 1;
                    return Some(v);
                }
                *idx += 1;
                if *idx >= WORDS {
                    return None;
                }
                *cur = words[*idx];
            },
            CIter::Run { runs, idx, next } => {
                let run = runs.get(*idx)?;
                let v = *next as u16;
                let end = u32::from(run[0]) + u32::from(run[1]);
                if *next >= end {
                    *idx += 1;
                    *next = runs.get(*idx).map_or(0, |x| u32::from(x[0]));
                } else {
                    *next += 1;
                }
                Some(v)
            }
        }
    }
}

// -- binary operations -----------------------------------------------------------------------

/// Exponential search for the first index in `v[from..]` with `v[i] >= x`.
#[inline]
fn gallop(v: &[u16], from: usize, x: u16) -> usize {
    let mut lo = from;
    let mut step = 1usize;
    let mut hi = from;
    while hi < v.len() && v[hi] < x {
        lo = hi + 1;
        hi += step;
        step <<= 1;
    }
    let hi = hi.min(v.len());
    lo + v[lo..hi].partition_point(|&y| y < x)
}

fn intersect_arrays(a: &[u16], b: &[u16], out: &mut Vec<u16>) {
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if small.len() * 32 < large.len() {
        let mut j = 0;
        for &x in small {
            j = gallop(large, j, x);
            if j == large.len() {
                break;
            }
            if large[j] == x {
                out.push(x);
            }
        }
        return;
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        // SAFETY: i < a.len() and j < b.len() are checked by the loop condition.
        let (x, y) = unsafe { (*a.get_unchecked(i), *b.get_unchecked(j)) };
        if x == y {
            out.push(x);
        }
        i += usize::from(x <= y);
        j += usize::from(y <= x);
    }
}

fn count_intersect_arrays(a: &[u16], b: &[u16]) -> u32 {
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut n = 0u32;
    if small.len() * 32 < large.len() {
        let mut j = 0;
        for &x in small {
            j = gallop(large, j, x);
            if j == large.len() {
                break;
            }
            n += u32::from(large[j] == x);
        }
        return n;
    }
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        // SAFETY: bounds checked by the loop condition.
        let (x, y) = unsafe { (*a.get_unchecked(i), *b.get_unchecked(j)) };
        n += u32::from(x == y);
        i += usize::from(x <= y);
        j += usize::from(y <= x);
    }
    n
}

#[inline]
fn bit(w: &Words, x: u16) -> bool {
    w[(x >> 6) as usize] & (1u64 << (x & 63)) != 0
}

/// Array values whose bit in `w` equals `want`.
fn filter_by_bits(v: &[u16], w: &Words, want: bool) -> Vec<u16> {
    let mut out = Vec::with_capacity(v.len());
    for &x in v {
        if bit(w, x) == want {
            out.push(x);
        }
    }
    out
}

/// Set bits of `w` in [start, end].
fn count_range(w: &Words, start: u32, end: u32) -> u32 {
    let (sw, ew) = ((start >> 6) as usize, (end >> 6) as usize);
    let first = !0u64 << (start & 63);
    let last = !0u64 >> (63 - (end & 63));
    if sw == ew {
        return (w[sw] & first & last).count_ones();
    }
    let mid: u32 = w[sw + 1..ew].iter().map(|x| x.count_ones()).sum();
    (w[sw] & first).count_ones() + mid + (w[ew] & last).count_ones()
}

/// Values of a sorted array inside (`keep_inside`) or outside the runs.
fn filter_by_runs(v: &[u16], r: &[[u16; 2]], keep_inside: bool) -> Vec<u16> {
    let mut out = Vec::with_capacity(v.len());
    let mut k = 0;
    for &x in v {
        while k < r.len() && u32::from(r[k][0]) + u32::from(r[k][1]) < u32::from(x) {
            k += 1;
        }
        let inside = k < r.len() && r[k][0] <= x;
        if inside == keep_inside {
            out.push(x);
        }
    }
    out
}

pub fn and(a: CRef<'_>, b: CRef<'_>) -> Option<Store> {
    match (a, b) {
        (CRef::Array(x), CRef::Array(y)) => {
            let mut out = Vec::with_capacity(x.len().min(y.len()));
            intersect_arrays(x, y, &mut out);
            (!out.is_empty()).then_some(Store::Array(out))
        }
        (CRef::Array(v), CRef::Bitset(w, _)) | (CRef::Bitset(w, _), CRef::Array(v)) => {
            let out = filter_by_bits(v, w, true);
            (!out.is_empty()).then_some(Store::Array(out))
        }
        (CRef::Array(v), CRef::Run(r, _)) | (CRef::Run(r, _), CRef::Array(v)) => {
            let out = filter_by_runs(v, r, true);
            (!out.is_empty()).then_some(Store::Array(out))
        }
        (CRef::Bitset(x, _), CRef::Bitset(y, _)) => {
            let mut words = empty_words();
            let mut card = 0u32;
            for i in 0..WORDS {
                let w = x[i] & y[i];
                words[i] = w;
                card += w.count_ones();
            }
            from_words(words, card)
        }
        (CRef::Run(x, _), CRef::Run(y, _)) => {
            let mut out: Vec<[u16; 2]> = Vec::new();
            let mut card = 0u32;
            let (mut i, mut j) = (0, 0);
            while i < x.len() && j < y.len() {
                let (xs, xe) = (u32::from(x[i][0]), u32::from(x[i][0]) + u32::from(x[i][1]));
                let (ys, ye) = (u32::from(y[j][0]), u32::from(y[j][0]) + u32::from(y[j][1]));
                let (s, e) = (xs.max(ys), xe.min(ye));
                if s <= e {
                    out.push([s as u16, (e - s) as u16]);
                    card += e - s + 1;
                }
                if xe < ye { i += 1 } else { j += 1 }
            }
            (card > 0).then_some(Store::Run(out, card))
        }
        (CRef::Bitset(w, _), CRef::Run(r, _)) | (CRef::Run(r, _), CRef::Bitset(w, _)) => {
            let mut words = empty_words();
            for run in r {
                set_range(&mut words, u32::from(run[0]), u32::from(run[0]) + u32::from(run[1]));
            }
            let mut card = 0u32;
            for i in 0..WORDS {
                words[i] &= w[i];
                card += words[i].count_ones();
            }
            from_words(words, card)
        }
    }
}

pub fn and_card(a: CRef<'_>, b: CRef<'_>) -> u32 {
    match (a, b) {
        (CRef::Array(x), CRef::Array(y)) => count_intersect_arrays(x, y),
        (CRef::Array(v), CRef::Bitset(w, _)) | (CRef::Bitset(w, _), CRef::Array(v)) => {
            v.iter().map(|&x| u32::from(bit(w, x))).sum()
        }
        (CRef::Bitset(x, _), CRef::Bitset(y, _)) => x.iter().zip(y.iter()).map(|(p, q)| (p & q).count_ones()).sum(),
        (CRef::Bitset(w, _), CRef::Run(r, _)) | (CRef::Run(r, _), CRef::Bitset(w, _)) => r
            .iter()
            .map(|run| count_range(w, u32::from(run[0]), u32::from(run[0]) + u32::from(run[1])))
            .sum(),
        (CRef::Array(v), CRef::Run(r, _)) | (CRef::Run(r, _), CRef::Array(v)) => filter_by_runs(v, r, true).len() as u32,
        _ => and(a, b).map_or(0, |s| s.as_ref().card()),
    }
}

pub fn or(a: CRef<'_>, b: CRef<'_>) -> Store {
    match (a, b) {
        (CRef::Array(x), CRef::Array(y)) => {
            if x.len() + y.len() > ARRAY_MAX {
                let mut words = empty_words();
                CRef::Array(x).or_into(&mut words);
                CRef::Array(y).or_into(&mut words);
                let card = popcount(&words);
                return from_words(words, card).unwrap();
            }
            let mut out = Vec::with_capacity(x.len() + y.len());
            let (mut i, mut j) = (0, 0);
            while i < x.len() && j < y.len() {
                // SAFETY: bounds checked by the loop condition.
                let (a, b) = unsafe { (*x.get_unchecked(i), *y.get_unchecked(j)) };
                out.push(a.min(b));
                i += usize::from(a <= b);
                j += usize::from(b <= a);
            }
            out.extend_from_slice(&x[i..]);
            out.extend_from_slice(&y[j..]);
            Store::Array(out)
        }
        (CRef::Run(x, _), CRef::Run(y, _)) => {
            let mut out: Vec<[u16; 2]> = Vec::with_capacity(x.len() + y.len());
            let (mut i, mut j) = (0, 0);
            while i < x.len() || j < y.len() {
                let take_x = j >= y.len() || (i < x.len() && x[i][0] <= y[j][0]);
                let run = if take_x { i += 1; x[i - 1] } else { j += 1; y[j - 1] };
                let (s, e) = (u32::from(run[0]), u32::from(run[0]) + u32::from(run[1]));
                match out.last_mut() {
                    Some(last) if s <= u32::from(last[0]) + u32::from(last[1]) + 1 => {
                        let le = (u32::from(last[0]) + u32::from(last[1])).max(e);
                        last[1] = (le - u32::from(last[0])) as u16;
                    }
                    _ => out.push(run),
                }
            }
            let card = out.iter().map(|r| u32::from(r[1]) + 1).sum();
            Store::Run(out, card)
        }
        (CRef::Bitset(x, _), CRef::Bitset(y, _)) => {
            let mut words = empty_words();
            let mut card = 0u32;
            for i in 0..WORDS {
                let w = x[i] | y[i];
                words[i] = w;
                card += w.count_ones();
            }
            Store::Bitset(words, card)
        }
        (CRef::Bitset(w, _), CRef::Run(r, _)) | (CRef::Run(r, _), CRef::Bitset(w, _)) => {
            let mut words: Box<Words> = Box::new(*w);
            for run in r {
                set_range(&mut words, u32::from(run[0]), u32::from(run[0]) + u32::from(run[1]));
            }
            let card = popcount(&words);
            Store::Bitset(words, card)
        }
        (CRef::Bitset(w, c), CRef::Array(v)) | (CRef::Array(v), CRef::Bitset(w, c)) => {
            let mut words: Box<Words> = Box::new(*w);
            let mut card = c;
            for &x in v {
                let (i, m) = ((x >> 6) as usize, 1u64 << (x & 63));
                card += u32::from(words[i] & m == 0);
                words[i] |= m;
            }
            Store::Bitset(words, card)
        }
        _ => {
            let mut words = a.to_words();
            b.or_into(&mut words);
            let card = popcount(&words);
            from_words(words, card).unwrap()
        }
    }
}

pub fn andnot(a: CRef<'_>, b: CRef<'_>) -> Option<Store> {
    match (a, b) {
        (CRef::Array(x), CRef::Array(y)) => {
            let mut out = Vec::with_capacity(x.len());
            let mut j = 0;
            for &v in x {
                j = gallop(y, j, v);
                if j == y.len() || y[j] != v {
                    out.push(v);
                }
            }
            (!out.is_empty()).then_some(Store::Array(out))
        }
        (CRef::Array(x), CRef::Bitset(w, _)) => {
            let out = filter_by_bits(x, w, false);
            (!out.is_empty()).then_some(Store::Array(out))
        }
        (CRef::Array(x), CRef::Run(r, _)) => {
            let out = filter_by_runs(x, r, false);
            (!out.is_empty()).then_some(Store::Array(out))
        }
        (CRef::Bitset(x, _), CRef::Bitset(y, _)) => {
            let mut words = empty_words();
            let mut card = 0u32;
            for i in 0..WORDS {
                let w = x[i] & !y[i];
                words[i] = w;
                card += w.count_ones();
            }
            from_words(words, card)
        }
        (_, other) => {
            let mut words = a.to_words();
            match other {
                CRef::Array(v) => {
                    for &x in v {
                        words[(x >> 6) as usize] &= !(1u64 << (x & 63));
                    }
                }
                CRef::Bitset(w, _) => {
                    for (d, s) in words.iter_mut().zip(w.iter()) {
                        *d &= !*s;
                    }
                }
                CRef::Run(r, _) => {
                    for run in r {
                        clear_range(&mut words, u32::from(run[0]), u32::from(run[0]) + u32::from(run[1]));
                    }
                }
            }
            let card = popcount(&words);
            from_words(words, card)
        }
    }
}

/// In-place OR when the destination is a bitset (the common case for unions of large postings).
pub fn or_assign(dst: &mut Store, src: CRef<'_>) {
    if let Store::Bitset(w, c) = dst {
        src.or_into(w);
        *c = popcount(w);
        return;
    }
    *dst = or(dst.as_ref(), src);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rng(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    fn sample(seed: &mut u64, kind: u8) -> std::collections::BTreeSet<u16> {
        let mut s = std::collections::BTreeSet::new();
        match kind {
            0 => (0..rng(seed) % 200).for_each(|_| { s.insert(rng(seed) as u16); }),
            1 => (0..5000 + rng(seed) % 30000).for_each(|_| { s.insert(rng(seed) as u16); }),
            _ => {
                for _ in 0..1 + rng(seed) % 20 {
                    let a = rng(seed) as u16;
                    let len = rng(seed) % 3000;
                    for x in u32::from(a)..(u32::from(a) + len as u32).min(65536) {
                        s.insert(x as u16);
                    }
                }
            }
        }
        s
    }

    fn store_of(s: &std::collections::BTreeSet<u16>, kind: u8) -> Option<Store> {
        let v: Vec<u16> = s.iter().copied().collect();
        let st = from_sorted_vec(v)?;
        Some(match kind {
            2 => Store::Run(st.as_ref().to_runs(), st.as_ref().card()),
            _ => st,
        })
    }

    fn values(s: &Option<Store>) -> Vec<u16> {
        s.as_ref().map_or_else(Vec::new, |s| s.as_ref().iter().collect())
    }

    #[test]
    fn algebra_matches_set_semantics() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        for round in 0..600 {
            let (ka, kb) = ((round % 3) as u8, ((round / 3) % 3) as u8);
            let (sa, sb) = (sample(&mut seed, ka), sample(&mut seed, kb));
            let (Some(a), Some(b)) = (store_of(&sa, ka), store_of(&sb, kb)) else { continue };
            let want_and: Vec<u16> = sa.intersection(&sb).copied().collect();
            let want_or: Vec<u16> = sa.union(&sb).copied().collect();
            let want_not: Vec<u16> = sa.difference(&sb).copied().collect();
            assert_eq!(values(&and(a.as_ref(), b.as_ref())), want_and, "and {ka}{kb}");
            assert_eq!(and_card(a.as_ref(), b.as_ref()) as usize, want_and.len());
            assert_eq!(values(&Some(or(a.as_ref(), b.as_ref()))), want_or, "or {ka}{kb}");
            assert_eq!(values(&andnot(a.as_ref(), b.as_ref())), want_not, "andnot {ka}{kb}");
            let mut d = a.clone();
            or_assign(&mut d, b.as_ref());
            assert_eq!(values(&Some(d)), want_or);
            for probe in [0u16, 1, 4095, 4096, 32767, 65535, *sa.iter().next().unwrap()] {
                assert_eq!(a.as_ref().contains(probe), sa.contains(&probe));
                assert_eq!(a.as_ref().rank(probe) as usize, sa.range(..=probe).count());
            }
            assert_eq!(a.as_ref().min(), sa.iter().next().copied());
            assert_eq!(a.as_ref().max(), sa.iter().next_back().copied());
            assert_eq!(a.as_ref().count_runs(), Store::Run(a.as_ref().to_runs(), 0).as_ref().count_runs());
            let opt = a.optimized();
            assert_eq!(values(&Some(opt)), sa.iter().copied().collect::<Vec<_>>());
        }
    }

    #[test]
    fn insert_remove_convert_between_kinds() {
        let mut s = Store::Array(Vec::new());
        for x in (0..10_000u32).map(|i| (i * 7 % 65536) as u16) {
            s.insert(x);
        }
        assert!(matches!(s, Store::Bitset(..)));
        assert_eq!(s.as_ref().card(), 10_000);
        for x in (0..6_000u32).map(|i| (i * 7 % 65536) as u16) {
            assert!(s.remove(x));
        }
        assert!(matches!(s, Store::Array(_)));
        assert_eq!(s.as_ref().card(), 4_000);
        let mut r = Store::full_range(100, 200);
        assert!(r.insert(50));
        assert!(r.remove(150));
        assert_eq!(r.as_ref().card(), 101);
        assert!(!r.as_ref().contains(150) && r.as_ref().contains(50));
    }
}
