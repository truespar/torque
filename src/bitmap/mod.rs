//! Torque's roaring bitmap.
//!
//! One type for owned bitmaps and for bitmaps read in place from a mapped TQBF segment ("frozen",
//! spec §6.2). All algebra runs on borrowed containers (`store::CRef`), so frozen and owned bitmaps mix
//! freely; results are owned. Cloning a frozen bitmap is an `Arc` increment; mutating one copies its
//! containers first.

pub mod frozen;
pub mod store;

#[cfg(target_endian = "big")]
compile_error!("crate::bitmap reads little-endian frozen bitmaps in place");

use crate::tqbf::bytes::Bytes;
use std::io::{Read, Write};
use store::{CRef, Store};

#[derive(Clone)]
enum Repr {
    Owned(Vec<(u16, Store)>),
    Frozen(frozen::Frozen),
}

#[derive(Clone)]
pub struct Bitmap {
    repr: Repr,
}

impl Default for Bitmap {
    fn default() -> Self {
        Self::new()
    }
}

#[inline]
fn split(v: u32) -> (u16, u16) {
    ((v >> 16) as u16, v as u16)
}

impl Bitmap {
    #[inline]
    pub fn new() -> Self {
        Self { repr: Repr::Owned(Vec::new()) }
    }

    pub fn of(values: &[u32]) -> Self {
        values.iter().copied().collect()
    }

    /// Wrap frozen bytes without copying (they are copied once only when misaligned).
    pub fn from_frozen(bytes: Bytes) -> std::io::Result<Self> {
        Ok(Self { repr: Repr::Frozen(frozen::Frozen::parse(bytes)?) })
    }

    fn from_entries(entries: Vec<(u16, Store)>) -> Self {
        Self { repr: Repr::Owned(entries) }
    }

    // -- container access ----------------------------------------------------------------------

    #[inline]
    fn n(&self) -> usize {
        match &self.repr {
            Repr::Owned(e) => e.len(),
            Repr::Frozen(f) => f.len(),
        }
    }

    #[inline]
    fn key(&self, i: usize) -> u16 {
        match &self.repr {
            Repr::Owned(e) => e[i].0,
            Repr::Frozen(f) => f.key(i),
        }
    }

