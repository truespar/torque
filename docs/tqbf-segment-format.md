# TQBF v6: Torque segment format

Status: specification, version 6.
Replaces every earlier on-disk and transport format (TQBF v1-v5, the `gen_{G}/seg_{N}` directory
layout, the rkyv/TRQX component files). No backward compatibility: a file of another version is
refused, and every collection is built again.

## 1. Goals

1. **One file is one segment, and the transport file is the serving file.** A nightly job produces a TQBF
   file; the server writes it to disk as received, checks its checksums, maps it and swaps an alias.
   The server does not decode documents, build indexes or parse JSON for that collection.
2. **Smallest possible transfer.** Every section uses an encoding that is both compact and queryable in place
   (FST, frozen roaring, bitpacked arrays). Generic compression is applied only where a section is read in
   blocks (the document store) or, optionally, for transport.
3. **RAM holds only what the OS decides is hot.** Every section is read in place from a memory-mapped file.
   Nothing is deserialized into the heap at load time except the field registry (a few KB).
4. **Documents come back exactly as Typesense returns them.** The stored record keeps every field the
   client sent, including fields that are not indexed and fields that are not in the search schema.
5. **JSON exists only at the HTTP edge**: parsing client requests, and rendering the hits of a response.
6. **Two use cases, one format.** Collections built once (nightly) are a set of segment files. Collections
   that change are the same segment files plus a write-ahead log and an in-memory overlay that compaction
   turns into new segment files.

## 2. Measured basis (1,000 random company documents)

| Encoding of the stored document | Bytes/doc | 3.78M companies |
|---|---|---|
| Typesense JSON (as stored and returned by Typesense) | 6,715 | 25.4 GB |
| Binary record, §5.2 | 2,871 | 10.9 GB |
| Records in 16 KB zstd-19 blocks, trained 16 KB dictionary | 709 | 2.7 GB |
| Records in 64 KB zstd-19 blocks, trained dictionary | 672 | 2.5 GB |
| JSON in 64 KB zstd-19 blocks (for comparison) | 978 | 3.7 GB |
| PAX (values grouped per field inside a 16 KB block) + dictionary | 706 | 2.7 GB |

PAX gains 3-6% over row records once a dictionary is used, and makes a single-document read slower,
so the document store uses row records. 16 KB blocks cost 5% more than 64 KB blocks and decompress a
quarter of the bytes per returned hit.

Of the 2,813 value bytes in an average document, 62% belong to indexed fields, 30% to `index:false`
fields and 8% to 81 paths that are not in the Typesense schema at all. This format keeps all three in
the document store only.

## 3. File layout

All integers little-endian. All sections start at a 64-byte boundary; sections larger than 64 KB start at
a 4 KB boundary so they can be advised per page range. Every array inside a section is naturally aligned
for its element type, so it can be read as `&[u16]`, `&[u32]`, `&[u64]` without copying.

```
offset 0      header (64 bytes)
                magic        "TQBF"            4
                version      6                 u16
                flags                          u16   bit0 = has index sections, bit1 = transport-compressed sections present
                doc_count                      u32
                section_count                  u32
                schema_hash  xxh3-64 of FIELDS u64
                created_unix                   i64
                table_offset                   u64
                table_xxh3                     u64
                reserved                       to 64
table         section_count x 40 bytes
                kind                           u16   (§4)
                field_id                       u16   (0xFFFF = segment-wide)
                codec                          u8    0 = stored as is, 1 = zstd frame (transport only)
                reserved                       3
                offset                         u64
                length                         u64   bytes in this file
                raw_length                     u64   bytes after the transport codec is removed
                xxh3                           u64   of the raw bytes
sections      ...
```

`table_offset` points anywhere in the file: the in-memory writer puts the table right after the header,
the streaming writer (`StreamingSegmentWriter`, used by the builder of §10) after the last section, so
it writes each section once as it is produced and the header last.

`codec = 1` exists only in transit. The receiving server streams each such section through zstd into its
final place, recomputes the checksum and rewrites the table with `codec = 0`. Decompression runs at more
than 1 GB/s per core and is the only work the server does on upload. Sections that are already compact
(DOCS, FST, bitpacked arrays) are sent with `codec = 0`.

## 4. Section kinds

| kind | name | per | content |
|---|---|---|---|
| 1 | FIELDS | segment | field registry: every document path, its id and schema flags (§5.1) |
| 2 | DOCS | segment | document store: block index, zstd dictionary, record blocks (§5.2) |
| 3 | IDS | segment | FST external id -> doc id (§5.3) |
| 4 | SCHEMA | segment | the collection schema, as `POST /collections` takes it (§5.1) |
| 5 | RECORDS | docs-only file | the documents a client wrote, in zlib chunks (§10.1) |
| 6 | SEQ | segment | the documents' `_seq_id`s of a segment realtime writes built: `[u32 runs][runs × u32 first doc][runs × u64 first id]` (absent: doc `d` is `base + d`) |
| 10 | TERMS | text field | FST term -> term ordinal, ordinals by document frequency (§6.1) |
| 11 | RAWTERMS | text field | FST "lower-cased token \0 term" -> ordinal, for prefixes as written (§6.1) |
| 12 | POSTINGS | text field | per posting: document frequency + frozen roaring; doc ids of single-document terms (§6.2) |
| 13 | FREQS | text field | per posting: term frequency - 1, bit-packed at the posting's width (§6.2) |
| 14 | NORMS | text field | field lengths (Lucene SmallFloat), bit-packed, dense or sparse; the field's total length (§6.3) |
| 20 | VALUES | string field | FST value -> value ordinal, ordinals in byte order (§7.1) |
| 21 | VALUE_DOCS | filter, facet or sort field | per value ordinal: the doc id of a single-document value, else a frozen roaring (§7.1) |
| 22 | DOC_ORDS | facet or sort field | per doc: its value ordinal, or the ordinals of its array elements (§7.1) |
| 30 | NUM_VALUES | numeric field | the distinct values in order, monotonic bit-packed (§7.2) |
| 40 | BOOL | bool field | frozen roaring true, frozen roaring false (§7.5) |
| 50 | GEO | geopoint field | packed Hilbert R-tree over the points, per doc point positions (§7.6) |
| 60 | VECTORS | float[] vector field | f32 vectors, HNSW layers as CSR neighbor lists, RaBitQ codes (§7.7) |

