# Builtin MCP server design

`ranki mcp` is a stdio Model Context Protocol server implemented in `crates/bin/src/mcp.rs`. It uses the same `Store` and FSRS scheduler as the CLI and takes no path flag.

## Startup and discovery

A missing collection is a startup error. Run `ranki check` first to create and verify the collection. The server canonicalizes the collection directory, opens the collection once, and serves one client per process. It never starts the GUI, binds a socket, or writes protocol data to stdout. Logs go to stderr.

The server identifies as `Ranki`. Its instructions point clients to `list_models`, `due_cards`, and the `confirm: true` rule for writes. Every tool publishes a title, a description, a closed input schema with `additionalProperties: false`, an output schema for its structured result, and annotations including `openWorldHint: false`. Results carry JSON text in `content` and the same value in `structuredContent`.

## Tool surface

| Tool | Input highlights | Result |
| --- | --- | --- |
| `list_decks` | none | `decks` |
| `list_models` | none | `models` and `stock_models` with names, ordered fields, and ordered templates |
| `create_deck` | `name`, `confirm` | new deck |
| `rename_deck` | `id`, `name`, `confirm` | renamed deck |
| `remove_deck` | `id`, `confirm` | removed deck, destructive |
| `due_cards` | `deck_id`, `limit`, `offset`, `now_secs`, `include_notes` | cards with note text by default, `now_secs`, `has_more`, `next_offset` |
| `find_cards` | `query`, `limit`, `offset` | `card_ids`, `has_more`, `next_offset` |
| `get_card` | `id` | card and note, or a not-found tool error |
| `get_cards` | `card_ids`, 1 to 200 | cards and notes in request order, null entries for missing ids |
| `get_note` | `id` | note, or a not-found tool error |
| `add_note` | `model_id` or `model_name`, `deck_id` or `deck_name`, `fields`, optional `tags` and duplicate options, `confirm` | `card_ids` |
| `update_note` | `id`, `deck_id` or `deck_name`, optional `expected_modified`, `fields`, `tags`, `model_id` or `model_name`, `confirm` | updated note |
| `answer_card` | `card_id`, `rating`, optional `elapsed_ms` and `now_secs`, `confirm` | updated card and review log |
| `stats` | optional `deck_id` and `now_secs` | card counts |
| `import_apkg` | `path`, `confirm` | `notes_added`, `cards_added`, `media_count` |
| `export_apkg` | `path`, `confirm` | `path`, `bytes`. The target must not exist |

All write tools require `confirm: true`: the deck tools, `add_note`, `update_note`, `answer_card`, `import_apkg`, and `export_apkg`. A missing or false value returns a tool error and performs no write.

`model_name` matches an installed or stock note type case-insensitively and installs a stock type when needed. `deck_name` matches a deck case-insensitively. `model_id` and `model_name` are mutually exclusive, as are `deck_id` and `deck_name`. Exactly one form is required for a deck and, when a model is supplied, for a model. `update_note` uses the same deck selector to move a note's cards. Its optional `expected_modified` rejects stale edits when the note changed after the client read it. `add_note` duplicate options are `allow_duplicate` (default false), `duplicate_scope` (`collection` or `deck`, default `collection`), `check_children`, and `check_all_models`.

`due_cards` and `find_cards` return bounded pages. Pass `next_offset` as `offset` while `has_more` is true. Continuation is offset-based, so inserts or deletes between calls can shift the remaining items. `now_secs` defaults to the server clock.

Shared shapes use the field names and numeric codes from `crates/model/src/lib.rs`. `Card.due` keeps its type-dependent meaning from `docs/architecture.md`.

```text
Deck  = {id:int, name:string, description:string}
Model = {id:int|null, name:string, fields:string[], templates:string[]}
Card  = {id:int, note_id:int, deck_id:int, ordinal:int, modified:int, usn:int,
         card_type:"new"|"learning"|"review"|"relearning",
         queue:"new"|"learning"|"review"|"day_learning"|"suspended"|"buried"|"preview",
         due:int, interval:int, factor:int, reps:int, lapses:int, left:int,
         original_due:int, original_deck_id:int, flags:int, data:string}
Note  = {id:int, guid:string, model_id:int, modified:int, usn:int,
         tags:string[], fields:string[], sort_field:string, checksum:int,
         flags:int, data:string}
RevLog= {id:int, card_id:int, usn:int, ease:int (1 again, 2 hard, 3 good, 4 easy),
         interval:int, last_interval:int, factor:int, time_ms:int,
         kind:"learn"|"review"|"relearn"|"filtered"|"manual"}
Stats = {new_cards:int, learning_cards:int, review_cards:int, total_cards:int}
```

`Card.queue` reflects the stored queue. It is not derived from the card type.

## Errors and safety

Malformed calls, unknown fields, invalid enum values, and out-of-range values return protocol `invalid_params` errors. Valid calls that cannot complete return tool errors with a short message the model can act on, including missing card, note, deck, and model ids, duplicate notes, cards that are not due, missing packages, and existing export targets. No tool panics, includes a backtrace, or logs to stdout.

File access is limited to the collection directory. Package paths are relative to it. The parent is canonicalized, paths outside it are rejected, and a symlink in the final component is rejected. Import merges notes, cards, and media into the destination without deleting existing records. Media created by a failed import is rolled back. Export reads the collection and writes one file.

`remove_deck` is destructive and the other writes are not. `openWorldHint` is false for every tool because all work stays in the local collection. One MCP client owns one server process and the mutable `Store` is not shared with the HTTP server.

## Coexistence with `ranki connect`

`ranki connect` remains the AnkiConnect-compatible HTTP JSON-RPC API for Anki tooling. `ranki mcp` is a local stdio process launched by an MCP client. There is no shared port, no HTTP inside `mcp`, and no MCP inside `connect`. Both call the same `Store` implementations and the same scheduler through thin adapters.