    #[inline]
    fn cref(&self, i: usize) -> CRef<'_> {
        match &self.repr {
            Repr::Owned(e) => e[i].1.as_ref(),
            Repr::Frozen(f) => f.cref(i),
        }
    }

    #[inline]
    fn card_at(&self, i: usize) -> u32 {
        match &self.repr {
            Repr::Owned(e) => e[i].1.as_ref().card(),
            Repr::Frozen(f) => f.card(i),
        }
    }

    #[inline]
    fn find(&self, key: u16) -> Result<usize, usize> {
        match &self.repr {
            Repr::Owned(e) => e.binary_search_by_key(&key, |x| x.0),
            Repr::Frozen(f) => f.keys().binary_search(&key),
        }
    }

    fn containers(&self) -> impl ExactSizeIterator<Item = (u16, CRef<'_>)> + Clone + '_ {
        (0..self.n()).map(move |i| (self.key(i), self.cref(i)))
    }

    /// Owned containers, copying a frozen bitmap first.
    fn owned(&mut self) -> &mut Vec<(u16, Store)> {
        if let Repr::Frozen(f) = &self.repr {
            let entries = (0..f.len()).map(|i| (f.key(i), f.cref(i).to_store())).collect();
            self.repr = Repr::Owned(entries);
        }
        match &mut self.repr {
            Repr::Owned(e) => e,
            Repr::Frozen(_) => unreachable!(),
        }
    }

    pub fn is_frozen(&self) -> bool {
        matches!(self.repr, Repr::Frozen(_))
    }

    // -- mutation ------------------------------------------------------------------------------

    pub fn add(&mut self, value: u32) {
        let (k, low) = split(value);
        let e = self.owned();
        match e.last_mut() {
            Some((lk, s)) if *lk == k => {
                s.insert(low);
            }
            Some((lk, _)) if *lk < k => e.push((k, Store::single(low))),
            None => e.push((k, Store::single(low))),
            _ => match e.binary_search_by_key(&k, |x| x.0) {
                Ok(i) => {
                    e[i].1.insert(low);
                }
                Err(i) => e.insert(i, (k, Store::single(low))),
            },
        }
    }

    pub fn add_many(&mut self, values: &[u32]) {
        for &v in values {
            self.add(v);
        }
    }

    /// Insert every value in `range`.
    pub fn add_range<R: std::ops::RangeBounds<u32>>(&mut self, range: R) {
        use std::ops::Bound::*;
        let start = match range.start_bound() {
            Included(&s) => u64::from(s),
            Excluded(&s) => u64::from(s) + 1,
            Unbounded => 0,
        };
        let end = match range.end_bound() {
            Included(&e) => u64::from(e) + 1,
            Excluded(&e) => u64::from(e),
            Unbounded => 1u64 << 32,
        };
        if start >= end {
            return;
        }
        let (sk, ek) = ((start >> 16) as u16, ((end - 1) >> 16) as u16);
        let e = self.owned();
        for k in sk..=ek {
            let lo = if k == sk { (start & 0xFFFF) as u16 } else { 0 };
            let hi = if k == ek { ((end - 1) & 0xFFFF) as u16 } else { 0xFFFF };
            let run = Store::full_range(lo, hi);
            match e.binary_search_by_key(&k, |x| x.0) {
                Ok(i) => e[i].1 = store::or(e[i].1.as_ref(), run.as_ref()),
                Err(i) => e.insert(i, (k, run)),
            }
        }
    }

    pub fn remove(&mut self, value: u32) {
        let (k, low) = split(value);
        if self.find(k).is_err() {
            return;
        }
        let e = self.owned();
        if let Ok(i) = e.binary_search_by_key(&k, |x| x.0) {
            e[i].1.remove(low);
            if e[i].1.is_empty() {
                e.remove(i);
            }
        }
    }

    /// `self -= other`.
    pub fn andnot_inplace(&mut self, other: &Bitmap) {
        *self = andnot(self, other);
    }

    /// Re-encode every container in its smallest form (array, bitset or run), allocated exactly.
    pub fn run_optimize(&mut self) {
        if let Repr::Owned(e) = &mut self.repr {
            for (_, s) in e.iter_mut() {
                *s = s.optimized();
            }
            e.shrink_to_fit();
        }
    }

    // -- queries -------------------------------------------------------------------------------

    #[inline]
    pub fn contains(&self, value: u32) -> bool {
        let (k, low) = split(value);
        self.find(k).is_ok_and(|i| self.cref(i).contains(low))
    }

    pub fn cardinality(&self) -> u64 {
        (0..self.n()).map(|i| u64::from(self.card_at(i))).sum()
    }

    #[inline]
    pub fn len(&self) -> u64 {
        self.cardinality()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.n() == 0
    }

    pub fn minimum(&self) -> Option<u32> {
        (self.n() > 0).then(|| (u32::from(self.key(0)) << 16) | u32::from(self.cref(0).min().unwrap()))
    }

    pub fn maximum(&self) -> Option<u32> {
        let n = self.n();
        (n > 0).then(|| (u32::from(self.key(n - 1)) << 16) | u32::from(self.cref(n - 1).max().unwrap()))
    }

    /// Number of values <= `value`.
    pub fn rank(&self, value: u32) -> u64 {
        let (k, low) = split(value);
        let mut r = 0u64;
        for i in 0..self.n() {
            let key = self.key(i);
            if key < k {
                r += u64::from(self.card_at(i));
            } else {
                if key == k {
                    r += u64::from(self.cref(i).rank(low));
                }
                break;
            }
        }
        r
    }

    pub fn and_cardinality(&self, other: &Bitmap) -> u64 {
        and_card(self, other)
    }

    /// Whether the two share a value (stops at the first shared container value).
    pub fn intersects(&self, other: &Bitmap) -> bool {
        let (mut i, mut j) = (0usize, 0usize);
        while i < self.n() && j < other.n() {
            let (ka, kb) = (self.key(i), other.key(j));
            if ka < kb {
                i += 1;
            } else if kb < ka {
                j += 1;
            } else {
                if store::and_card(self.cref(i), other.cref(j)) > 0 {
                    return true;
                }
                i += 1;
                j += 1;
            }
        }
        false
    }

    /// A cursor answering the 0-based rank of values asked in increasing order.
    pub fn rank_cursor(&self) -> RankCursor<'_> {
        RankCursor { bm: self, i: 0, before: 0, at: 0, below: 0 }
    }

    pub fn or_cardinality(&self, other: &Bitmap) -> u64 {
        self.cardinality() + other.cardinality() - self.and_cardinality(other)
    }

    /// K-way union: containers are bucketed by key (no sort), each bucket combined in one pass.
    pub fn union_many<'a, I: IntoIterator<Item = &'a Bitmap>>(iter: I) -> Bitmap {
        let maps: Vec<&Bitmap> = iter.into_iter().filter(|b| !b.is_empty()).collect();
        match maps.len() {
            0 => return Bitmap::new(),
            1 => return maps[0].clone(),
            2 => return maps[0] | maps[1],
            _ => {}
        }
        let lo = maps.iter().map(|b| b.key(0)).min().unwrap() as usize;
        let hi = maps.iter().map(|b| b.key(b.n() - 1)).max().unwrap() as usize;
        let mut buckets: Vec<Vec<CRef<'_>>> = vec![Vec::new(); hi - lo + 1];
        for b in &maps {
            for (k, c) in b.containers() {
                buckets[k as usize - lo].push(c);
            }
        }
        let mut out: Vec<(u16, Store)> = Vec::new();
        for (offset, group) in buckets.iter().enumerate() {
            let store = match group.len() {
                0 => continue,
                1 => group[0].to_store(),
                2 => store::or(group[0], group[1]),
                _ => {
                    let mut words = group[0].to_words();
                    for c in &group[1..] {
                        c.or_into(&mut words);
                    }
                    let card = words.iter().map(|w| w.count_ones()).sum();
                    store::from_words(words, card).unwrap()
                }
            };
            out.push(((lo + offset) as u16, store));
        }
        Bitmap::from_entries(out)
    }

    /// OR every value into a dense bitset (value `v` is bit `v & 63` of `words[v >> 6]`). `words`
    /// covers whole containers: 1024 words per 16-bit key up to the largest key.
    pub fn or_into_words(&self, words: &mut [u64]) {
        for (k, c) in self.containers() {
            let at = usize::from(k) * store::WORDS;
            let chunk: &mut store::Words = (&mut words[at..at + store::WORDS]).try_into().unwrap();
            c.or_into(chunk);
        }
    }

    /// The set bits of a dense bitset (whole containers, as `or_into_words` fills).
    pub fn from_words(words: &[u64]) -> Bitmap {
        let mut out = Vec::new();
        for (k, chunk) in words.chunks(store::WORDS).enumerate() {
            let card: u32 = chunk.iter().map(|w| w.count_ones()).sum();
            if card == 0 {
                continue;
            }
            let mut w = vec![0u64; store::WORDS].into_boxed_slice();
            w[..chunk.len()].copy_from_slice(chunk);
            if let Some(s) = store::from_words(w.try_into().unwrap(), card) {
                out.push((k as u16, s.optimized()));
            }
        }
        Bitmap::from_entries(out)
    }

    pub fn iter(&self) -> Iter<'_> {
        Iter { bm: self, idx: 0, key: 0, cur: store::CIter::Array([].iter()) }
    }
}

