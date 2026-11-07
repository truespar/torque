# Security

## Reporting a vulnerability

Report security problems privately, not in a public issue:

- email **security@tic.io**, or
- use GitHub's private vulnerability reporting (the repository's **Security** tab, **Report a
  vulnerability**).

Tell us the Torque version, how to reproduce the problem and what an attacker could do with it.
We'll confirm we got your report, keep you posted while we work on a fix, and credit you in the
release notes unless you'd rather stay anonymous. Give us reasonable time to ship a fix
before you disclose the problem.

## Supported versions

Security fixes go into the latest release. Upgrade to it to get them.

## What's in scope

Everything in this repository: the server (HTTP, TCP ingest and MCP endpoints), Studio, `torque-admin`,
the clients, the native segment builder and the segment and import file formats. A crafted file or request that
crashes the process counts, as does anything that gets past an API key, a scoped key or its collection
limits.

## Running a server safely

- **Authentication.** HTTP requests need an API key (`TORQUE_API_KEY`, generated at startup when
  unset). `/health` and `/metrics` don't.
- **The TCP ingest port takes no key.** It listens on every interface by default (port 8109), and
  anyone who reaches it can write to any collection. Bind it to a private address (`--ingest-addr`),
  put it behind a firewall, or list its clients in `ingest_allow`, which takes exact IP addresses, not
  ranges.
- **Network.** Listen on a private address or behind a firewall; `allowed_ips` limits HTTP clients to
  CIDR ranges. Turn on TLS (`--tls-cert`, `--tls-key`) on anything that leaves the machine.
- **Destructive operations.** `allow_drop_collection = false` disables deleting collections through
  the API (allowed by default).
- **The banner prints a generated API key.** Set your own where stdout ends up in logs.
- **Search Assistant.** When `ai_provider` is set, Studio sends users' questions, the collection's
  schema and the editor's search to that provider; the provider's key stays on the server.
