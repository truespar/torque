# Realtime Write Path

How a realtime collection takes writes: small immutable segments in memory, merged by tier, made
durable by a write-ahead log, saved by writing only what changed.

## Overview

A write is indexed on its own as a segment appended to the collection's segments, and the copies it
replaces are hidden by deletion masks. Searches run over the segments as over any multi-segment
collection, so a document is searchable the moment its write returns. The answer is exactly the one
a single segment holding the same live documents gives. A record of the write-ahead log is on disk
before the write is acknowledged.

```
Client write (upsert, update, emplace, delete, import, binary protocol, MCP)
  → queued; the writer that takes the collection applies every write queued by then together
    (LevelDB's write group): one segment, one log record holding each write in order
  → write::apply: the group's documents indexed as one segment, every older copy of an id
    written or deleted masked, the bundle published (ArcSwap)
  → the record appended to the log; its writer thread syncs it (group commit)
  → acknowledged once the record is on disk
```

- **Visible at once, durable when acknowledged.** The bundle is published before the sync and the
  answer waits for it. A crash loses no acknowledged write.
- **One search path.** Filters, facets, sorts, geo, group_by, vectors, joins and highlights see
  written documents through the same segment code as batch collections. No overlay merge logic.
- **Lock-free reads.** A search loads the bundle once and never waits for a write or a merge.
- **Concurrent writes share the work.** A write group pays one build and one sync.
- **Bounded memory.** Writes get 429 while the segments in memory hold `MAX_MEMORY_DOCS` (10,000)
  documents; the flush that starts makes room.

## The bundle

```
IndexBundle (immutable, replaced whole)
├── segments: Vec<Arc<Segment>>            the flushed ones first, then `memory` built by writes
├── deleted_per_segment: Vec<Arc<Bitmap>>  hidden copies; a write clones only the masks it changes
├── Segment::seq (SeqIds)                  each document's `_seq_id`, as runs of consecutive ids
├── next_seq                               the `_seq_id` of the next new document
├── memory                                 how many trailing segments are not flushed yet
└── lsn                                    the last log record the bundle holds
```

A document has one unmasked copy. Writing an id again hides the old copy and keeps its `_seq_id`;
deleting it and writing it again gives a new one (`next_seq`). Lookups by id (GET, update, emplace)
take the newest unmasked copy.

`write::apply` builds the segment single-threaded and without purging the allocator below
`SMALL_BUILD_DOCS` (4,096 documents). A pool of threads per build, allocator purges and zstd
contexts made per block cost more than a small build itself. Blocks without a dictionary use
thread-local zstd contexts.

## Merges and flushes

Segments merge in tiers (`write::merge_run`). A segment's tier is the number of digits of its live
document count. Once the newest run of `MERGE_FACTOR` (10) segments of one tier exists, it merges
into one. So each document is merged about log10(n) times over its life, not once per write, and
the number of segments stays logarithmic in the collection's size.

A merge takes the live documents of the run (masks applied) in `_seq_id` order, keeping each
document's `_seq_id`, and makes the merged segment from the run's sections (Merging from sections):
no document is parsed, typed or tokenized again. Writes go on meanwhile. Publishing
(`write::publish`) reconciles: a document of the run deleted or written again since the merge's
snapshot is masked in the merged segment. A merge whose segments a truncate or a schema change
replaced is dropped. A merge of no live document leaves no segment (a bundle keeps one).

A flushed segment whose deleted copies (documents deleted or written again) are `RECLAIM_SHARE`
(20%) of it merges on its own (`write::reclaim`), which drops them. Big segments rarely form a run
of their tier, so a segment whose documents are updated would otherwise keep its deleted copies
for good. Each deletion costs at most 5 documents merged again, and a segment holds at most 1.25
times its live documents.