/// Ranks of values asked in non-decreasing order. Containers before the value's are counted once
/// as the cursor passes them, and so is the part of the value's container before it: an array's
/// values, a bitset's words or a container's runs, so a value costs a step forward, not a search.
pub struct RankCursor<'a> {
    bm: &'a Bitmap,
    i: usize,
    before: u64,
    /// In container `i`: the array index, bitset word or run the cursor is at.
    at: usize,
    /// The container's values before `at` (bitset words and runs).
    below: u32,
}

impl RankCursor<'_> {
    /// The value's 0-based rank among the bitmap's values, None when it is not one of them.
    #[inline]
    pub fn rank(&mut self, value: u32) -> Option<u64> {
        let (k, low) = split(value);
        if self.i < self.bm.n() && self.bm.key(self.i) < k {
            while self.i < self.bm.n() && self.bm.key(self.i) < k {
                self.before += u64::from(self.bm.card_at(self.i));
                self.i += 1;
            }
            self.at = 0;
            self.below = 0;
        }
        if self.i >= self.bm.n() || self.bm.key(self.i) != k {
            return None;
        }
        match self.bm.cref(self.i) {
            CRef::Array(v) => {
                self.at = gallop(v, self.at, low);
                (v.get(self.at) == Some(&low)).then(|| self.before + self.at as u64)
            }
            CRef::Bitset(w, _) => {
                let word = (low >> 6) as usize;
                while self.at < word {
                    self.below += w[self.at].count_ones();
                    self.at += 1;
                }
                let bit = 1u64 << (low & 63);
                (w[word] & bit != 0).then(|| self.before + u64::from(self.below + (w[word] & (bit - 1)).count_ones()))
            }
            CRef::Run(r, _) => {
                while self.at < r.len() && u32::from(r[self.at][0]) + u32::from(r[self.at][1]) < u32::from(low) {
                    self.below += u32::from(r[self.at][1]) + 1;
                    self.at += 1;
                }
                let run = r.get(self.at).filter(|run| run[0] <= low)?;
                Some(self.before + u64::from(self.below + u32::from(low - run[0])))
            }
        }
    }
}

