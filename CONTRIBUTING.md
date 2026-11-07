# Contributing to Torque

Thanks for helping. Bug reports, fixes, tests, docs and features are all welcome.

## Before you start

- **Bugs**: open an issue with the version, what you ran (the schema, the search, the API call) and
  what happened. A few documents that reproduce it help.
- **Bigger changes** (a new feature, a change to the segment format or a public API): open an issue
  first so we can agree on the approach before you write the code.
- **Security problems**: don't open an issue; see [SECURITY.md](SECURITY.md).

## Building and testing

The [README](README.md) covers building. On Windows, run cargo from a Visual Studio 2022 developer
prompt.

```bash
cargo test --lib                        # unit, crash and random-operation tests
cargo test --test api_test              # one integration suite; the rest are under tests/
cargo check --workspace --all-targets --features tls,mcp
```

- **More random operations**: `TORQUE_MODEL_SEEDS=5000 cargo test --release --lib many_random_operations -- --ignored`
  runs the HTTP API against a reference, crashes included, and cuts a failing seed to the fewest
  operations.
- **Durability**: every change to the data directory goes through `persistence::disk`, so
  `collection::crash_tests` can crash and fail each one. A new durable step gets a scenario there.
- **Benchmarks**: `cargo bench --bench search_bench`, or `realtime_bench`; `TORQUE_BENCH_DIR` names
  the drive.
- **Studio**: `cd studio && npm ci && npx vue-tsc --noEmit`, and `npx vite build` to rebuild the
  bundle the server embeds.
- **.NET**: `cd clients/dotnet && dotnet test tests/Torque.Client.Tests`.

## What we look for

- **Tests** for the change: a regression test for a bug, coverage for a feature.
- **No warnings** from `cargo check`.
- **Comments** that say why, briefly, for a reader who wasn't there.
- **Your own work.** Typesense compatibility comes from its public documentation and from a running
  server treated as a black box. Typesense is GPL-3.0: don't port, translate or closely follow its
  source or its tests, here or in a dependency you add.
- **Release notes**: user-visible changes get an entry in [RELEASE_NOTES.md](RELEASE_NOTES.md),
  written for users.

## Pull requests

Keep a pull request to one change and describe what it does and why. Maintainers review it here; an
accepted change is applied to the development tree and published with the next update of this
repository, and the release notes credit you.

## License

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
Torque by you, as defined in the Apache-2.0 license, shall be dual licensed as MIT OR Apache-2.0,
without any additional terms or conditions.