| Step | Of | When |
|---|---|---|
| Merge in memory (`Collection::merge_memory`) | segments not flushed | after a write leaves a run due |
| Flush (`Collection::compact`, `compaction::flush`) | the segments in memory into one; then merge flushed, save, checkpoint the log | `FLUSH_DOCS` (1,000) documents in memory; `POST /collections/{name}/documents/compact`, `POST /operations/db/compact`; the schedule (`compaction_interval_secs`); memory pressure (`compaction_memory_threshold_percent`); shutdown |
| Merge flushed (`Collection::merge_flushed`) | flushed segments: runs of a tier, then each with 20% deleted copies alone | after a flush; after a write leaves a segment with 20% deleted copies (`Collection::wants_reclaim`), then save |

Merges and flushes after a write run in one background task per collection
(`compaction::maintain`, `Collection::maintaining`). The task goes on while a merge, a flush or a
merge of a segment with many deleted copies is due, so writes never queue flushes behind each other: each would save the few documents written
meanwhile as a segment of its own (measured: 299 flushes and 330 segments in 8 s of 64 writers,
against 50 and 20 with one task). A flush waits for a merge under way, and a merge in memory stops
for a waiting flush (`Collection::begin_publish`, `flush_waiting`); a flush used to flush nothing
when a merge held the collection.

There is no flush on a timer: the log already makes the writes durable. Flushes bound the log's
replay at restart and the memory the segments in memory take. The batch segment merge
(`compact_collection_segments`) skips realtime collections, whose segments merge by tier; it
merges a batch collection's segments from their sections too.

## Merging from sections

`index::merge`, Lucene's segment merge. The merged segment is the one a build of the same
documents in the same order makes, section for section; only the HNSW graphs differ. The tests
compare every section byte for byte, and the stored documents record for record. Each document
gets its number in the merged order (`DocMap`), and deleted ones are left out.

| Part | Merged by |
|---|---|
| Text (TERMS, RAWTERMS, POSTINGS, FREQS, NORMS) | the union of the sources' term FSTs; each term's documents and frequencies read in place, renumbered and put together (sorted only where sources interleave), each document's norm moved over; token forms are the sources', so a form only deleted copies held stays (`InvertedIndex::merge`). KEYS from the merged TERMS |
| Values (VALUES or NUM_VALUES, VALUE_DOCS, DOC_ORDS) | the union of the dictionaries (FSTs, or the sorted keys merged), dropping a value without a document left; postings renumbered; each document's ordinals mapped. |
| BOOL | the true and false documents renumbered |
| GEO | every point of the documents carried over, the R-tree packed again |
| VECTORS | vectors, RaBitQ codes and norms copied. The HNSW graph of the largest source without deletions is kept, its nodes renumbered and their edges as they are, and every other vector is inserted into it (Lucene's merge from the largest segment's graph). The graph is built afresh when every source has deletions, since a graph without some of its nodes loses the paths through them. |
| Stored documents (FIELDS, DOCS, IDS) | records copied. A full block (cut at the 16 KB block size) is copied as it is (Lucene's bulk copy of clean chunks) when all its documents come over side by side and it is compressed with the merged segment's dictionary. The other records are compressed again in full blocks, so small segments' blocks do not pile up. IDS is the union of the sources' IDS. |
| References, all documents | from the merged values, and the count |

The segments of a collection share one zstd dictionary, so a block that reaches a flushed segment
carries from merge to merge until one of its documents goes:
- a merge compresses with the dictionary of the collection's largest segment (`write::dictionary`);
- the first merge with enough records and no dictionary to use (512 KB, the first flush) trains one;
- a write's own segment trains none (`docstore::untrained`): it is merged soon, and training costs
  more than the write.

Compression contexts are kept per thread and dictionary. Decompression dictionaries are shared by
the segments that use them. Pools start only for work of 256 KB of records or more
(`PARALLEL_BYTES`).

## `_seq_id`

A document's place in the order documents were first written, as Typesense's sequence id. Writing
it again (upsert, update, emplace) keeps it; deleting it and writing it again gives a new one. It
orders ties of every sort (the later first), the default order of a wildcard query, and
`sort_by=_seq_id`.

Every segment stores its documents' exact ids (`Segment::seq`, saved in the segment's file as the
SEQ section), and merges keep them. `next_seq` (saved in the manifest) survives a truncate, so a
document written after it sorts after every earlier one.