/// The first index from `from` on whose value is at least `low` (`v` ascending, `v[..from]` below
/// `low`): doubling steps, then a binary search inside the last one.
#[inline]
fn gallop(v: &[u16], from: usize, low: u16) -> usize {
    if from >= v.len() || v[from] >= low {
        return from;
    }
    let (mut lo, mut step) = (from, 1);
    while lo + step < v.len() && v[lo + step] < low {
        lo += step;
        step *= 2;
    }
    let hi = (lo + step + 1).min(v.len());
    lo + 1 + v[lo + 1..hi].partition_point(|&x| x < low)
}

impl Bitmap {

    pub fn to_vec(&self) -> Vec<u32> {
        let mut v = Vec::with_capacity(self.cardinality() as usize);
        self.iter().for_each(|x| v.push(x));
        v
    }

    /// The values from the largest down, while `f` returns true.
    pub fn for_each_rev(&self, mut f: impl FnMut(u32) -> bool) {
        for i in (0..self.n()).rev() {
            let high = u32::from(self.key(i)) << 16;
            if !self.cref(i).for_each_rev(|low| f(high | u32::from(low))) {
                return;
            }
        }
    }

    /// The `n` largest values, largest first.
    pub fn top_desc(&self, n: usize) -> Vec<u32> {
        let mut out = Vec::with_capacity(n.min(self.cardinality() as usize));
        if n > 0 {
            self.for_each_rev(|x| {
                out.push(x);
                out.len() < n
            });
        }
        out
    }

    /// Heap bytes held by this bitmap: owned containers, or the frozen bytes when they are a heap
    /// buffer (0 when they are a memory-mapped segment).
    pub fn heap_bytes(&self) -> usize {
        match &self.repr {
            Repr::Owned(e) => e.capacity() * std::mem::size_of::<(u16, Store)>() + e.iter().map(|(_, s)| s.heap_bytes()).sum::<usize>(),
            Repr::Frozen(f) if f.is_mapped() => 0,
            Repr::Frozen(f) => f.byte_len(),
        }
    }

    // -- frozen serialization --------------------------------------------------------------------

    /// Append the frozen form. `out.len()` is the absolute position in an 8-aligned buffer.
    pub fn write_frozen(&self, out: &mut Vec<u8>) {
        frozen::write(self.containers(), out);
    }

    pub fn serialize_portable(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.serialized_size());
        self.write_frozen(&mut out);
        out
    }

    pub fn serialize_into<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        writer.write_all(&self.serialize_portable())
    }

    pub fn serialized_size(&self) -> usize {
        frozen::size(self.containers())
    }

    /// Read frozen bytes into an aligned copy that the bitmap then reads in place.
    pub fn deserialize_portable(bytes: &[u8]) -> std::io::Result<Self> {
        Self::from_frozen(frozen::aligned_copy(bytes))
    }

    pub fn deserialize_from<R: Read>(mut reader: R) -> std::io::Result<Self> {
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf)?;
        Self::deserialize_portable(&buf)
    }
}

// -- binary operations -----------------------------------------------------------------------

/// Container access monomorphized per representation, so merge loops do not re-dispatch on
/// owned vs frozen at every step.
trait View {
    fn n(&self) -> usize;
    fn key(&self, i: usize) -> u16;
    fn cref(&self, i: usize) -> CRef<'_>;
}

