# Ranki

Ranki is an Anki-compatible flashcard app written in Rust. It manages its own
Anki-compatible collection, imports and exports packages, and provides a
Wayland-first review window, a command line interface, an AnkiConnect-compatible
API, and a Model Context Protocol server.

## Install and run

With Nix installed:

```bash
nix profile add git+https://github.com/DontBlameMeDev/ranki
ranki --help
```

Run once without installing:

```bash
nix run git+https://github.com/DontBlameMeDev/ranki -- --help
```

Build from a checkout:

```bash
nix develop              # development shell with Rust and Wayland libraries
cargo build
./target/debug/ranki --help

nix build .#default      # packaged binary in ./result/bin/ranki
./result/bin/ranki --help
```

The desktop UI uses GPUI and adapts to phone-width windows. The same `ranki`
package also builds the touch renderer and Android entry point:

```bash
nix develop --command cargo run
nix develop --command cargo run -- --mobile
nix develop --command bash -lc \
  'nix shell nixpkgs#cargo-ndk --command cargo ndk --target arm64-v8a --platform 26 check --package ranki --no-default-features --lib'
CC_aarch64_apple_ios=clang CRATE_CC_NO_DEFAULTS=1 \
  nix develop --command cargo check -p ranki --no-default-features --target aarch64-apple-ios
```

`cargo run` opens the desktop layout. `cargo run -- --mobile` opens the touch
layout in a phone-sized window. Android APK commands are in
`docs/Android-build.md`.

## Home Manager

Add Ranki and Home Manager as flake inputs, both following the same nixpkgs:

```nix
{
  inputs.ranki.url = "git+https://github.com/DontBlameMeDev/ranki";
  inputs.ranki.inputs.nixpkgs.follows = "nixpkgs";
  inputs.home-manager.url = "github:nix-community/home-manager";
  inputs.home-manager.inputs.nixpkgs.follows = "nixpkgs";
}
```

Import the module and enable it:

```nix
{ inputs, ... }:
{
  imports = [ inputs.ranki.homeManagerModules.default ];

  programs.ranki = {
    enable = true;
    settings.ui.theme = "dark";
  };
}
```

For a build without the Wayland review window and its GPUI dependency, use
`cargo build -p ranki --no-default-features`. The CLI, storage, format, and sync crates
build without desktop UI libraries.

```bash
home-manager switch --flake .#user@host
```

The module writes `~/.config/ranki/config.toml` when `settings` is set and leaves an
existing file alone unless `forceConfig = true`. It installs a desktop entry by default
and uses the wrapped Ranki package unless `package` is overridden. The package installs
`assets/rai.svg` as the hicolor icon `ranki`, which the desktop entry uses. The generated
settings use this schema:

```toml
[ui]
theme = "system"   # dark | light | system
font_size = 16.0
review_batch_size = 20
show_keyboard_hints = true
reduce_motion = false # omitted: follow the desktop animation preference
[scheduler]
desired_retention = 0.9
[study]
leech_enabled = true
again = "1"
hard = "2"
good = "3"
easy = "4"
```

Local study preferences live in `[study]`. An empty answer key disables that shortcut.

The GUI keeps window state in `ui-state.toml` next to `config.toml`: the active
screen, sidebar width, expanded decks, selected deck, search queries, and theme
choice. Home Manager does not manage that file, so switching generations does
not undo the layout.

## Collections

Ranki owns one application data directory:

```text
$RANKI_DATA_DIR/            # or $XDG_DATA_HOME/ranki, then ~/.local/share/ranki
├── collection.anki2
├── collection.media/
└── ranki-sync.json
```

Set `RANKI_DATA_DIR` to use an app-owned directory supplied by a platform or a
custom location. Desktop builds otherwise use `XDG_DATA_HOME/ranki` or
`~/.local/share/ranki`. Settings live in `$RANKI_CONFIG_DIR/config.toml`, then
`$XDG_CONFIG_HOME/ranki/config.toml`, then `~/.config/ranki/config.toml`.
The data directory is the storage seam for mobile ports: a platform supplies its
own app data and config directories and the files inside stay the same.