## Saves

`persistence::index`. A collection's segment set is a directory of immutable files:

```
data_dir/indexes/<collection>/segments/
  manifest.bin          the commit point: segments in order (id, base, documents, deletion file,
                        deleted count), the schema and its stamp, next_seq, wal_lsn, the field
                        registry (TRQX JSON)
  <id:016x>.tqbf        a segment (Segment::id), written once
  <id:016x>-<n>.del     its deletion mask as of a save (portable roaring, TRQX), written once
```

A segment's id comes from a process-wide counter that starts from the clock and moves above every id
loaded, so ids never repeat within a data directory. A save (`save_bundle`):
1. writes the segments not on disk yet (`Segment::is_saved`), and for each segment whose mask
   changed since the last save the next deletion file. A write copies a mask it changes, so the same
   mask (`Arc::ptr_eq`) is an unchanged one. Files are written side by side, each synced.
2. syncs the directory, then replaces the manifest (temporary name, sync, rename, directory sync).
3. removes the files the previous manifest held and this one does not. A removal Windows refuses
   (a file still mapped by a search) is tried again at the next save.
4. maps the bundle's segments built in memory from their files. `Collection::save` puts them in
   place of the heap ones (`adopt_saved`, compare-and-swap of the bundle), so a saved segment serves
   from its mapped file and its heap memory goes. A merge under way still publishes: merges know
   segments by id.

So a save costs what changed (the flushed segment, the changed masks, the manifest), not the
collection. A crash before the manifest leaves the previous one and every file it holds. Files no
manifest holds (a cut-short save, an earlier layout) are removed when the collection loads, at
startup or resume (`remove_strays`), never while something may be writing them. A manifest of
another format version is refused ("re-import"), and nothing is removed then.

The streaming batch build writes each segment's file as it finishes (`write_and_open`) and maps it;
its save writes the manifest alone. An upload moves its files into the set under new ids.

Every change to the data directory goes through `persistence::disk`: files created, synced, cut,
renamed and removed, directories made, removed and synced. A name (a file created or renamed into
place, a removal) is made durable on Unix by syncing its directory, and on Windows, where a directory
cannot be synced, by syncing the file: NTFS journals names in the order they are made, and a file's
sync flushes the journal through its own name. The manifest and collections.json are synced so
after their rename. A removal of a collection's files is made durable the same way.

## Collections on disk

collections.json lists every collection: those loaded, those suspended (`"suspended": true`), and
those whose files could not be read at the start, as those of another format version. Such a one
is set aside with its files (an error is logged): the next start tries again,
`POST /collections/{name}/resume` loads it, and `DELETE /collections/{name}` removes it and its files
without reading them. It holds its name until then (a create is a 409) and searches answer 404. A
collection's files are its segment set (`indexes/<name>`) and its log (`wal/<name>`).

| Change | Its commit | Then |
|---|---|---|
| Create | collections.json saved | (before: the files an earlier collection of the name left removed, its log made) |
| Delete | collections.json saved without it | its files removed |
| Suspend | saved, its log closed; collections.json saved with it suspended | |
| Resume | loaded; collections.json saved | |
| Upload | segments installed, their manifest; collections.json saved | the staging directory removed |
| Schema change | the manifest of the rebuilt segments | collections.json saved (a restart finishes it) |

At the start the files of names collections.json does not list are removed: a deletion a crash cut
short. A collections.json that cannot be read stops the server from starting: started without its
collections, its next save would drop them from the list.