impl View for [(u16, Store)] {
    #[inline]
    fn n(&self) -> usize {
        self.len()
    }
    #[inline]
    fn key(&self, i: usize) -> u16 {
        self[i].0
    }
    #[inline]
    fn cref(&self, i: usize) -> CRef<'_> {
        self[i].1.as_ref()
    }
}

impl View for frozen::Frozen {
    #[inline]
    fn n(&self) -> usize {
        self.len()
    }
    #[inline]
    fn key(&self, i: usize) -> u16 {
        frozen::Frozen::key(self, i)
    }
    #[inline]
    fn cref(&self, i: usize) -> CRef<'_> {
        frozen::Frozen::cref(self, i)
    }
}

macro_rules! dispatch2 {
    ($a:expr, $b:expr, $f:ident) => {
        match (&$a.repr, &$b.repr) {
            (Repr::Owned(x), Repr::Owned(y)) => $f(&x[..], &y[..]),
            (Repr::Owned(x), Repr::Frozen(y)) => $f(&x[..], y),
            (Repr::Frozen(x), Repr::Owned(y)) => $f(x, &y[..]),
            (Repr::Frozen(x), Repr::Frozen(y)) => $f(x, y),
        }
    };
}

fn and(a: &Bitmap, b: &Bitmap) -> Bitmap {
    Bitmap::from_entries(dispatch2!(a, b, and_v))
}

fn and_card(a: &Bitmap, b: &Bitmap) -> u64 {
    dispatch2!(a, b, and_card_v)
}

fn or(a: &Bitmap, b: &Bitmap) -> Bitmap {
    Bitmap::from_entries(dispatch2!(a, b, or_v))
}

fn andnot(a: &Bitmap, b: &Bitmap) -> Bitmap {
    Bitmap::from_entries(dispatch2!(a, b, andnot_v))
}

fn and_card_v<A: View + ?Sized, B: View + ?Sized>(a: &A, b: &B) -> u64 {
    let (mut i, mut j, mut n) = (0usize, 0usize, 0u64);
    while i < a.n() && j < b.n() {
        match a.key(i).cmp(&b.key(j)) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                n += u64::from(store::and_card(a.cref(i), b.cref(j)));
                i += 1;
                j += 1;
            }
        }
    }
    n
}

fn and_v<A: View + ?Sized, B: View + ?Sized>(a: &A, b: &B) -> Vec<(u16, Store)> {
    let (mut i, mut j) = (0usize, 0usize);
    let mut out = Vec::with_capacity(a.n().min(b.n()));
    while i < a.n() && j < b.n() {
        match a.key(i).cmp(&b.key(j)) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                if let Some(s) = store::and(a.cref(i), b.cref(j)) {
                    out.push((a.key(i), s));
                }
                i += 1;
                j += 1;
            }
        }
    }
    out
}

fn or_v<A: View + ?Sized, B: View + ?Sized>(a: &A, b: &B) -> Vec<(u16, Store)> {
    let (mut i, mut j) = (0usize, 0usize);
    let mut out = Vec::with_capacity(a.n() + b.n());
    while i < a.n() || j < b.n() {
        if j >= b.n() || (i < a.n() && a.key(i) < b.key(j)) {
            out.push((a.key(i), a.cref(i).to_store()));
            i += 1;
        } else if i >= a.n() || b.key(j) < a.key(i) {
            out.push((b.key(j), b.cref(j).to_store()));
            j += 1;
        } else {
            out.push((a.key(i), store::or(a.cref(i), b.cref(j))));
            i += 1;
            j += 1;
        }
    }
    out
}

fn andnot_v<A: View + ?Sized, B: View + ?Sized>(a: &A, b: &B) -> Vec<(u16, Store)> {
    let mut out = Vec::with_capacity(a.n());
    let mut j = 0usize;
    for i in 0..a.n() {
        let k = a.key(i);
        while j < b.n() && b.key(j) < k {
            j += 1;
        }
        if j < b.n() && b.key(j) == k {
            if let Some(s) = store::andnot(a.cref(i), b.cref(j)) {
                out.push((k, s));
            }
        } else {
            out.push((k, a.cref(i).to_store()));
        }
    }
    out
}

