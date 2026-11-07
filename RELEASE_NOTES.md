# Release notes

All notable changes to Torque are documented in this file.

---

## 0.7.0

### Open source

- **Torque is now open source.** The server, Studio, the clients and the native segment builder are licensed under MIT OR Apache-2.0.
- **Studio's Search Assistant runs on your own model.** Set `ai_provider` (`anthropic`, or `openai` for any OpenAI-compatible endpoint) and `ai_api_key` in `torque.toml` or as `TORQUE_AI_*`. The assistant stays hidden until you set it and it only gets the collection's schema and never the documents.
- The Go client is at `github.com/truespar/torque/clients/go`: `go get` it instead of downloading an archive.

### Upgrading

- **Every collection has to be built again.** Segment files are TQBF version 6, and files of any other version are not compatible. Rebuild the native segment builder and the .NET client together with the server. The server starts over older files and sets those collections aside: their searches answer 404 until they are built again, and `DELETE /collections/{name}` removes them, which frees the name.
- Data directories of earlier versions are not migrated.
- `overlay_flush_interval_ms` and `--overlay-flush-interval-ms` are gone.
- A document without a language is stemmed in the collection's `default_language`, not in English.

### Text search

- **Text search is Torque's own implementation, so results and `text_match` differ from Typesense's.** Every search parameter stays accepted with its documented meaning. `text_match` is BM25F behind words matched, typos, completions and a field holding exactly the query; its values, and the order of equally good hits, are Torque's own. A search over any split of a collection's documents gives what one segment holding them gives.
- **Words.** Only whitespace and `token_separators` split words; other punctuation is dropped from inside a word (`non-stick` is one word, `nonstick`). Letters of every script fold to ASCII, so "шипулин" finds "Шипулин" in any field, and every Unicode space splits words in any field.
- **Prefixes** keep their accents ("trä" finds "trädgård", not "transport"; "tra" finds both), and find stemmed words as written.
- `max_candidates` defaults to 4 at any collection size, as documented.
- A `default_sorting_field` still sorts hits; it doesn't decide which candidate words are tried.
- Snippets are the first match with `highlight_affix_num_tokens` tokens on each side, `...` where cut.
- A `"phrase"` in `q` is checked against the stored text, so a phrase whose words many documents hold costs more than a plain search.
- `prioritize_token_position` has no effect, and neither does a field's or a synonym's `locale`: tokenization is the same for every language.
- `prefix=false` matches the last word of a multi-word query whole.

### Name matching

- **`name_matching`, a field option for names.** A `name_matching` field's words are also found by their passport spelling ("Oedegaard" and "Ødegård") and by sound ("Mohammed" and "Muhamad"). The search parameter `name_matching` (`off`, `fallback`, `always`, per `query_by` field) sets when.
- **`name_score=true`** orders the first `name_score_candidates` hits by how well their names match, parts paired in any order, with the evidence on each hit. `name_score_kind` tells people from organizations.
- **`explain_matches=true`** lists, per query word, the term it matched and how.

### Realtime collections

- **Writes are segments, made durable by a write-ahead log.** A write is on disk before it is acknowledged, and a restart replays the log.
- **Saves write what changed**, and saved segments serve from their mapped files.
- **Segments merge from their sections** instead of decoding every document and building again.
- Collection info reports `realtime` (`memory_documents`, `deleted_documents`, `segments`, `last_flush_at`, `wal_bytes`), with the metrics `torque_memory_documents` and `torque_deleted_documents`.

### Performance

- **14 of the Studio benchmark's 15 queries are faster than on 0.6.3** (309K documents, the same machine): text search 1.98 ms to 0.70, facets 1.06 to 0.17, a filter on two fields 0.55 to 0.23. The 15th, a sort alone (0.14 to 0.16 ms), 0.6.3 answered wrongly. Against Typesense 30.1 the geometric mean is 16.5x at one client and 9.5x at 24, against Quickwit 0.8.2 13.3x and 8.0x.
- Nested AND filters on arrays of objects read the arrays' element sequences, not each document (836 ms to 4 ms on 1M documents); facets on arrays count by what costs less (11 ms to 0.3 ms); a filter and a sort on the same field no longer walk the values the filter leaves empty (9.6 ms to 1.4 ms).
- Every hit is written once: a response was copied whole to sort its keys.
- Ingest over TCP: 49K documents a second until every one is searchable, 7.7x Typesense's HTTP import.

### Fixes

- **String filters find documents of every language.** A filter on a stemmed field missed the documents whose language is not the collection's: `category:=Electronics` found none of the Swedish ones. The .NET client's fields are stemmed by default.
- `highlights` lists top-level fields only, as Typesense's first format does; a field inside an object is in `highlight`.
- `/metrics` stays bounded: requests count under the route they matched, and a deleted collection's series go with it.
- A power loss, a crash or a failed write at any step of a change to the data directory leaves a state the server recovers: renames and removals are durable on Windows and Unix, and a collection whose files can't be read is set aside instead of being overwritten.
- A restart before a flush keeps every field of the documents it replays, those outside the schema too.
- An import no longer costs about 200 ms on Windows whatever its size.
- `facet_return_parent` gives a parent for numbers and for fields in arrays of objects, `max_facet_values` has no cap, and a collection never written to answers `facet_by` with each facet field, empty.
- Collection answers carry `default_sorting_field`, `token_separators`, `symbols_to_index` and, per field, `locale`, `stem_dictionary` and `store`. `POST /collections`, `POST /keys`, `POST /config` and a document `POST` answer 201.
- A filter condition without a `:` is a 400 before its field is looked up.
- .NET client: searches of a `string[]` field read their highlights.
- The MCP server's search tool takes `prefix` and `num_typos` as JSON values or text.
- The Docker image generates an API key.