A save of collections.json that fails may have replaced the file or not (its rename done, the sync
after failing): the change is neither done nor undone. The registry takes the side that
acknowledges nothing more (a collection created or installed is not there, one deleted stays
deleted, one suspended stays suspended) and no file is removed on that ground. Whichever file a
restart finds, nothing acknowledged is lost: a collection it lists has its files, and the files of
one it does not list are removed then. The answer is a 500.

A manifest holds its collection's `created_at`; one of another (an earlier collection of the name)
is not loaded.

## Write-ahead log

`persistence/wal.rs`, one log per collection, for realtime collections with persistence.

```
<data_dir>/wal/<collection>/<first lsn, 20 digits>.log

record  [u32 length][u32 crc32 of body][body]
body    [u64 lsn][u64 schema version][u8 kind]
        kind 1, writes:  [u32 n][n × path] [u32 count] then per write
                         [u32 n][n × (u8 how, u32 length, document)] [u32 n][n × (u32 length, id)]
        kind 2, clear:   nothing more (a truncate)
path    [u16 id][u16 parent][u16 flags][u8 type][u16 length][name]
```

A document is logged as written: its record (`how` 1, `tqbf::record`: every path, indexed or
not, as the doc store keeps it), or typed fields for one that came without JSON (`how` 2, typed
TCP ingest, `ingest::protocol::serialize_document`). A record's paths are ids of the collection's
field registry, so a log record also carries the paths the registry gained since the record before
(`Collection::logged_paths`); a restart takes them, in order, onto the registry of the saved
segment set, whose manifest holds the registry as the save found it. Records are numbered (LSN) in
the order the collection applies them, and one record holds a write group with each write in
order.

**Group commit.** A writer thread per log takes every record queued, writes them with one
`write_all` and one `sync_data`, then answers each waiter. Concurrent writers share the sync. On
the development machine a sync costs about 2.1 ms, which is the drive's cache flush (preallocating
the file gained nothing).

**A failed sync poisons the log.** Every waiting and later write fails, and the collection takes no
writes until a restart recovers it. A write that may not be on disk is never acknowledged (as
PostgreSQL after its fsync failure).

**Checkpoints.** A save records the bundle's LSN in the manifest (`wal_lsn`). The writer then starts
a new file and removes every file holding only records at or before it. Files also rotate at 64 MiB.

**Recovery** (`collection::recover`). The saved segments load, then the records after `wal_lsn`
apply as one batch with the semantics of the writes (`write::apply`). The collection is as it was
at its last acknowledged write.
- A torn tail (the last record of the last file cut short or failing its checksum) was never
  acknowledged and is cut.
- Damage before the end of the last file is an error. The collection loads what it has and takes no
  writes until the log is repaired, rather than silently dropping acknowledged writes after the
  damage.
- A record written under another schema version is skipped (see Schema changes).

| Event | Log |
|---|---|
| Create (realtime, persistence on) | a new, empty log (one left by an earlier collection of the name is replaced) |
| Write | one record per write group, acknowledged after its sync |
| Truncate | one `clear` record; the empty segment set keeps `next_seq` |
| Flush, save | checkpoint at the saved bundle's LSN (the manifest's `wal_lsn`) |
| Schema change | carries over to the new collection, which logs on under the new version |
| Suspend | saved, then closed with every write in the saved segments |
| Delete | closed and removed |
| Shutdown | collections flush and save, then each log closes |

## Schema changes

A `PATCH /collections/{name}` with field changes (`CollectionRegistry::change_schema`) makes the
collection again from every live document: each segment's (masks applied, the segments in memory
included), in `_seq_id` order. Each document is typed from its stored record against the new schema
and validated as a write is. A document the new schema refuses (a required field it lacks, a value
of another type) fails the change with Typesense's message, and nothing changes. A change of
metadata, synonym or curation sets alone keeps the segments as they are.