`RANKI_DATA_DIR` is the only way to choose where Ranki stores data. There is no path key
in `config.toml`, `RANKI_COLLECTION` is ignored, an existing `[general]` section is
ignored with a warning, and `--media-dir` is unknown. The hidden `--collection` flag
stays for the performance harness, which is shared with older commits: it must name the
collection inside the data directory and cannot select storage. Ranki does not look up
or open an Anki profile on its own: it never touches `~/.local/share/Anki2` unless you
point `RANKI_DATA_DIR` at it yourself. Its collection is filled by your own
use, by AnkiWeb when sync is enabled, and by packages you import. Use `.apkg` or
`.colpkg` packages to move data between apps. Both export commands write the same Anki
package shape, and import merges a package into the destination collection. Neither
file is a full replacement backup. Most commands create a missing collection. `ranki mcp`
does not.

## Commands

```bash
ranki gui                                      # review window
ranki decks                                    # list deck IDs and names
ranki deck-create NAME
ranki deck-rename ID NAME
ranki deck-remove ID [--force]                 # --force allows non-empty decks
ranki models                                   # ordered fields and templates
ranki due [--deck ID_OR_NAME] [--limit N]      # default limit 20
ranki search 'deck:Default tag:flags'          # prints one card id per line
ranki show CARD_ID                             # card, deck, note, and field values
ranki add-note --model ID_OR_NAME --deck ID_OR_NAME \
  --field 'Front' --field 'Back' [--tag TAG]
ranki note-edit NOTE_ID --field 'Front' --field 'Back' \
  [--tag TAG | --clear-tags] [--deck ID_OR_NAME] [--expected-modified SECS]
ranki add-image-occlusion --deck ID --image PATH --occlusion MARKER
ranki answer CARD_ID RATING                    # 1-4 or again/hard/good/easy
ranki stats [--deck ID_OR_NAME]                # card counts
ranki import FILE.apkg                         # import .apkg or .colpkg package
ranki export FILE.apkg [--force]
ranki export-colpkg FILE.colpkg [--force]
ranki check                                    # create if missing, then verify
ranki connect [--port 8765]                    # AnkiConnect JSON-RPC server
ranki mcp                                      # MCP server over stdio
```

Run `ranki COMMAND --help` for the full argument list.