/// `a |= b`, reusing a's owned containers.
fn or_assign(a: &mut Bitmap, b: &Bitmap) {
    if b.is_empty() {
        return;
    }
    if a.is_empty() {
        *a = b.clone();
        return;
    }
    let mine = std::mem::take(a.owned());
    let mut out = Vec::with_capacity(mine.len() + b.n());
    let mut j = 0usize;
    for (k, mut s) in mine {
        while j < b.n() && b.key(j) < k {
            out.push((b.key(j), b.cref(j).to_store()));
            j += 1;
        }
        if j < b.n() && b.key(j) == k {
            store::or_assign(&mut s, b.cref(j));
            j += 1;
        }
        out.push((k, s));
    }
    while j < b.n() {
        out.push((b.key(j), b.cref(j).to_store()));
        j += 1;
    }
    a.repr = Repr::Owned(out);
}

impl std::ops::BitOrAssign<&Bitmap> for Bitmap {
    #[inline]
    fn bitor_assign(&mut self, rhs: &Bitmap) {
        or_assign(self, rhs);
    }
}

impl std::ops::BitOrAssign<Bitmap> for Bitmap {
    #[inline]
    fn bitor_assign(&mut self, rhs: Bitmap) {
        or_assign(self, &rhs);
    }
}

impl std::ops::BitAndAssign<&Bitmap> for Bitmap {
    #[inline]
    fn bitand_assign(&mut self, rhs: &Bitmap) {
        *self = and(self, rhs);
    }
}

impl std::ops::BitAndAssign<Bitmap> for Bitmap {
    #[inline]
    fn bitand_assign(&mut self, rhs: Bitmap) {
        *self = and(self, &rhs);
    }
}

impl std::ops::SubAssign<&Bitmap> for Bitmap {
    #[inline]
    fn sub_assign(&mut self, rhs: &Bitmap) {
        *self = andnot(self, rhs);
    }
}

impl std::ops::SubAssign<Bitmap> for Bitmap {
    #[inline]
    fn sub_assign(&mut self, rhs: Bitmap) {
        *self = andnot(self, &rhs);
    }
}

macro_rules! binop {
    ($tr:ident, $m:ident, $f:ident) => {
        impl std::ops::$tr<&Bitmap> for &Bitmap {
            type Output = Bitmap;
            #[inline]
            fn $m(self, rhs: &Bitmap) -> Bitmap {
                $f(self, rhs)
            }
        }
        impl std::ops::$tr<Bitmap> for Bitmap {
            type Output = Bitmap;
            #[inline]
            fn $m(self, rhs: Bitmap) -> Bitmap {
                $f(&self, &rhs)
            }
        }
        impl std::ops::$tr<&Bitmap> for Bitmap {
            type Output = Bitmap;
            #[inline]
            fn $m(self, rhs: &Bitmap) -> Bitmap {
                $f(&self, rhs)
            }
        }
        impl std::ops::$tr<Bitmap> for &Bitmap {
            type Output = Bitmap;
            #[inline]
            fn $m(self, rhs: Bitmap) -> Bitmap {
                $f(self, &rhs)
            }
        }
    };
}

binop!(BitAnd, bitand, and);
binop!(BitOr, bitor, or);
binop!(Sub, sub, andnot);

// -- iteration -------------------------------------------------------------------------------

pub struct Iter<'a> {
    bm: &'a Bitmap,
    idx: usize,
    key: u32,
    cur: store::CIter<'a>,
}

impl Iterator for Iter<'_> {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<u32> {
        loop {
            if let Some(low) = self.cur.next() {
                return Some(self.key | u32::from(low));
            }
            if self.idx >= self.bm.n() {
                return None;
            }
            self.key = u32::from(self.bm.key(self.idx)) << 16;
            self.cur = self.bm.cref(self.idx).iter();
            self.idx += 1;
        }
    }

    /// Internal iteration: one tight loop per container (sum, count, for_each, to_vec).
    fn fold<B, F: FnMut(B, u32) -> B>(mut self, init: B, mut f: F) -> B {
        let mut acc = init;
        while let Some(low) = self.cur.next() {
            acc = f(acc, self.key | u32::from(low));
        }
        for i in self.idx..self.bm.n() {
            let key = u32::from(self.bm.key(i)) << 16;
            match self.bm.cref(i) {
                CRef::Array(v) => {
                    for &x in v {
                        acc = f(acc, key | u32::from(x));
                    }
                }
                CRef::Bitset(w, _) => {
                    for (wi, &word) in w.iter().enumerate() {
                        let mut word = word;
                        let base = key | ((wi as u32) << 6);
                        while word != 0 {
                            acc = f(acc, base | word.trailing_zeros());
                            word &= word - 1;
                        }
                    }
                }
                CRef::Run(r, _) => {
                    for run in r {
                        let s = key | u32::from(run[0]);
                        for x in s..=s + u32::from(run[1]) {
                            acc = f(acc, x);
                        }
                    }
                }
            }
        }
        acc
    }
}