The change takes the collection as a flush does (a flush or merge in progress finishes first). It
holds the writes of the old collection and of the new one until it is persisted: first the new
segment set, its manifest checkpointing the log at the last write the old collection applied, then
the schema. Writes wait meanwhile, as Typesense queues them behind an alter. One that still reaches
the old collection afterwards goes to the new one (`Collection::successor`), typed against its
schema, and the old collection saves and flushes nothing more.

The segment set's manifest commits the change. Every manifest holds the schema its segments were
saved under, with the collection's creation time and the schema's version
(`Collection::schema_version`, which every change of the fields advances). Every log record holds
the version its documents are typed under.
- A restart after the manifest and before collections.json finishes the change
  (`persistence::load_collections`). The manifest's schema is newer than collections.json's, so the
  collection loads under the manifest's schema and collections.json is saved again. A record of the
  old version after the checkpoint is skipped, since the new segments hold it.
- A restart before the manifest finds the collection as it was before the change.

collections.json and the other JSON files are replaced whole: a new file is written, synced and
renamed over the old one.

## Collection info and metrics

`GET /collections/{name}` of a realtime collection includes:

```json
"realtime": {
  "memory_documents": 412,
  "deleted_documents": 1873,
  "segments": 14,
  "last_flush_at": 1792481234,
  "wal_bytes": 1048576
}
```

`memory_documents`: documents in segments not flushed. `deleted_documents`: copies deleted or written
again, still in their segments until a merge. `last_flush_at`: Unix seconds (0: none). Prometheus
also exports `torque_memory_documents` and `torque_deleted_documents`.

`POST /collections/{name}/documents/compact` flushes and answers `docs_compacted` (the documents flushed),
`total_docs` and `compact_ms`.

## Verification

Tests:
- `tests/segments_test.rs` `realtime_writes_match_one_segment` compares whole search responses (hits,
  order, scores, facets, highlights, groups) of a collection under writes with one segment built
  from the same documents. It checks the collection with a segment per write, after merging in memory,
  and after a flush.
- `collection::write` covers write order, replay as one batch, merge reconciliation, dropped merges,
  tier runs and `_seq_id` runs.
- `collection` covers restart replay, truncation, a damaged log and write groups.
- `persistence::wal` covers numbering, torn tails, damage in the middle, checkpoint removal, concurrent
  group commit and multi-write records.
- `tests/writes_test.rs` checks that acknowledged writes are on disk before the answer, and flushing
  once the segments in memory hold enough documents.
- `persistence::index` covers a save writing only what changed and removing what went, `_seq_id`s
  saved with their segment, files no manifest holds ignored then removed, a save cut short, a
  manifest of another version, and uploads.
- `collection` covers saved segments serving from their files (the same answers, a merge begun
  before still publishing, a restart), segments a schema change saved mapped at the next save, a
  flush waiting for a merge, and a segment with many deleted copies merged alone (saved, and the same
  after a restart).