A field only has the sections its flags need. `index:false` fields and stored-only paths have none:
they exist only inside DOCS records. Reference fields have no section of their own: their value index
(§7.1) is the JOIN index (§7.8).

## 5. Segment-wide sections

### 5.1 FIELDS and SCHEMA

FIELDS has one entry per JSON path that appears in any document of the collection, in id order:

```
u32 count
entries[count]:
  u16 parent               id of the enclosing object path, 0xFFFF for top level (always lower)
  u16 flags                bit0 in_schema, bit1 index, bit2 facet, bit3 sort, bit4 optional,
                           bit5 stored_only, bit6 infix, bit7 stem, bit8 reference
  u8  value_type           Typesense type of a schema field (0 = not a schema field)
  u16 name_length, name    last path component, UTF-8
```

- Fields in the Typesense schema have `in_schema`; paths that only appear in documents have
  `stored_only` and are never indexed or type-checked. This is Lucene's FieldInfos / Meilisearch's
  FieldsIdsMap: every stored field has a number, flags say what it is used for. Ids only grow, so
  records written against an older registry stay readable.
- A writer may list its paths as `stored_only` in any order; the builder applies the schema's flags.
- Keys are rendered in byte order of their names (the order nlohmann::json writes, §8); the names
  sort at render time, which costs less than a stored order per object path.

SCHEMA is the collection schema as JSON, exactly what `POST /collections` takes (fields with every
attribute, token_separators, symbols_to_index, default_sorting_field, metadata, ...). It is read once,
when an uploaded segment creates its collection (§10.3), never on a query; keeping Typesense's own
schema form avoids a second encoding of every attribute that would have to track Typesense. A few KB
per segment.

### 5.2 DOCS: document store

```
u32 block_count, u32 dict_length
u32 first_doc[block_count + 1]     first doc id of each block; last entry = doc_count
                                   padded to 8
u64 block_offset[block_count + 1]  byte offset of each block after the dictionary; last entry = end
dict bytes                         zstd dictionary trained on this collection's records (16 KB; none
                                   for a few records)
blocks                             zstd frames compressed with the dictionary
```

A decompressed block (target 16 KB of records):

```
u32 n_docs
u32 record_offset[n_docs + 1]      offsets after the table
records
```

Blocks are independent frames. A merge copies a full block (cut at 16 KB) as it is when all its
documents come over side by side and the merged segment uses its dictionary. A realtime
collection's segments share one dictionary (the largest segment's; the first flush with enough
records trains it; a write's own segment trains none), so its blocks carry from merge to merge
(docs/realtime-write-path.md, Merging from sections).

Record encoding (protobuf-style, keyed by registry id; no key names):

```
record      := varint(id_len) id_bytes  object          (the top-level `id` is not in the object)
object      := varint(n) entry{n}                         entries in key byte order
entry       := varint(field_id << 4 | tag) value
tag 0 int   := varint(zigzag(i64))
tag 1 float := 8 bytes, exact IEEE-754 bits
tag 2 str   := varint(len) UTF-8
tag 3 false, tag 4 true, tag 7 null   (no payload)
tag 5 array := varint(count) [u8 element_tag  element{count}]   element_tag omitted when count = 0;
               element_tag 9: bool elements, one byte each; element_tag 15: mixed types, every
               element carries its own tag; object elements are `object` whose keys are children
               of the array's path
tag 6 obj   := object      (entries use child ids of that object path)
tag 8 u64   := varint(u64) for integers above i64::MAX
```

Numbers are classified the way Torque's JSON import (serde_json) reads JSON text: integer text as
i64, else u64, else f64; `-0` and integers below i64::MIN as f64; text with a fraction or an exponent
as f64, correctly rounded. A repeated key keeps its last value.

- Empty arrays are kept (`count = 0`); absent fields are absent. That is exactly what Typesense returns.
- Integers and floats stay distinct (`40` vs `40.0`), and floats keep their exact bits, so rendering
  reproduces Typesense byte for byte (§8).
- Reading one document: binary-search `first_doc`, decompress one block (cached per block), slice the
  record by `record_offset`, walk it. Search, filter, facet and sort never touch DOCS; only the returned
  hits, GET by id and highlighting of returned hits do.
- Block cache: decompressed blocks in a bounded LRU (default 256 MB, per server). Compressed blocks are
  served from the page cache.