impl FromIterator<u32> for Bitmap {
    fn from_iter<I: IntoIterator<Item = u32>>(iter: I) -> Self {
        let mut v: Vec<u32> = iter.into_iter().collect();
        v.sort_unstable();
        v.dedup();
        let mut out: Vec<(u16, Store)> = Vec::new();
        let mut i = 0;
        while i < v.len() {
            let k = (v[i] >> 16) as u16;
            let mut j = i;
            while j < v.len() && (v[j] >> 16) as u16 == k {
                j += 1;
            }
            let lows: Vec<u16> = v[i..j].iter().map(|&x| x as u16).collect();
            let s = if lows.len() <= store::ARRAY_MAX {
                Store::Array(lows)
            } else {
                let mut words: Box<store::Words> = vec![0u64; store::WORDS].into_boxed_slice().try_into().unwrap();
                for &x in &lows {
                    words[(x >> 6) as usize] |= 1u64 << (x & 63);
                }
                Store::Bitset(words, lows.len() as u32)
            };
            out.push((k, s));
            i = j;
        }
        Bitmap::from_entries(out)
    }
}

impl Extend<u32> for Bitmap {
    fn extend<I: IntoIterator<Item = u32>>(&mut self, iter: I) {
        for v in iter {
            self.add(v);
        }
    }
}

impl<'a> Extend<&'a u32> for Bitmap {
    fn extend<I: IntoIterator<Item = &'a u32>>(&mut self, iter: I) {
        for v in iter {
            self.add(*v);
        }
    }
}

impl std::fmt::Debug for Bitmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bitmap(len={})", self.cardinality())
    }
}

impl PartialEq for Bitmap {
    fn eq(&self, other: &Self) -> bool {
        self.n() == other.n()
            && (0..self.n()).all(|i| self.key(i) == other.key(i) && self.card_at(i) == other.card_at(i))
            && self.iter().eq(other.iter())
    }
}

impl Eq for Bitmap {}

#[cfg(test)]
mod tests {
    use super::*;
    use roaring::RoaringBitmap;

    fn rng(s: &mut u64) -> u64 {
        *s ^= *s << 13;
        *s ^= *s >> 7;
        *s ^= *s << 17;
        *s
    }

    /// Random bitmap mixing sparse, dense and run-heavy containers across several keys.
    fn random(seed: &mut u64) -> Vec<u32> {
        let mut v = Vec::new();
        for _ in 0..1 + rng(seed) % 6 {
            let key = (rng(seed) % 8) as u32;
            match rng(seed) % 3 {
                0 => (0..rng(seed) % 300).for_each(|_| v.push((key << 16) | (rng(seed) as u32 & 0xFFFF))),
                1 => (0..5000 + rng(seed) % 20000).for_each(|_| v.push((key << 16) | (rng(seed) as u32 & 0xFFFF))),
                _ => {
                    let a = rng(seed) as u32 & 0xFFFF;
                    let len = rng(seed) as u32 % 40000;
                    v.extend(((key << 16) | a)..((key << 16) | a).saturating_add(len).min((key << 16) | 0xFFFF));
                }
            }
        }
        v
    }

    fn both(v: &[u32], freeze: bool) -> (Bitmap, RoaringBitmap) {
        let mut b: Bitmap = v.iter().copied().collect();
        b.run_optimize();
        if freeze {
            b = Bitmap::deserialize_portable(&b.serialize_portable()).unwrap();
            assert!(b.is_frozen());
        }
        (b, v.iter().copied().collect())
    }