`add-note` and `search` print one card id per line so you can pipe them into
`show` or `answer`. `due` prints card id, deck id, and due value as tab-separated
columns. Use its first column for `show` or `answer`. `search` supports `deck:`,
`tag:`, `note:`, `is:`, quoted text, and `*`, and prints card ids in search order.
Deck selectors on `add-note`, `due`, and `stats` accept
an id or a case-insensitive name. `--field` values are positional. `models` prints each
note type's ordered field and template names as tab-separated columns.
`show` prints the note id and modified seconds. Pass those to `note-edit` as the
note id and `--expected-modified` to reject stale edits. `note-edit` replaces all
field values in model order. Omitted tags and deck stay unchanged, supplied
`--tag` values replace the tags, and `--clear-tags` removes them.
`add-image-occlusion` accepts `--occlusion`, or one or more `--rect` and `--ellipse`
masks. Those forms cannot be mixed. `export` and `export-colpkg` refuse an existing
destination unless `--force` is set, and always refuse to write to the active collection.
`ranki sync` also uploads, downloads, or checks the remote collection. Section
[Sync](#sync) covers credentials and the optional window login.

## Sync

AnkiWeb sync is optional. In the review window, open Settings and sign in with
your AnkiWeb username and password. Use **Sign out** in the same panel to stop
syncing. Ranki stores only the resulting host key in `ranki-sync.json` in its app data
directory, with mode `0600` on Unix. The password is discarded after login. Ranki
syncs after each completed study session, at launch, and at exit. The sidebar
shows the age of the last successful sync.

Sync flags are global, so they work on any command. `--sync` and `--no-sync` turn syncing
on or off for one run. `--sync-auto` and `--no-sync-auto` control startup and shutdown
sync. `--sync-username USER` overrides the configured username and `--sync-hkey-file PATH`
reads the host key from a file. Persistent settings live in the `[sync]` section of
`config.toml`: `enabled`, `auto`, `username`, `hkey_file`, `retries`, and `backoff_ms`.

Command-line sync accepts a host key or a one-time login without putting a
password in the process arguments:

```bash
export RANKI_SYNC_HKEY='your-host-key'  # or RANKI_SYNC_USERNAME + RANKI_SYNC_PASSWORD
ranki sync --status                     # inspect remote metadata
ranki sync                              # safe automatic direction
ranki sync --full-upload --yes          # replace the remote collection
ranki sync --full-download --yes        # replace the local collection
```

Explicit sync needs credentials or a stored host key, even for `--status`. Without them,
`ranki sync` exits nonzero and names `RANKI_SYNC_HKEY`, `RANKI_SYNC_USERNAME` with
`RANKI_SYNC_PASSWORD`, and `--sync-hkey-file`. Conflicts and retry-later outcomes also
exit nonzero. `--full-upload` and `--full-download` require `--yes` because they replace
one side. A media failure after a successful collection transfer prints a warning and
keeps the successful exit status.

`ranki sync` writes the same state file, so the window and the CLI share one
sync status. Ordinary commands sync the collection only, so a large media
library never delays them. `ranki mcp` skips automatic startup and shutdown
sync so AnkiWeb cannot delay session startup or exit. `ranki sync` also
refreshes media. The review window does that in the background.

## Automation

`ranki connect` serves a subset of the AnkiConnect API on `127.0.0.1`. The
default port is 8765; change it with `--port`. Clients send JSON-RPC POST
requests such as `{"action":"version","version":6}` to `http://127.0.0.1:8765`.

`ranki mcp` speaks Model Context Protocol on stdin and stdout and writes logs to
stderr. Register the binary with an MCP client as a stdio server:

```bash
ranki mcp
```

It uses Ranki's app collection and takes no path flag. It does not create a missing
collection, so run `ranki check` first. The server identifies as Ranki and publishes
instructions, output schemas, tool titles, and read-only or destructive hints. Read
tools cover decks, models, due cards with note text, search, cards, notes, and stats.
Write tools cover deck changes, note edits, answers, and packages. Each requires
`confirm: true`. Package paths are relative to the collection directory. `due_cards` and
`find_cards` return bounded pages with `has_more` and `next_offset`. Pass `next_offset`
as `offset` to continue. See [docs/mcp-design.md](docs/mcp-design.md) for the tool
contracts.

## Keyboard shortcuts

The review window uses these keys:

| Key | Action |
| --- | --- |
| Space, Enter | Reveal the answer |
| 1, 2, 3, 4 | Again, Hard, Good, Easy |
| R | Toggle the answer |
| N | Select another deck |
| F5 | Reload the collection |

## Compatibility

Ranki targets Anki collections with schema 11 through 18 and AnkiConnect API
version 6. Notes, cards, review logs, media, and package import/export are
preserved. Schema 18 stores deck, model, and preset configuration in normalized
protobuf tables; Ranki reconstructs the config JSON needed for its own use
minimally rather than reproducing every Anki setting. Add-on settings and
unused configuration keys may not survive a round trip. Anki's sync protocol
is limited to login, metadata checks, and full collection transfers. Media sync
and delta merges are not implemented, and AnkiConnect support is a subset of the
upstream API.

All reviews use Anki-compatible FSRS: Anki's default parameters unless the
config overrides them, and memory state stored in `cards.data` for Anki to read.

## Testing

```bash
nix build .#checks.x86_64-linux.nextest --print-build-logs
```

Coverage must stay at or above 80%. The floor is set in `.tarpaulin.toml` and
applies to local runs and CI:

```bash
nix build .#checks.x86_64-linux.tarpaulin --print-build-logs
```

Performance is measured across the CLI, AnkiConnect, MCP, GUI, storage,
scheduling, and package surfaces in release builds. A pull request fails the
`Performance audit` check when a workload regresses. See
[docs/performance.md](docs/performance.md) for the method and local commands.