Rejected: zero-copy record formats (FlatBuffers, Cap'n Proto, rkyv): 1.5-2x larger because of alignment
and offset tables, and only the returned hits are ever read. JSON blocks: 38% larger after compression and
need parsing. RocksDB: an LSM tree adds levels, bloom filters, compaction and write amplification to a
store that is immutable; a lookup here is one array search and one block read.

### 5.3 IDS

FST (BurntSushi `fst` crate) mapping external id bytes to doc id. Read in place. The external id is also
the first item of each record, so doc id -> external id needs no extra structure.

### 5.4 Doc ids

A segment's doc ids are `0..doc_count`, all live; no section lists them. Deletions of mutable
collections are separate frozen roaring files referenced by the manifest (§9), never edits of the
segment file.

## 6. Text fields

A text query takes Typesense's parameters with their documented meaning; `text_match` and the order of
hits are Torque's own (`src/search/text.rs`, `src/search/scoring.rs`). A token is what lies between
whitespace and `token_separators`; its term is the token lower-cased, folded to ASCII through `deunicode`
without its punctuation (`symbols_to_index` stay), then stemmed when the field asks (`src/text`).

Per query word and field the candidates are the word's term, then the `max_candidates` most frequent
completions of the last word, and by rising cost its typo corrections and a name-matching field's keys.
A word matches a document when one of its candidates is in one of the fields; a document holds every word
of a query. The cost rises while fewer results than `typo_tokens_threshold` are found; with nothing
found words are joined and split; words are dropped while fewer than `drop_tokens_threshold` are found.

`text_match` packs, most significant first: words matched, typos, completions, whether the words are a
field's whole value, fields matched, the BM25F score, the field's weight (`scoring::Rank`).
`text_match_info` reads them back.

- TERMS, RAWTERMS: the term dictionary, the candidates come from it (§6.1);
- POSTINGS, FREQS: the documents of a term and its frequency in each (§6.2);
- NORMS: each document's field length (§6.3).

Positions are not indexed. A `"phrase"` takes the documents holding every term from the postings and
reads the order from the stored text, one block decompressed per candidate at worst, within the search's
cutoff; `:=` and filter phrases do the same unless the field's values or lengths answer them (§7.1).
`prioritize_token_position` has no effect.

A collection of several segments (a large batch collection, a realtime collection's writes and
merges) is searched as one: candidates, thresholds and dropping words are decided over all segments, and
a term's frequency, the document count and the average field length are those of the live documents, so
a search over any split of the documents gives what one segment holding them gives, `text_match`
included. A document deleted or replaced since its segment was built counts nowhere: a term's frequency
is its posting less the deleted documents (one intersection with the deletion mask, only where there is
one), and the lengths of a segment's deleted copies are summed once per mask (`Segment::deleted_lengths`).
Each segment scores, sorts, facets and groups its own documents, and the merge (by sort keys, then the
later document; facets counted over every value; groups by their values, with a second round for a page
group's documents in a segment where the group ranked below the page) completes the response.

### 6.1 TERMS and RAWTERMS

Terms are numbered by document frequency, most frequent first, ties in byte order. Ordinals
`0..n_postings` are the terms in two or more documents; the rest occur in exactly one document.

TERMS: FST per field, term -> ordinal.

- A term in one document has no posting: POSTINGS holds only its doc id, bit-packed at the bits the
  segment's doc count needs (Lucene's singleton doc id). Unique tokens (organization numbers, street
  names, rare name parts) are a large share of a company vocabulary; each would otherwise cost an offset,
  a document frequency, a bitmap header and a container.
- Because the FST outputs are frequency-ordered ordinals, the most frequent terms under a prefix come out
  of a best-first walk first (below).

Exact lookup, prefix and typo search all run on the FST in place:

- typo: Levenshtein automaton (Schulz-Mihov parametric DFA, `levenshtein_automata` crate as in Tantivy,
  distance at most 2, an adjacent transposition counts as one edit) intersected with the FST. A word's
  corrections at a cost are the `max_candidates` most frequent terms at that distance. Tokens longer than
  64 characters get no corrections (the automaton grows with the token).
- prefix (last query word): the lower-cased but not accent-folded word is matched against TERMS and
  RAWTERMS, so "trä" finds "trädgård" but not "transport", "tra" finds both, and a word stemmed away
  is found as written ("runni" finds the term "run" through the token "running"). They come from a
  best-first walk of both FSTs, merged by ordinal. The FST builder pushes outputs toward the root (a
  transition carries the smallest output below it), so the output accumulated at a node is the smallest
  ordinal under it, and the walk reaches the k most frequent terms in about k times the key length steps
  instead of scanning every term under the prefix. This is the technique of Lucene's FST suggesters.

RAWTERMS: FST of keys `lowered 0x00 term` -> ordinal of the term, only for tokens that differ from their
term (accent folding, stemming); absent when a field has none. Putting the term in the key lets a prefix
match yield its term without a reverse lookup, and lets one token map to several terms (documents of
different languages stem differently).

RAWTERMS is the one text section a merge cannot make exactly as a build would: a token form is not
recorded per document, so a merge keeps the forms of every term it carries over, also one only deleted
copies held. The only effect is that a prefix of such a form finds the term's other documents. The form
goes with the term, or with the next build.

### 6.2 POSTINGS and FREQS

POSTINGS: `u64 n_postings`, `u64 n_singles`, `u64 offset[n_postings + 1]` (bitmap starts relative to the
section), `u32 df[n_postings]`, `u32 doc_width`, `u32 0`, the doc ids of the single-document terms
bit-packed at `doc_width` (in ordinal order, 8 zero bytes of slack, padded to 8), then one frozen roaring
bitmap per posting, each starting on a 4-byte boundary. `df` makes the document frequency an O(1) read
for IDF. A large field's posting terms encode in parallel runs of about 256 KiB documents; a run's bitmaps
start on an 8-byte boundary. Runs are cut by the terms' documents, so the sections are the same on every
machine.

Frozen roaring layout (one bitmap, starting on a 4-byte boundary):

```
u32 n                 containers
u32 offset[n]         container start, relative to the bitmap start
u16 key[n]            high 16 bits, strictly increasing
u16 card_minus_1[n]
u8  kind[n]           0 array, 1 bitset, 2 run
containers            array:  u16 values[card]
                      bitset: u64 words[1024]
                      run:    u16 n_runs, (u16 start, u16 len_minus_1)[n_runs]
```

No padding inside the header. Containers are aligned absolutely within the section (u16 data
to 2 bytes, bitsets to 8), so a section on an 8-byte boundary is read as `&[u16]` and
`&[u64; 1024]` without copying; bytes that arrive misaligned are copied once. Each container is
written in the smallest of the three encodings. Parsing validates every offset, kind, size, key
order and run before any in-place read.

This is the roaring portable format reorganized as CRoaring's "frozen" format: headers first, every
container aligned, so a bitmap is used directly from the mapped bytes. Torque gets its own container
algebra (and, or, andnot, xor, cardinality, rank, contains, iterate, multi-way union) that works on
borrowed (mapped) and owned containers alike; owned results use the same container representation.
This replaces the `roaring` crate, which can only deserialize into the heap (`src/bitmap/`).
Measured against roaring-rs 0.11 on 3.78M-doc posting shapes
(`benches/bitmap_bench.rs`): at par or faster on 21 of 25 cases, e.g. iteration 0.66x the time,
k-way union 0.69x, andnot on sparse postings 0.55x, bitset/run counts 0.47x; within run-to-run noise
on the rest.

FREQS: `tf - 1` of every posting, in groups:

```
u64 n_postings
u64 n_singles
u64 group_bit[n_postings + 2]   start of each group in the packed bits (the singles' group last), then the end
u8  width[n_postings + 1]       bits per value of each group, at most 16
                                padded to 8
packed bits, LSB first
8 zero bytes of slack
```

Posting `p` (ordinal `p`) is group `p`, one value per document in doc order; the single-document terms
share the last group, in ordinal order. A width of 0 means every frequency is 1, so most postings of short
fields (names, cities, codes) take no bytes. The runs of §6.2 each start on a 64-bit boundary, so the last
group of a run can be followed by a few zero bits. Every read is one unaligned 8-byte load, a shift and a
mask; a document's frequency is `rank` in the posting bitmap and one read, and a scorer going through
ascending documents keeps a rank cursor.

Rejected for now: Lucene/PISA-style bitpacked doc-id blocks with skip lists. Torque's query engine
(filters, facets, candidate sets, the AND of each word's OR over fields) works on bitmaps, and roaring is
the standard for bitmap indexes (Druid, Pinot, ClickHouse, Lucene's IndexedDISI).

### 6.3 NORMS

The field length of each document in tokens, as Lucene's SmallFloat `intToByte4` norm (exact below 24,
then 4 significant bits; 0 = no tokens), stored like Lucene's norms: dense or sparse, at the width the
field needs.

```
u64 total_length         the lengths the norms stand for, summed
u32 doc_count
u32 docs_with_tokens
u8  width                bits per stored norm, 0-8
u8  min                  sparse: stored value = norm - min
u8  sparse
5 zero bytes
sparse only:  u32 bitmap_len, u32 0, frozen roaring of the documents with tokens, padded to 8
values        bit-packed per document (dense) or per document with tokens, in doc order (sparse);
              8 zero bytes of slack
```

The builder writes whichever of the two is smaller. A field with tokens in few documents and short
lengths costs a small bitmap and a few bits per document that has tokens, instead of a byte per document
of the segment (102 text fields x 3.78M documents would be 385 MB as one byte each). `total_length` sums
what the norms decode to, not the lengths written, so a merge, which has only the norms, computes it as a
build does; the average field length of a search comes from it, less the deleted copies.

### 6.4 Measured

The 1,000 random company documents of §2 (102 text fields with tokens): TQBF text sections
960,774 B mapped against 5,447,354 B of heap before (inverted index 4.42 MB, SymSpell 0.71 MB, prefix FST +
map 0.32 MB), 0.89 KB per document marginal; POSTINGS 395,820 B, TERMS 355,947, RAWTERMS 101,283, FREQS
71,604, NORMS 36,120, measured on the same sections at v3, before FREQS moved to groups with a width byte.
The v3 build of all 3,782,003 companies was 5.4 GB.

## 7. Other field types

### 7.1 Value index: VALUES / NUM_VALUES, VALUE_DOCS, DOC_ORDS

Filters, facets, sorting and group_by read one structure per field: a dictionary of the field's
distinct values, numbered in value order, the documents of each value, and for facet and sort
fields each document's value ordinals. Strings and numbers share the layout; only the dictionary
differs (VALUES for strings, NUM_VALUES for numbers, §7.2).

```
VALUES      FST value -> ordinal (byte order)
VALUE_DOCS  u64 n_values, u32 entry_width, u32 0
            entries[n_values] bit-packed at entry_width: doc << 1 | 1 for a value in one document,
            else (offset / 4) << 1 of its frozen roaring (relative to the bitmaps); padded to 8
            frozen roaring bitmaps, each on a 4-byte boundary
DOC_ORDS    u64 n_values, u32 doc_count, u8 width, u8 multi, u16 0       (ordinal n_values = none)
            u64 present_len, frozen roaring of the documents that have the field (an empty array
            counts), padded to 8
            single: one ordinal per document, bit-packed at width = bits(n_values)
            multi:  monotonic element offsets (doc_count + 1, §7.4), padded to 8; every document's
                    elements in document order, bit-packed at width (ordinal n_values = a null element)
```

- Which fields: VALUE_DOCS for every filterable field (string[] and numeric fields with index or
  facet, faceted strings) and every sort field; DOC_ORDS for facet and sort fields (strings and
  numbers: one ordinal per document; string[] and faceted numeric arrays: the element sequence).
- String filters compare tokens (`src/filter/text.rs`), so they are answered by the text index
  (§6) whether or not the field is faceted: the documents with every term of the value (the
  terms' postings intersected); for `:=` and a `"phrase"` those whose value, or one array element,
  holds the terms where they must be, read from the stored text (on a string field `:=` first keeps
  the documents of the value's length, which is all one word needs); a trailing `*` takes every term
  with the prefix as the last. A faceted field answers `:=` and phrases from its VALUES instead, each value tokenized as
  the field indexes it, when it has fewer values than there are documents to check. Both find the
  same documents: a document's text is analyzed in its language, as the text index holds it, and a
  word of the filter's value stands for its term in every language on a stemmed field (Swedish
  "toys", English "toy"); a value whose match depends on its documents' languages leaves the
  answer to the text index. A field without a text index answers from its VALUES, in the
  collection's language. A document without a language is in the collection's (`default_language`).
- `field:(a:=x && b:=y)` on arrays of objects (an element index where every condition holds) reads
  the fields' DOC_ORDS element sequences, each value decided once, not the candidates' records.
- A value in one document is stored as its doc id in the entry (Lucene's singleton doc id), so a
  unique value (an amount, a name, an id) costs one packed entry and no bitmap.
- Exact numeric filter: dictionary lookup, one posting, read in place. Range filter: the ordinals in range
  are consecutive (binary search over the dictionary), and their postings are OR-ed into a dense
  bitset of the segment, which becomes the result bitmap. IN lists are unions of exact lookups.
- Facet counts (`src/index/facet.rs`): the candidates' ordinals are counted from DOC_ORDS
  (O(candidates): a few nanoseconds a document, tens for an array's elements) when that costs less
  than intersecting each value's posting with the candidates (O(values): about a microsecond for
  every 65,536 documents of the segment). Strings, numbers and bools
  alike; a numeric field's stats come from the same counts, and its ranges (`price(cheap:[0, 100])`,
  from the lower bound up to the upper) are sums of consecutive ordinals' counts. The values are
  ordered by count (ties in value order), in value order (`sort_by:_alpha`) or by another field
  (`sort_by:points:desc`, the largest value of it among a value's documents), then cut to
  max_facet_values; only those are looked up by ordinal (an FST with outputs in key order is walked
  from ordinal to key, Lucene's `Util.getByOutput`). A grouped search counts each value's distinct
  groups (pairs of ordinal and group, sorted); `top_k:true` counts the search's first 250 hits (or
  up to the page's end); a facet_query counts only the documents holding its words, found in the
  field's TERMS (every term within the typos, their postings united and intersected), and keeps
  the values matching with the fewest typos any result matches. Over several segments each counts
  every value (a grouped search's groups listed) and the merge sums (unites) them; facets that need
  every segment's results at once (top_k, a facet_query) are counted after the merge.
- group_by hashes ordinals (equal ordinals are equal values within a segment) and reads the key
  values once per group. string[] keys keep the element order and null elements, as before. With
  `group_missing_values=false` a document without a value is a group of its own.
- Per-document numbers (facet stats, decay, JOIN sort) are ordinal -> dictionary, two O(1) reads.
- DOC_ORDS keeps the element order of arrays (with nulls), unlike Lucene's SortedSet (sorted unique
  ordinals): group keys of string[] fields depend on the sequence.
- Filter values are read as the field's type: on a string field `02101` stays the string "02101",
  on a float field `1000` is the float 1000.0, and a fractional bound on an integer field selects the
  integers on its side. A range matches only documents that have a value in it.

### 7.2 Numeric fields (NUM_VALUES)

`u8 float`, 7 zero bytes, then the field's distinct values in order as a monotonic sequence (§7.4).
Each value is its i64 key with the sign bit flipped; floats are mapped to an order-preserving i64
first (the bits, negative values flipped; -0.0 and 0.0 share the key of 0.0, as they compare equal).
int32[], int64[] and float[] fields index every element.

Rejected: a 1-D BKD tree (Lucene points) for ranges plus a separate per-document value column
(Lucene numeric doc values). The dictionary + VALUE_DOCS layout answers ranges by unioning the
postings of a contiguous ordinal range, which for unique values is the same sequential decode a BKD
leaf scan does, and for low-cardinality fields (counties, size classes, years) is a few run or
bitset containers per value instead of one packed doc id per document. The per-document column is
the DOC_ORDS ordinal, at most as wide as a delta-packed value (bits(n_values) <= bits(range / gcd)),
and the dictionary is shared with the filters.

### 7.3 Sorting

No sort-specific section. A field's DOC_ORDS ordinals are ranks in value order (ties share a rank,
a missing value ranks last), so sorting compares two packed integers whatever the value type; bool
fields rank false < true from BOOL. The sort key of a document is `rank << 32 | (u32::MAX - doc)`,
which orders ties by highest doc id first. A page over a dense candidate set walks the values in
rank order and intersects each value's posting with the candidates, stopping after the page, so no
rank -> doc permutation is stored (one would cost 2 x 22 bits per document per sort field at 3.78M
documents: 20 sort fields x 3.78M x 44 bits = 416 MB).

### 7.4 Packed integers

- Fixed width: values LSB first at any width up to 64, followed by 8 zero bytes, so a read is one
  unaligned 8-byte load, a shift and a mask (a ninth byte above 56 bits). Widths are exact, not
  rounded to Lucene's DirectWriter set: x86-64 and AArch64 load unaligned words at full speed.
- Monotonic (Lucene's DirectMonotonic): blocks of 1024 values, each stored as its residual above a
  line through the block: `u64 count, u64 n_blocks`, per block `u64 base, f64 slope, u64 bit_offset,
  u32 width, u32 0`, then the residuals. Used for array element offsets and numeric dictionaries.

### 7.5 BOOL

`u64 true_len`, the frozen roaring of the documents with true, padded to 8, then the frozen roaring
of the documents with false. A bool[] document is in both when it has both values (match-any).

### 7.5.1 Measured

The 1,000 random company documents of §2 (82 string, 67 numeric and 21 bool fields with
value sections):

| | before (heap) | TQBF sections (mapped) |
|---|---|---|
| string filters | 2,235,309 B | |
| numeric filters | 1,048,208 B | |
| bool filters | 18,000 B | |
| sort indexes | 152,582 B | |
| column store (typed values of every indexed field) | 2,657,695 B | |
| total | 6,111,794 B | 619,229 B (9.9x smaller) |
| marginal per document (100 -> 1,000 docs) | 5.90 KB | 0.58 KB |

By section: VALUES 315,441 B, VALUE_DOCS 171,190, DOC_ORDS 73,348, NUM_VALUES 43,362, BOOL 15,888.
The heap of the loaded collection is now 64,620 B, of which 64,000 B is the geo index (§7.6);
it was 6,203,382 B. Loading the segment takes under 1 ms. Documents are byte-identical and the 50
test searches (text, filters, facets, sorts, group_by, decay) identical before and after a restart.

Lookups over 3.78M synthetic documents shaped like the companies (`benches/values_bench.rs`): exact
filter 121 ns (a 21-value county field) and 135 ns (a string[] code); range over 10 years of a 15K-day
date (880K documents) 2.3 ms; range over a mostly unique amount (150K documents) 0.80 ms; facet
counts of an 800-value string[] field over all documents 79 µs, over 1% of them 0.75 ms; top 10 by
date descending over all documents 0.19 ms, over 10% of them 4.0 ms; one per-document value read
3.5 ns. Section bytes per document at that size: county 2.1, date 5.9, amount (in 1 of 5
documents) 3.5, string[] codes 4.3.

### 7.6 GEO

One section per geopoint or geopoint[] field: every point of the field in a static packed Hilbert
R-tree (the Flatbush layout), coordinates in 1e-6 degrees, plus each document's point positions.

```
u32 doc_count, u32 n_points, u32 n_nodes, u8 n_levels, u8 multi, u8 doc_width, u8 pos_width
u64 present_len, frozen roaring of the documents with a point, padded to 8
u32 level_end[n_levels], padded to 8      cumulative node counts per level, leaves first
i32 box[n_nodes][4]                       min_lat, min_lng, max_lat, max_lng
u32 leaf_offset[n_leaves + 1], padded 8   byte offset of each leaf's points
u8  leaf_width[n_leaves][2], padded 8     bits of lat - min_lat and of lng - min_lng
point_docs   each point's doc id at doc_width, in tree order; 8 zero bytes, padded to 8
points       per leaf its points relative to the leaf box's minimum, lat then lng at the leaf's
             widths, each leaf from a byte boundary; 8 zero bytes, padded to 8
single:      per document the tree position of its point at pos_width (n_points = none)
multi:       monotonic element offsets (doc_count + 1), padded to 8; each document's point
             positions at pos_width, in array order
```

- Points are ordered by the Hilbert value of their position on a 2^16 grid over the field's extent,
  16 to a leaf and 16 children to a node. Node `j` of level `l` covers nodes `16j..16j + 16` of
  level `l - 1` and tree positions `16^(l+1) j ..`, so no child pointers are stored. A leaf's box is
  small, so its points take few bits each (exact widths, §7.4).
- A document's point is one packed read away (position -> leaf box + packed offset), so distance
  sorting and `geo_distance_meters` need no separate per-document column.
- Radius filter: walk the tree; a node outside the cap's bounding box is skipped, a node whose four
  corners are inside the cap is taken whole (on a sphere the farthest point of a box less than 90
  degrees of longitude wide is a corner), the points of the other leaves are tested one by one.
- Polygon filter: the query builds a grid over the polygon's latitude/longitude bound, about 16
  cells per edge (1,024 to 16,384). Each cell has a reference point, whether it is inside, and the
  edges that pass near the cell. Those edges are found per column from the edge's latitude there,
  widened by the most a geodesic between two points of the cell bows toward the pole. Summed-area
  tables over the grid relate a node box in O(1): when none of its cells lists an edge it is wholly
  inside (taken whole) or wholly outside (skipped). A point in a cell without edges takes the
  cell's answer; any other counts the listed edges that the arc from the reference point crosses.
  A million points take 2.5 to 5.3 ms, about what a radius takes; the grid is built in 0.13 ms for
  100 vertices and 2.6 ms for 5,000.
- Nearest first: best-first search over node boxes keyed by the smallest angle from the center to
  the box (along the box's latitude span, or along its nearer meridian edge, where the distance is
  smallest at an end or at the edge's closest latitude). Points leave the heap nearest first; once a
  page is full, the points at the last page distance are still collected so ties are broken by doc
  id. Farthest first, and small candidate sets, rank every candidate.

Typesense's geo arithmetic, measured on a running server with single-document queries
(24 distances from 111 m to 16,679 km and 7 radius tests):

- Coordinates are truncated to 1e-6 degrees (`int32(lat * 1e6)`, as Typesense packs them), both the
  stored points and the sort center; the radius filter's center is used at full precision.
- Sort and `geo_distance_meters`: the haversine angle (`S2LatLng::GetDistance`) times 6,369,628.75 m
  (fitted: every one of the 24 distances is reproduced, the radius is known to +-0.04 m), truncated
  to whole meters. A document without a point is 2147483647 m away (last ascending, first
  descending). A geopoint[] document is as far as its closest point.
- Radius filter: the S2 cap of `radius / 6371010` radians (`S2Earth::RadiusMeters`) around the center,
  tested on the truncated point as `S2Cap::Contains` does (squared chord between unit vectors). The
  radius number is read as a 32-bit float (`stof`), then times 1000 (km) or 1609.34 (mi). Filtering is
  exact at every radius tested (5, 20 and 30 km), not approximate above 10 km.
- Polygon filter, as S2's `S2Loop` (Typesense's): the edges are geodesics between the vertices,
  which are used at full precision. The polygon is the smaller of the two regions the loop bounds,
  whichever way the vertices run, so one around a pole or across the antimeridian needs nothing
  more. It is tested on the truncated point. A point on the boundary is in exactly one of two
  polygons that share the edge or the vertex (S2's semi-open model, here by symbolic perturbation
  in a fixed direction with exactly antisymmetric orientation tests). Validation and messages are
  S2's: "Polygon is invalid: " and then "Non-empty, non-full loops must have at least 3 vertices",
  "Edge i is degenerate (duplicate vertex)", "Vertices i and j are antipodal", "Edge i has
  duplicate vertex with edge j" or "Edge i crosses edge j", numbered in the order given (Typesense
  numbers them after it reverses a clockwise loop). Torque goes further in three places. A last
  vertex repeating the first (a closed ring, as GeoJSON writes it) is dropped. A latitude outside
  [-90, 90] is refused, where S2 would fold it over the pole. An odd number of coordinates is a
  format error, where Typesense ignores the last one.
- Both of Typesense's filter forms: `(lat, lng, 5 km)` and `(lat1, lng1, lat2, lng2, lat3, lng3,
  ...)`, and `([lat, lng], radius: 5 km)` and `([lat1, lng1, ...])`, either with
  `exact_filter_radius`, and lists of them (a point in any). Beyond `exact_filter_radius` (default
  10 km) Typesense keeps every point in the S2 cells covering the region, a superset. Torque reads
  the parameter and tests every point exactly. A value of four numbers, `(lat1, lng1, lat2, lng2)`,
  is Torque's latitude/longitude box (Typesense refuses it as a polygon of two vertices).

Rejected: a 2-D BKD tree (Lucene LatLonPoint) plus a per-document doc values column. The BKD leaves
need a column beside them for sorting (8 bytes per document); here the tree is the column. A Z-order
(Morton) layout instead of Hilbert gives larger leaf boxes. An S2 cell-id index (what Typesense
uses) needs the covering cells of every query region and an exact pass anyway.

### 7.7 VECTORS

One section per float[] field with `num_dim`:

```
u32 doc_count, u16 num_dim, u8 metric, u8 effective_metric (0 cosine, 1 ip, 2 l2),
u32 entry_point (u32::MAX = none), u32 max_level, u32 n_layers, u8 quantized, 3 x 0; to 64
f32 vectors[doc_count x num_dim] (unit length for cosine), padded to 8
per layer: u32 n_members, u32 n_edges; upper layers: u32 members[n_members], padded to 8;
           u32 offsets[n_members + 1], padded to 8; u32 neighbors[n_edges], padded to 8
quantized (num_dim >= 64): f32 rotation[num_dim x num_dim], padded to 8;
           u8 codes[doc_count x ceil(num_dim / 8)], padded to 8; f32 norms[doc_count], padded to 8
```

Vectors stay f32 so distances match Typesense's float32 HNSW. Layer 0 lists every node; an upper
layer lists only its members (about 1 in 16 per level), found by binary search, where the graph
being built keeps a slot per node on every layer. Search runs on the section in place (the arrays are
read as `&[f32]` and `&[u32]`); RaBitQ codes are what a GPU scan uploads. A merge copies the vectors,
codes and norms, keeps the graph of its largest source without deletions (nodes renumbered, edges as
they are) and inserts the other vectors into it.

### 7.8 REFERENCE

A reference field (`reference: "collection.field"`) keeps its values in the value index (§7.1), with
ordinals for every document: forward JOINs read a document's target ids from DOC_ORDS, reverse JOINs a
target id's documents from VALUE_DOCS. Targets are resolved against the target collection's IDS at
query time. Target doc ordinals are not stored: the target collection is rebuilt and swapped on its
own schedule.

### 7.9 Measured

The 1,000 random company documents of §2 (3 geopoint fields and one geopoint[] field,
1,773 points): GEO sections 22,400 bytes (12.6 per point) in place of 64,000 heap bytes of
per-document coordinate arrays and an R-tree that was not counted. The loaded collection now holds
620 heap bytes of index data (the all-docs bitmap and the field maps); everything else is the mapped
segment file, which loads in under 1 ms. Documents, the 50 test searches and 21 geo checks against
a running Typesense (9 radius filters: found and ids; 12 geo sorts, ascending, descending, past the
documents without a point, mixed with other sort fields: ids and `geo_distance_meters`, 230 finite
distances) are identical before and after a restart. The only differences are the order of documents at the same distance,
which follows each engine's doc order.

Over 3.78M synthetic documents with a point each in 95% of them, placed like company addresses
(`benches/geo_bench.rs`): 10.7 bytes per point (38 MB); radius 1 km around Stockholm 54 µs, 10 km
(159K documents) 2.2 ms, 50 km (1.2M documents) 4.9 ms; nearest 10 from Stockholm 10 µs (23 µs when a
filter keeps 10% of the documents); one document's distance 0.12 µs; build 0.35 s.

## 8. Rendering (the only JSON the server writes)

Hits are rendered straight from records into the response buffer, without an intermediate value tree:

- keys in `render_order` (byte order, as Typesense); `id` included as Typesense does.
- `include_fields` / `exclude_fields` are applied while walking the record.
- numbers formatted exactly like nlohmann::json `dump()`: integers as decimal; doubles with nlohmann's
  Grisu2 `to_chars` and its format rules (decimal notation for exponents -4 < n <= 15, otherwise
  `d.ddde+XX`; integral doubles keep `.0`; Grisu2 is not always shortest, e.g. `11.952819999999999`).
  The algorithm is ported, not approximated.
- strings as raw UTF-8; `"` `\` and control characters escaped as nlohmann does (`\b \f \n \r \t`,
  others as `\u00xx`); `/` is not escaped.
- highlights are computed from the same record values.

Pass condition: for a random sample of documents, `GET /collections/{c}/documents/{id}` and every
`hits[].document` are byte-identical to Typesense.

## 9. Collections that change

- Segments: the same immutable segments. A realtime write group is indexed as one small segment in
  memory with the same builder as the nightly path; segments merge by tier (10 of one size into one),
  in memory and once flushed, and a flushed segment with 20% deleted copies merges alone. A merge
  makes its segment from the sources' sections (`index::merge`): the segment a build of the same
  documents makes, without parsing or indexing them again. `docs/realtime-write-path.md` has the
  details.
- Write-ahead log per collection (`wal/<collection>/<first lsn>.log`): records
  `[u32 len][u32 crc32][u64 lsn][u64 schema_version][u8 kind][writes]`, a record holding a write
  group, each write's upserts (binary protocol records) and deleted ids in order. A write is
  acknowledged only after its record is on disk; concurrent writes share one sync (group commit).
- Deletes and replaced documents: per-segment deletion bitmaps (frozen roaring files).
- `_seq_id` (Typesense's sequence id: a document's place in the order documents were first written,
  kept when it is written again): every segment stores its documents' ids as runs (SEQ); merges
  keep them; the manifest holds `next_seq`.
- The set on disk: `segments/<id>.tqbf` per segment (written once), `segments/<id>-<n>.del` per
  changed deletion mask, `segments/manifest.bin` (the commit point, with `wal_lsn`, the log's
  checkpoint, and the collection's field registry: the log's records after the checkpoint read
  against it). A save writes what changed, then the manifest (temp, fsync, rename, fsync
  directory), then removes the files the previous manifest held and this one does not; log files
  wholly before the checkpoint are removed (docs/realtime-write-path.md).
- Schema changes: the manifest holds the schema its segments were saved under (`schema`), with the
  collection's `created_at` and the schema's `schema_version`. A change of the fields commits with its
  rebuilt segments in that one file replace. On load, a manifest newer than collections.json is the
  schema; one of another `created_at` (an earlier collection of the name) is not loaded.

## 10. Build and upload

1. The nightly job builds the complete segment file itself, in its own process, with the native
   builder (§10.5): the .NET `Torque.Http.Tqbf.TqbfSegmentBuilder` calls `torque_build`
   (ffi/torque-build), the same Rust code the server indexes with. Documents go in as binary
   records: `Torque.Http.Newtonsoft`'s `TqbfNewtonsoftEncoder` writes them straight from the
   document objects, exactly as Newtonsoft would serialize them and the JSON import would read that
   JSON, without writing JSON (`TqbfRun.AddJson` takes JSON lines too). Doc ids follow the run keys
   (§10.5), which decide tie order in results.
2. Upload (§10.3): .NET `CollectionClient.UploadSegmentsAsync`, or `torque-admin upload
   <collection> -f out/ [--alias A --drop-old]`.
3. `PUT /aliases/{alias}` switches readers atomically (ArcSwap). Deleting the old collection first
   releases its mapped files and then removes them (Windows refuses to delete a mapped file; searches
   still holding the old snapshot finish within moments).

A docs-only file (SCHEMA, FIELDS, RECORDS: §10.1), as `TqbfDocsWriter` writes it, is built by the same
builder: `torque-admin build docs.tqbf -o out/ [--level 19] [--transport]` offline, or the server when
one is uploaded (zstd level 3). The builder takes over the file's FIELDS (adding the schema's paths), so
its records are indexed and stored as they are.

### 10.1 RECORDS

```
chunks       u32 n_docs, u32 raw_len, zlib stream of n_docs x (varint(len) record)
trailer      u64 chunk_offset[n_chunks + 1] (relative to the section), u32 doc_count, u32 n_chunks
```

Chunks of about 1 MB of records, so a writer streams them and the builder decodes them in parallel.
zlib because every client platform has it built in (.NET `ZLibStream`); RECORDS only lives on the
build machine or in transit, never in a served segment. The trailer lets a writer stream without
knowing the chunk count.

### 10.2 Transport codecs

`--transport` stores every section that zstd (level 19) shrinks by at least 10% as a zstd frame
(codec 1, `raw_length` and `xxh3` of the decoded bytes in the table). On the 1,000
companies: 2.49 MB -> 1.97 MB (POSTINGS 2.7x, the term and value FSTs 1.4-1.8x, SCHEMA 17x; DOCS,
the bit-packed sections and most small sections stay as they are). The docs-only file of the same
documents is 1.16 MB, the smallest transfer, at the price of a build on the server.

### 10.3 Upload

`PUT /collections/{name}/segments?part=i&parts=n`, one file per request (a segment file, or a
single docs-only file), mounted outside the request timeout. The server streams the body to
`data_dir/uploads/{name}/`, removes transport codecs, verifies every section checksum, and answers
202 until all parts are there. The request that completes the set creates the collection from the
SCHEMA of its parts (identical in every part), builds a docs-only part, moves the files into the
segment set under new segment ids, publishes the manifest and maps the set: 200 with the collection. Nothing is
decoded or indexed for a built segment. The name must be free (409 otherwise); a damaged file is
refused (400). The parts of one upload come from one build: they share the FIELDS ids.

### 10.4 Measured

The 1,000 random company documents through the whole path (.NET writer, `torque-admin build`, upload,
and the server-side build of the docs-only file): documents byte-identical to a running Typesense's,
the 50 test searches identical to the JSONL import, the 21 geo checks identical to it, also after a
restart and with transport codecs. An edge-case
set (-0, u64 and below-i64 integers, mixed, nested and empty arrays, empty objects, Unicode key
order, repeated keys, escapes, 4-byte UTF-8) renders byte-identically through both paths.

- .NET writer: 1,000 documents in 133 ms (cold); 1.16 KB per document in the docs-only file.
- Build: 20,000 documents (the sample repeated under new ids) in 1.9 s. Upload of a 2.4 MB segment:
  52 ms from the first byte to the installed collection (local); the segment maps in under 1 ms.
- Build memory (that first builder, replaced by §10.5) grew by about 32 KB per document: it held
  every parsed document of a segment.
- Search across several segments returns the same hits and counts as one segment, but not the same
  order (ties are broken per segment, the default `q=*` order and text_match differ): 4 of the 50
  test searches identical with the sample in four segments. A collection that must match Typesense
  is built as one segment, which the streaming builder does in bounded memory.

### 10.5 The builder (`src/build`)

Records come in runs. A run is a stretch of consecutive doc ids added on one thread, which indexes each
record as it arrives; runs are laid out in key order (the caller numbers its runs by the data they
cover, e.g. the CompanyId range), so the doc id order and the file never depend on thread timing.

- Per record: the typed values are read in place from the record bytes, with the JSONL import's exact
  flattening, pivoting of object arrays, type checks and error messages (`build/doc.rs`); nothing is
  decoded into a JSON value. A document the import would refuse is reported and takes no doc id.
- Accumulators per field: terms and values interned in one byte arena each, (term, doc, tf) and
  (value, doc) triples, ordinals; every distinct token is analyzed once per piece. Records are grouped
  into 16 KB DOCS blocks and zstd-compressed while they arrive.
- When its accumulators reach 64 MB, and at its end, a run freezes a piece: terms and values sorted,
  postings delta-coded, ordinals bit-packed, lower-cased tokens equal to their term as one bit. Frozen
  pieces go to a spill file (mapped for the merge), so the builder's memory does not grow with the
  collection: per adding thread one piece of accumulators, plus the merge.
- `finish` maps the spill file and merges the pieces field by field, in parallel windows written in
  field order: k-way merges of terms and values, postings concatenated in doc order. The sections come
  from the import's encoders (`encode_text`, `ValueDocs::write_with`, `DocOrds::write_*`,
  `BoolIndex`, `GeoIndex`), so they equal a JSONL import's byte for byte. The file is streamed out
  section by section, the table last.