    #[test]
    fn differential_against_roaring_rs() {
        let mut seed = 0xD1B5_4A32_D192_ED03u64;
        for round in 0..300 {
            let (fa, fb) = (round % 2 == 0, round % 3 == 0);
            let (va, vb) = (random(&mut seed), random(&mut seed));
            let (a, ra) = both(&va, fa);
            let (b, rb) = both(&vb, fb);
            assert_eq!(a.cardinality(), ra.len());
            assert_eq!(a.to_vec(), ra.iter().collect::<Vec<_>>());
            assert_eq!((&a & &b).to_vec(), (&ra & &rb).iter().collect::<Vec<_>>());
            assert_eq!((&a | &b).to_vec(), (&ra | &rb).iter().collect::<Vec<_>>());
            assert_eq!((&a - &b).to_vec(), (&ra - &rb).iter().collect::<Vec<_>>());
            assert_eq!(a.and_cardinality(&b), ra.intersection_len(&rb));
            assert_eq!(a.or_cardinality(&b), ra.union_len(&rb));
            assert_eq!(a.minimum(), ra.min());
            assert_eq!(a.maximum(), ra.max());
            for probe in [0u32, 65535, 65536, 131071, va.first().copied().unwrap_or(7), 7 << 16 | 12345] {
                assert_eq!(a.contains(probe), ra.contains(probe));
                assert_eq!(a.rank(probe), ra.rank(probe));
            }
            let mut c = a.clone();
            c |= &b;
            assert_eq!(c, &a | &b);
            let mut d = a.clone();
            d &= &b;
            assert_eq!(d, &a & &b);
            let mut e = a.clone();
            e -= &b;
            assert_eq!(e, &a - &b);
            let u = Bitmap::union_many([&a, &b, &d]);
            assert_eq!(u.to_vec(), (&ra | &rb).iter().collect::<Vec<_>>());
            let mut f = a.clone();
            if let Some(&x) = va.first() {
                f.remove(x);
                f.add(x ^ 1);
                let mut rf = ra.clone();
                rf.remove(x);
                rf.insert(x ^ 1);
                assert_eq!(f.to_vec(), rf.iter().collect::<Vec<_>>());
            }
            let back = Bitmap::deserialize_portable(&a.serialize_portable()).unwrap();
            assert_eq!(back, a);
            assert_eq!(a.serialize_portable().len(), a.serialized_size());
        }
    }

    #[test]
    fn rank_cursor_against_roaring_rs() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        for round in 0..300 {
            let va = random(&mut seed);
            let (a, ra) = both(&va, round % 2 == 0);
            // the bitmap's values (some), values it lacks, and repeats, ascending as text scoring asks
            let mut asked: Vec<u32> = va.iter().copied().filter(|_| rng(&mut seed) % 3 == 0).collect();
            asked.extend(random(&mut seed).into_iter().filter(|_| rng(&mut seed) % 4 == 0));
            asked.extend([0, 65535, 65536, u32::MAX]);
            asked.sort_unstable();
            let repeats: Vec<u32> = asked.iter().copied().filter(|_| rng(&mut seed) % 5 == 0).collect();
            asked.extend(repeats);
            asked.sort_unstable();
            let mut cursor = a.rank_cursor();
            for &x in &asked {
                assert_eq!(cursor.rank(x), ra.contains(x).then(|| ra.rank(x) - 1), "round {round}, value {x}");
            }
        }
    }

    #[test]
    fn ranges_and_frozen_alignment() {
        let mut b = Bitmap::new();
        b.add_range(10..200_000);
        b.add_range(70_000..=70_010);
        b.add(5);
        assert_eq!(b.cardinality(), 1 + 199_990);
        b.run_optimize();
        let bytes = b.serialize_portable();
        // 4 containers: 40-byte header + run containers of 10, 6, 6 and 6 bytes.
        assert_eq!(bytes.len(), 68);
        // Misaligned source buffer: parse copies once into an aligned buffer.
        let mut shifted = vec![0u8];
        shifted.extend_from_slice(&bytes);
        let f = Bitmap::deserialize_portable(&shifted[1..]).unwrap();
        assert_eq!(f, b);
        assert_eq!(f.heap_bytes(), 68, "a heap copy counts as heap");
        let mut g = f.clone();
        g.remove(100);
        assert!(!g.contains(100) && f.contains(100));
        let mut all = Bitmap::new();
        all.add_range(..);
        assert_eq!(all.cardinality(), 1u64 << 32);
        assert!(Bitmap::deserialize_portable(&[1, 0, 0, 0]).is_err());
    }
}
