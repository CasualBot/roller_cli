# Roller CLI

A command-line client for the [ROLLER](https://github.com/rollersoftware/) **REST** and **Reporting** APIs, built so both people and AI agents can drive a [ROLLER](https://github.com/rollersoftware/) venue from a terminal.

- **All 88 documented operations**, generated 1:1 from [ROLLER](https://github.com/rollersoftware/)'s official docs repo ([rollersoftware/public-api-documentation](https://github.com/rollersoftware/public-api-documentation)): 63 REST + 25 Reporting.
- **Self-describing**: `roller describe` returns any operation's parameters, request-body schema and response shapes as JSON; `roller example` prints a request body to start from; `roller docs` prints [ROLLER](https://github.com/rollersoftware/)'s own guides.
- **Safe by default**: `--dry-run` shows the exact request without sending it, and any non-GET call on a **live** venue requires `--yes`.
- **Handles the API's rules for you**: OAuth tokens are cached and reused ([ROLLER](https://github.com/rollersoftware/) suspends credentials that request a token per call), refreshed once on `401`, `429`s are retried, and Reporting results can be paged automatically.
- **Multiple venues**: one saved profile per venue and environment, with secrets kept in the OS's protected storage.
- A single native binary that works the same in PowerShell, cmd, bash, zsh and sh on Windows, macOS and Linux.

> Using an AI agent? Point it at [`skills/roller/SKILL.md`](skills/roller/SKILL.md). It covers how to discover operations, build requests and stay safe on live venues.

---

## Install

You need a Rust toolchain ([rustup.rs](https://rustup.rs)). Installing puts `roller` in `~/.cargo/bin`, which rustup adds to your PATH.

```bash
git clone git@github.com:CasualBot/roller_cli.git
cd roller_cli
cargo install --path .
```

Check it:

```bash
roller
```

Running `roller` with no arguments prints the command guide and the venue you're currently using.

**Updating:** `git pull && cargo install --path . --force`.

---

## Set up a venue

[ROLLER](https://github.com/rollersoftware/) API credentials belong to **one venue in one environment**. Generate them in Venue Manager under **Settings → Integrations → API keys → Create client key**. Playground and Live have separate keys. If you don't see API keys, your venue needs [ROLLER](https://github.com/rollersoftware/)'s API access add-on.

```bash
roller login                 # Playground credentials (the default)
roller login --env live      # Live credentials
```

`login` asks for the client ID and the secret (hidden as you type). It checks them against [ROLLER](https://github.com/rollersoftware/) and saves them as a profile named after the venue, for example `funland-downtown-live`, which becomes your current venue.

| Task | Command |
| --- | --- |
| List saved venues (`*` = current) | `roller profiles` |
| Switch venue | `roller use funland-downtown-live` |
| Use another venue for one command | `roller --profile funland-downtown-play rest get-venue-detail` |
| Check what commands will hit | `roller whoami` |
| Remove a venue | `roller logout funland-downtown-play` |

Choose your own profile name with `--profile`. If it has spaces, put it in quotes.

### Non-interactive login (CI, scripts, 1Password)

The secret is never accepted as a command-line argument, because that would leave it in shell history. Pipe it in instead:

```bash
op read "op://Vault/ROLLER Funland/client_secret" | roller login --env live --client-id <id> --secret-stdin
```

Alternatively, skip saved profiles entirely with environment variables. They take priority over any profile:

```bash
export ROLLER_CLIENT_ID=...
export ROLLER_CLIENT_SECRET=...
export ROLLER_ENV=live        # optional, defaults to play
```

PowerShell: `$env:ROLLER_CLIENT_ID = "..."`. To inject them from 1Password: `op run --env-file roller.env -- roller whoami`.

### Where credentials are stored

Everything lives in `~/.roller/`:

| File | Contents |
| --- | --- |
| `config.json` | profiles: environment, client ID, venue name/ID, and the secret, protected per OS (below) |
| `token-<env>-<client-id>.json` | the cached access token (about 24h), reused until it expires or returns `401`; removed by `logout` or when its profile is replaced |
| `ops.json` | only present after `roller sync`; overrides the built-in operation table |

How the secret and the cached token are protected:

- **Windows:** both encrypted with DPAPI, so only your Windows account on that machine can read them.
- **macOS:** the secret is in the login Keychain (service `roller-cli`). The token is in a file only you can read.
- **Linux:** both are in files only you can read. Files are created `0600` inside a `0700` folder.

Use `ROLLER_HOME` to put this folder somewhere else.

If your venue has a dedicated API domain, set `ROLLER_BASE_URL=https://...` to use it. It must be `https://`, because your client secret and token are sent there. Any host other than [ROLLER](https://github.com/rollersoftware/) Playground counts as **live**, so writes to it need `--yes`.

---

## Use it

### Find an operation

```bash
roller ops                    # all 88
roller ops booking            # filter by any word: name, path, method or summary
roller ops --json             # machine-readable, including a usage line per operation
```

Operations are grouped by API: `roller rest <operation>` and `roller report <operation>`. Names match [ROLLER](https://github.com/rollersoftware/)'s documentation URLs, so `docs.roller.app/docs/api/rest/operations/get-booking-detail` is `roller rest get-booking-detail`.

### Learn what it needs

```bash
roller rest get-product-availability -h        # flags, types, required/optional
roller describe rest create-a-booking          # full JSON: params, body schema, responses, docs link
roller example rest create-a-booking           # body template built from ROLLER's examples
```

### Call it

```bash
roller rest get-venue-detail
roller rest get-booking-detail --uniqueId 79189348-70fe-4347-bc0f-8887df841a3a
roller rest search-for-bookings --date 2026-10-12
roller report get-bookings --startDate 2026-10-12 --endDate 2026-10-13 --all-pages
```

Flags use the API's own parameter names exactly, including their casing (`--uniqueId`, `--startDate`).

**Request bodies** go in `--body` as a file (`@file.json`), from stdin (`-`), or inline JSON:

```bash
roller example rest create-a-booking > booking.json   # edit it, then:
roller rest create-a-booking --body @booking.json --dry-run
roller rest create-a-booking --body @booking.json
```

> **PowerShell:** inline JSON loses its quotes when passed to native programs, so use `@file.json` or pipe the JSON in with `--body -`.

### Options for any call

| Flag | Effect |
| --- | --- |
| `--dry-run` | Print the method, URL, headers (auth redacted) and body instead of sending. No login needed. |
| `--yes` | Required for any non-GET call on a **live** venue. Without it, roller refuses and exits 1. |
| `--all-pages` | Reporting endpoints: keep fetching `pageNumber` until the last page, then print one combined array. |
| `--compact` | One-line JSON instead of pretty-printed. |
| `--profile NAME`, `--env live\|play` | Override the current venue for this command. |

### Output and exit codes

The API's response JSON goes to **stdout**. Messages and errors go to **stderr**, prefixed `roller:`. On an HTTP error the response body is still printed to stdout, so you can see [ROLLER](https://github.com/rollersoftware/)'s error message.

| Exit code | Meaning |
| --- | --- |
| 0 | success |
| 1 | CLI, config, credential or network error, or a live write refused without `--yes` |
| 2 | bad arguments (unknown flag, missing required option, invalid enum value) |
| 4 | the API returned 4xx |
| 5 | the API returned 5xx |

### ROLLER's guides

```bash
roller docs                         # list guides by section
roller docs checkout-workflow       # print one (fetched live from ROLLER's docs repo)
roller docs --json
```

The guides cover workflows that one operation description can't: checkout, editing or refunding bookings, discounts, price rules, membership reporting, webhooks, rate limits and date formats.

---

## API rules worth knowing

These come from [ROLLER](https://github.com/rollersoftware/)'s docs; run `roller docs <topic>` for detail.

- **Rate limits:** REST allows 600 requests per minute per credential set and Reporting allows 120. roller retries `429`s, but don't share one credential set across several apps (`rate-limits-rest-api`, `rate-limits-reporting-api`).
- **Reporting date ranges** must not exceed 1 day. Dates are in the venue's local time, and most endpoints filter on the record's *modified* date (`filtering-and-pagination`).
- **Playground first:** test against Playground credentials. Clicking **Update Playground** in Venue Manager resets it and invalidates its API keys (`environments`).
- Dates and times use ISO 8601 (`formatting`).

---

## Keeping up with ROLLER's API

The operation table (`roller_ops.json`) is generated from [ROLLER](https://github.com/rollersoftware/)'s docs repo and built into the binary.

```bash
roller sync                                   # refresh ~/.roller/ops.json from GitHub (no rebuild needed)
roller sync --out roller_ops.json             # in this repo: regenerate the built-in table, then rebuild
```

The test `embedded_table_covers_every_operation` checks that the table loads, that every operation with a body has a schema and a usable example, and that clap accepts all generated subcommands.

**Out of scope:** the [ROLLER](https://github.com/rollersoftware/) Payments flow is a browser-side Adyen payment form, not an API call (`roller docs roller-payments`). Webhooks are *received* by your server; roller can create and manage them (`roller ops webhook`) but doesn't receive them.

---

## Development

```bash
cargo test
cargo run -- ops booking
```

| Path | Purpose |
| --- | --- |
| `src/main.rs` | CLI, auth and profiles, HTTP, describe/example |
| `src/sync.rs` | builds `roller_ops.json` from the docs repo, inlining every `$ref` |
| `roller_ops.json` | generated operation table built into the binary; don't edit by hand |
| `skills/roller/SKILL.md` | the guide for AI agents |

On Windows, if the link step fails with `LNK1104`, the project path is too long. Build into a short target directory instead: `CARGO_TARGET_DIR=C:/tmp/roller-target cargo build`.

---

## License

Copyright (C) 2026 CasualBot. Licensed under the [GNU General Public License v3.0 or later](LICENSE).

You may use, modify and redistribute this software. If you distribute it, modified or not, you must do so under the same license with its source code available. It comes with **no warranty**, and the authors accept **no liability** for any damage or loss from its use (see sections 15–16 of the license).

`roller_ops.json` and `roller docs` content are derived from [ROLLER](https://github.com/rollersoftware/)'s public API documentation ([rollersoftware/public-api-documentation](https://github.com/rollersoftware/public-api-documentation)), which remains [ROLLER](https://github.com/rollersoftware/)'s. This project isn't affiliated with or endorsed by [ROLLER](https://github.com/rollersoftware/).
