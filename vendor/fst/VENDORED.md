# fst 0.4.7, Torque's copy

BurntSushi/fst 0.4.7 (crates.io, commit fbd5992), used by Torque through `[patch.crates-io]` in the
root `Cargo.toml`. The crate's `data/`, `tests/`, `scripts/` and its dev-dependencies are left out;
`src/` is as published but for the changes below.

## Changes

- `raw::Builder::memory_sized(keys)`, `raw::Builder::new_type_sized(wtr, ty, keys)`,
  `MapBuilder::memory_sized(keys)` and `MapBuilder::new_sized(wtr, keys)` (`src/raw/build.rs`,
  `src/map.rs`): a builder whose node registry is sized to about `keys` keys, eight rows (16
  cells) per key, at most the 10,000 rows every builder had. `memory` and `new` are unchanged.
- `src/raw/mod.rs`: `FstRef<'_>` in `as_ref` (a lint of newer compilers; a registry dependency's
  lints are capped, a path dependency's are not).

## Why

Every builder set up a registry of 10,000 rows of two cells, about 1 MB, before its first key and
dropped it after. That is about 17 us per FST on the server's allocator, more than building an FST
of a few hundred keys. Torque builds several small FSTs for each realtime write and each merge of
small segments (each text field's terms, IDS, string values), so it was about 120 us of every
write's build.

The registry is a cache of compiled nodes that later keys may share: it changes how many nodes an
FST shares, never what it maps. A sized builder maps the same keys to the same values for any
`keys` (`tqbf::ids` tests).

Measured (release, mimalloc, one FST of string values to ordinals):

| Keys | 10,000 rows | Sized (8 per key) |
|---|---|---|
| 10 words | 171 B, 20.9 us | 173 B, 3.4 us |
| 100 words | 993 B, 35.9 us | 993 B, 17.6 us |
| 1,000 words | 6,479 B, 119 us | 6,479 B, 104 us |
| 10 two-word names | 234 B, 24.1 us | 234 B, 6.1 us |
| 99 two-word names | 1,295 B, 55.7 us | 1,301 B, 41.5 us |
| 1,000 ids | 7,130 B, 165 us | 7,138 B, 150 us |

At 1,250 keys and more the sized registry is the 10,000 rows it always was.

In Torque (the `index::merge` `merge_speed` test of the time, the same build with the registry
forced back to 10,000 rows): a realtime write of one document built its segment in 239 to 246 us instead of 344 to 354 us,
and a merge of ten one-document segments took 0.26 ms instead of 0.38 ms.

## Licence

Dual-licensed under the Unlicense and MIT, as upstream (`COPYING`, `LICENSE-MIT`, `UNLICENSE`).