- `index::merge` checks that a merge is the build of its live documents. This holds for every field
  type (string, string[], numbers and their arrays, float, bool, bool[], geopoint, geopoint[],
  references, vectors with and without RaBitQ, a default_sorting_field), after writes that rewrite
  and delete, for merges of merges, and on many threads. It also covers HNSW recall after keeping
  the largest graph (cosine, l2, ip; sequential and parallel insertion, at least a build's), a whole
  segment merged alone keeping its graph byte for byte, a source without documents, full blocks
  copied, and small segments' blocks compressed again together.
- `collection::write` covers the choice of a segment with many deleted copies, and a merge of no
  live document leaving no segment.

**Crashes at every change** (`collection::crash_tests`, with `persistence::disk::fault`). In the
lib's tests a disk can watch a directory: each change `persistence::disk` makes under it is counted
and recorded, with what it does to the names there and the data each sync makes durable (syncs are
not made for real). A scenario runs once to count its changes, then once for each, the disk
stopping there: that change and every later one fail. Each state the crash could leave is put on
disk and recovered:
- the process stopped: everything written is there;
- the power went, under POSIX's rules (a name kept once its directory is synced) and on Windows also
  NTFS's (names in the order made, through the last one of a file a sync since flushed), with each
  further part of the journal kept as a state of its own;
- the power went with the data written after a sync torn: cut short, zeros, garbage.

The recovered collection must load, hold every acknowledged step and the one under way whole or not
at all, answer searches (text, typos, filters, sorts, facets) as one segment of its documents does,
keep no file its manifest does not hold and no temporary file, and take writes that survive a
restart; a deleted collection must leave no file. Each recovery is crashed again at each change it
makes, and those states recovered too. The scenarios: writes, a merge in memory, flushes, saves, a
segment merged alone, a restart and a truncation; merges of flushed segments by tier; a schema
change; a delete and a create under the name; suspend and resume; an upload deleted and installed
again (the nightly rebuild). In all 571 crash points, 2,630 crash states and 7,730 recoveries
checked, about 55 s in a debug build (`TORQUE_CRASH_STATS=1` prints each scenario's counts).

**Failures at every change.** The same scenarios run once for each change failing (an I/O error,
the disk going on), every later step made. After a restart each value (a document, the
collection's existence, its fields, suspended or not) is as the last step acknowledged to set it
left it, or as a failed step after that set it, and the collection is whole as above.

They found:
- a crash during a collections.json save left its temporary file for good;
- on Windows the rename of collections.json and of the manifest was not flushed: a power loss after
  an acknowledged create could lose the collection;
- removals were not durable on Unix: a deleted collection's files could come back after a crash,
  and a collection created again under the name, within the same second, loaded them;
- a collections.json save that failed after its rename: the deletion it would commit was undone in
  the registry, the collection took writes, and the next start removed its files with them;
- a create whose collections.json save failed was acknowledged all the same (only logged), and a
  resume too.

Reading the code beside them: a collection whose files could not be read started empty, and its
next save removed them; a collections.json that could not be read started the server with no
collections; a deletion cut short left files that looked like a suspended collection's for good;
two collections.json saves at once could leave the older list on disk.

**Random operations against a reference** (`server::model_tests`). A seed makes a sequence of
operations on one realtime collection, through the HTTP routes called in process: document writes
of every action (create, upsert, update, emplace), PATCH, DELETE and GET of one document, imports
of several lines and actions, deletes and updates by filter, truncation, flushes, merges in memory,
segments merged alone, saves, a field added, restarts, and crashes (a disk stopping at a change of
the data directory, `persistence::disk::fault`, and one of the states it leaves put back). The
documents carry fields outside the schema, and some lack a required field. The reference is the
collection's documents in `_seq_id` order under Typesense's rules for each action. Every answer
(status and body, each line of an import) must be the reference's, the documents exported after
each operation its documents, and whole search responses (text, typos, prefixes, filters, sorts,
facets, groups) those of one segment built from them. A crash leaves the operation under way done
or not: the documents recovered are the one or the other. A failing seed is cut to the fewest
operations that still fail. The suite runs 48 seeds of 150 operations, and 12 with the background
merges and flushes on (as the server runs); `many_random_operations_answer_as_the_reference`
(ignored) runs more: 6,050 seeds of 200 operations passed in October 2026 (5,500 with the merges made by the operations, 550 with the background ones on), about 0.18 s a seed in a release build.

`a_view_holds_each_write_whole`: writers rewriting and deleting their documents together, merges
in memory, flushes and merges of flushed segments going on, readers checking that every view holds
each write whole.

They found:
- the log held a document's typed fields only: a restart before a flush lost every field outside
  the schema and every unindexed one of the writes it replayed. The log holds each document's
  record now, and the paths the record needs.
- a collection never written to answered `facet_by` with no `facet_counts`; one whose documents
  were all deleted answered with each field's, empty (Typesense's). An empty segment no longer
  answers apart.
- an HTTP import took about 200 ms whatever its size on Windows, and so did a binary import: the
  memory admission check listed every process, disk and network of the machine
  (`sysinfo::System::new_all`) to read the free memory. It reads the memory alone now (under 1 ms);
  a seed's 200 operations take about 150 ms instead of 6 s.

Measured on the development machine (Windows 11, NVMe, release build), over HTTP,
one document per write.

**Benchmarks in the repository.** Every number below can be measured again:
- `tests/realtime_load_test.rs` (ignored) starts the server binary on a data directory and runs the
  kill test, the write and search latencies, and a load with its flushes and a restart:
  `cargo test --release --test realtime_load_test -- --ignored --nocapture --test-threads 1`.
- `benches/realtime_bench.rs` (criterion): the segment a write builds, merges of ten segments, a
  flush and its save, the log's group commit, a restart replaying the log:
  `cargo bench --bench realtime_bench` (`-- --save-baseline before`, then `-- --baseline before`,
  compares with an earlier run).
- `benches/search_bench.rs`: searches of 100,000 documents on one segment and on a realtime
  collection's segments as writes leave them.

Both take their data directories from `TORQUE_LOAD_DIR` / `TORQUE_BENCH_DIR` (default: the
temporary directory), and the drive decides every acknowledged write: on this machine a sync costs
about 2.4 ms on the system NVMe and 4 to 6 ms on the second drive, where a single writer's write
took 4.4 ms instead of 2.4 and a flush with its save 184 ms instead of 34.

**Kill test** (`taskkill /F` while writes are in flight, restart on the same data, every
acknowledged write checked):
- The log: 0 missing and 0 deleted documents back, over about 46,000 acknowledged writes (log
  alone), 3 rounds of 13,000 to 15,500 acknowledged upserts (segments and log), 3 rounds of 21,000
  to 25,500 (incremental saves), and 3 rounds of 13,500 to 17,500 (merges from sections). Restart is
  about 0.5 s.
- The redb journal it replaced: 5,733 acknowledged writes lost and 958 deletes undone.
- `realtime_load_test` (12 writers upserting and deleting, killed after 4, 5 and 6 s): 3 rounds of
  7,947 to 11,520 acknowledged upserts and 1,320 to 1,908 deletes, 0 missing, 0 back; restart
  0.51 s.

**Write latency and throughput** (`realtime_load_test`):

| Writers | p50 | p99 | Writes/s |
|---|---|---|---|
| 1 | 2.43 ms | 9.03 ms | 311 |
| 16 | 4.50 ms | 11.34 ms | 3,154 |
| 64 | 4.80 ms | 14.15 ms | 10,310 |

The log alone (`realtime_bench`, a write acknowledged once synced): 2.52 ms for one writer, 0.20 ms
a write for 16 (they share syncs), 65 us for 64.

**Search latency while 4 writers write** (36,242 searches during 6,509 writes; `realtime_load_test`
35,513 during 6,179: p50 0.20 ms, p99 0.56 ms):

| Write path | p50 | p99 |
|---|---|---|
| Segments and log (this design) | 0.20 ms | 0.57 ms |
| Overlay and log | 3.90 ms | 5.55 ms |
| No writes | 0.23 ms | 0.55 ms |

**Saves** (600,000 documents, 109 MB on disk; 5 rounds of 500 updates of random documents, each
followed by a flush answered once saved):

| | Flush and save, median | Server working set |
|---|---|---|
| Every segment written again (step 2) | 156 ms (135 to 1,169) | 201 MB |
| What changed (incremental saves) | 34 to 50 ms | 102 to 118 MB |

The first grows with the collection; the second with what changed. Over 8 s of 64 writers of new
documents, saves took 23 ms on average against 197 ms, and the writes ran at 6,341 to 6,448 per
second against 5,970.

**Merges** (10 segments, the server's allocator, 3 runs; the larger ones with a tenth of their
documents written again at random, so nearly every stored block has a deleted copy; measured with
the `merge_speed` test of the time, which compared a rebuild; `realtime_bench` measures merges now:
0.31, 2.86 and 18.4 ms for ten segments of 1, 10 and 100 documents in memory beside a flushed
segment of 3,000, 50.5 and 92.0 ms for ten flushed of 1,000 and 10,000):

| 10 segments of | Rebuilt from documents (before) | From sections |
|---|---|---|
| 1 document (in memory) | 0.58 to 0.60 ms | 0.26 ms |
| 10 documents | 2.9 to 3.5 ms | 1.75 to 1.81 ms |
| 100 documents | 21 to 26 ms | 11.7 to 13.4 ms |
| 1,000 documents of 60 words | 465 to 517 ms | 44 to 48 ms |
| 10,000 documents of 60 words | 1,315 to 1,711 ms | 94 to 98 ms |
| 50,000 documents of 60 words | 3,183 to 3,620 ms | 281 to 289 ms |

**Loads and flushes over HTTP** (imports of 5,000 documents; then 5 rounds of 500 updates, each
followed by a flush answered once saved):

| | Merges rebuilt from documents | From sections |
|---|---|---|
| 600,000 documents of 30 words | 55.0 to 60.4 s (9,900 to 10,900/s) | 40.9 to 47.5 s (12,600 to 14,700/s) |
| 150,000 documents of 300 words | 34.1 to 36.8 s (4,100 to 4,400/s) | 20.1 to 22.4 s (6,700 to 7,500/s) |
| Flush and save after 500 updates, 300 words | 114 to 133 ms | 40 to 41 ms |

Over 8 s of 64 writers of new documents: 6,730 to 6,780 writes per second against 6,300 to 6,350,
and 19 to 25 ms of flushing against 57 to 64 ms. Writes of one document are where they were (one
writer p50 2.6 ms: the sync).

Since the import fix (each import paid about 200 ms for the memory admission check),
`realtime_load_test` loads 150,000 documents of 30 words in 3.2 s (46,900/s) and flushes and saves
after 500 updates in 47 ms (median; 46 to 64); a restart answers in 0.51 s. `realtime_bench`: a
flush and its save after 500 updates of 100,000 documents 54 ms; a restart of a saved collection
with 10,000 writes in its log 130 ms; the segment of a write of one document 317 us (beside a flushed
segment of 3,000), of 32 documents 1.50 ms.

**Searches** (`search_bench`, 100,000 documents; one segment / a realtime collection's 14 segments
as writes leave them, a tenth of the documents written again):

| Search | One segment | 14 segments |
|---|---|---|
| two words, two fields | 1.09 ms | 2.02 ms |
| a prefix | 1.92 ms | 2.43 ms |
| a typo | 1.87 ms | 2.47 ms |
| a filter and a sort | 0.40 ms | 2.16 ms |
| a word, facets | 4.03 ms | 6.14 ms |
| a filter | 52 us | 196 us |

A filter with a sort pays most for the segments: a merge of the flushed segments (or their
background merges catching up) takes it back to one.

**FST builders sized to their keys** (`vendor/fst`, VENDORED.md): the `fst` crate's builder set up
a registry of 20,000 cells (about 1 MB) for every FST, and a write builds several small ones (each
text field's terms, the IDS, string values). With builders sized to their keys a write of one
document built its segment in 239 to 246 us instead of 344 to 354 us, and a merge of ten
one-document segments took 0.26 ms instead of 0.38 ms (the `merge_speed` test of the time, the same
build with the registry forced back to 10,000 rows).

## Next steps

- HNSW: merging the other sources' graphs too (join-set merging), instead of inserting their
  vectors.
- A filter with a sort over many segments: 2.16 ms on 14 against 0.40 ms on one (`search_bench`).