- The DOCS dictionary is trained on the first 8 MB of records in key order. With run keys 0, 1, 2, ...
  that happens as soon as those runs have them, and every later block is compressed while it arrives;
  with other keys it is trained at `finish` on the first records in doc id order. The same records
  either way, so the same file.
- Document ids must be unique within a build (a repeated id fails it).

### 10.6 Measured (the builder)

20,000 company documents on the development machine:

- Every index section byte-identical and every stored document equal across: the old path (Newtonsoft
  JSON -> `TqbfDocsWriter` -> the batch build), JSON lines into the native builder, the encoder on one
  thread, and the encoder on 8 threads. File 36.36 MB vs 36.25 MB (+0.3%: DOCS blocks restart at run
  boundaries).
- Per document on one thread: the encoder 16-20 us (the old path: Newtonsoft JSON 57-80 us plus the
  docs-only writer 66-71 us); indexing about 45 us (text 21, record reading 5, values 4, piece freeze 8,
  storing with zstd level 1: 17); zstd level 19 on the stored records 320-340 us (8.8 MB/s). On 8
  threads 26,000-29,000 documents/s, zstd 19 included.
- zstd on the records (2.84 KB raw per document): level 19 0.657 KB, 15 0.660 KB at twice the speed,
  9 0.686 KB at 12x, 3 0.739 KB at 44x.
- Memory: peak working set 143 MB for the 20,000 documents (the batch build: 913 MB); frozen pieces
  2.7 KB per document in the spill file (about 10 GB for 3.78M companies, page cache and temp disk,
  not heap).
- The encoder allocates about 70 bytes per document of its own.

## 11. Memory model

- The process heap holds the field registry, the block cache and per-query scratch. Everything else is
  mapped file pages (§7.9).
- Hot sections (TERMS, POSTINGS, VALUE_DOCS, DOC_ORDS of fields used in filters, facets and sorts)
  can be prefetched at load (`madvise(WILLNEED)` / `PrefetchVirtualMemory`); DOCS is advised random.
- Per collection stats report mapped bytes per section and resident bytes per section (`mincore` /
  `QueryWorkingSetEx`), so memory per collection is measured, not estimated.
