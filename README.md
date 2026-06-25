# notion-backup

Back up an accessible Notion workspace to a git repository. The backup runs in
two stages:

1. **Dump (always):** the raw JSON returned by the API is written to
   `<BACKUP_DIR>/json/`, the durable, lossless source of truth.
2. **Render (optional):** when `RENDER_MARKDOWN` is enabled, the JSON is
   converted into Markdown pages + a CSV index per database under
   `<BACKUP_DIR>/markdown/`, which diffs nicely in git.

Because rendering reads from the JSON on disk (never the network), you can
re-render offline at any time with `--render-only` after changing the converter.

**Incremental by default.** When a previous JSON tree exists, the dump only
re-fetches objects whose `last_edited_time` changed; unchanged pages keep their
stored content, skipping the (expensive) recursive block fetches. Discovery still
enumerates the whole workspace each run, so new, moved, and deleted objects are
reconciled. The first run, or any run with `--full` / `FULL_SYNC=true`, fetches
everything.

It uses the **official Notion REST API** (`2025-09-03`) with an internal
integration token. The token does not expire, unlike the unofficial
workspace-export approach. The trade-off is that the integration only sees pages
that have been shared with it (see setup below).

## How it works

1. `POST /v1/search` enumerates every page and data source the integration can
   access (under the `2025-09-03` API, search returns `page` and `data_source`
   objects; each data source is grouped under its parent database).
2. Top-level objects are dumped to disk, recursing into sub-pages and
   sub-databases via their block children. Each object is a node directory named
   `<id>-<slug>` (the Notion id always prefixes the title slug, so names are
   unique and greppable by id). A page node holds `page.json` (the page object +
   its full block tree); a database node holds `database.json` (the database
   object + the ordered list of its rows, which are themselves page nodes).
3. If rendering is enabled, the JSON tree is mirrored into Markdown: every node
   becomes `index.md` with YAML front-matter (id, url, timestamps, and, for
   pages, a `properties:` map of the flattened property values), and every
   `database.json` additionally yields `_index.csv`. References to other pages
   (inline links, mentions, `child_page`/`child_database`, `link_to_page`) are
   rewritten to relative links between the rendered files, so the Markdown tree
   is self-navigable.
4. The JSON tree is reconciled on every run (full wipe + rebuild, or incremental
   prune of stale nodes) and the Markdown tree is regenerated, then committed, so
   deletions in Notion show up as deletions in git.

As it runs, the dump logs per-object progress at `INFO`, e.g.
`[42/310] fetched page: Project Plan` / `[43/310] reused page: Meeting Notes`,
so you can see what it is working on and how far along it is. The total grows as
database rows are discovered (those are not part of the initial enumeration).

Images and file attachments are linked by their (signed, expiring) URLs rather
than downloaded.

Individual objects that have become inaccessible (e.g. an API `404`/`403` on a
block, page, or data source) are logged and skipped rather than aborting the
whole run, so a single broken reference never costs you the backup.

## Setup

1. **Create an integration.** Go to <https://www.notion.so/my-integrations>,
   create a new *internal* integration, and copy the token (`ntn_…`).
2. **Share content with it.** In Notion, open each top-level page or teamspace
   you want backed up, `•••` -> *Connections* -> add your integration. Sharing a
   parent page also covers its sub-pages. Workspace admins can grant broader
   access.
3. **Configure.** Copy `.env.example` to `.env` and fill in `NOTION_TOKEN`
   (and optionally `BACKUP_DIR` / `GIT_REMOTE`).

   ```sh
   cp .env.example .env
   ```

4. **Build.**

   ```sh
   cargo build --release
   ```

## Usage

```sh
cargo run --release                  # incremental JSON dump (+ render if RENDER_MARKDOWN), commit, push if GIT_REMOTE set
cargo run --release -- --full        # force a full re-fetch this run
cargo run --release -- --render      # force the Markdown/CSV render this run
cargo run --release -- --no-render   # force JSON only this run
cargo run --release -- --render-only # re-render from existing JSON, no API calls, no token needed
cargo run --release -- --no-commit   # dump/render only, no git commit
cargo run --release -- --backup-dir /path/to/repo -v
```

The compiled binary is `target/release/notion-backup`; run it directly once
built.

### Options

| Flag            | Effect                                                            |
| --------------- | ----------------------------------------------------------------- |
| `--backup-dir`  | Override the `BACKUP_DIR` target directory.                       |
| `--full`        | Force a full re-fetch instead of an incremental dump.             |
| `--render`      | Render Markdown/CSV this run (overrides `RENDER_MARKDOWN`).        |
| `--no-render`   | Skip the Markdown/CSV render even if `RENDER_MARKDOWN` is set.     |
| `--render-only` | Render from the existing JSON tree only; no fetch, no token.      |
| `--no-commit`   | Dump/render files only; do not create a git commit.               |
| `--no-push`     | Commit, but do not push to the configured remote.                 |
| `-v/--verbose`  | Enable debug logging.                                             |

`--render`, `--no-render`, and `--render-only` are mutually exclusive.

## Configuration

| Variable            | Required | Description                                                            |
| ------------------- | -------- | --------------------------------------------------------------------- |
| `NOTION_TOKEN`      | yes¹     | Internal integration token.                                           |
| `BACKUP_DIR`        | no       | Output directory (default `backup`); contains `json/` and `markdown/`.|
| `RENDER_MARKDOWN`   | no       | Render Markdown/CSV from the JSON dump (default `false`).             |
| `FULL_SYNC`         | no       | Force a full re-fetch every run instead of incremental (default `false`).|
| `GIT_REMOTE`        | no       | Git remote URL; when set, commits are pushed to it.                   |
| `NOTION_API_VERSION`| no       | Override the `Notion-Version` header (default `2025-09-03`).          |

¹ Not required when running `--render-only`, which never contacts the API.

## Development

```sh
cargo test
cargo clippy --all-targets
cargo fmt --check
```

## License

GPLv3 - see [LICENSE](LICENSE).
