//! End-to-end coverage of the MCP stdio server.
#![expect(clippy::arithmetic_side_effects, reason = "test request ids")]
#![expect(clippy::excessive_nesting, reason = "test response loop")]
#![expect(clippy::indexing_slicing, reason = "test assertions")]
#![expect(
    clippy::needless_pass_by_value,
    reason = "test helpers own JSON payloads"
)]
#![expect(clippy::panic, reason = "test assertions")]
#![expect(clippy::unwrap_used, reason = "test assertions")]
use std::fs;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::path::PathBuf;
use std::process::Child;
use std::process::ChildStdin;
use std::process::Command;
use std::process::Stdio;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;

const PROTOCOL_VERSION: &str = "2025-06-18";

struct McpClient {
    process: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: i64,
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

impl McpClient {
    fn start(app: &Isolated) -> Self {
        let mut process = Command::new(env!("CARGO_BIN_EXE_ranki"))
            .env("XDG_DATA_HOME", &app.data)
            .env("XDG_CONFIG_HOME", &app.config)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = process.stdin.take().unwrap();
        let stdout = process.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if sender.send(line).is_err() => break,
                    Ok(_) => {}
                }
            }
        });
        let mut client = Self {
            process,
            stdin,
            lines,
            next_id: 0,
        };
        let response = client.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "ranki-ci", "version": "1"},
            }),
        );
        assert_eq!(
            response["result"]["protocolVersion"],
            json!(PROTOCOL_VERSION)
        );
        client.notify("notifications/initialized", json!({}));
        client
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({
            "jsonrpc": "2.0", "method": method, "params": params,
        }));
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({
            "jsonrpc": "2.0", "id": id, "method": method, "params": params,
        }));
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|error| {
                    panic!("MCP server did not answer id {id}: {error}")
                });
            let response: Value = serde_json::from_str(&line).unwrap();
            if response["id"] == json!(id) {
                return response;
            }
        }
    }

    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.request(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        );
        assert!(response.get("error").is_none(), "{response}");
        assert_eq!(response["result"]["isError"], json!(false), "{response}");
        response["result"]["structuredContent"].clone()
    }

    fn tool_failure(&mut self, name: &str, arguments: Value) -> String {
        let response = self.request(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        );
        assert_eq!(response["result"]["isError"], json!(true), "{response}");
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn invalid_params(&mut self, name: &str, arguments: Value) -> String {
        let response = self.request(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        );
        assert_eq!(response["error"]["code"], json!(-32602), "{response}");
        response["error"]["message"].as_str().unwrap().to_owned()
    }
}

struct Isolated {
    data: PathBuf,
    config: PathBuf,
}

fn isolated(dir: &TempDir, name: &str) -> Isolated {
    let root = dir.path().join(name);
    let data = root.join("xdg/data");
    let config = root.join("xdg/config");
    fs::create_dir_all(&data).unwrap();
    fs::create_dir_all(config.join("ranki")).unwrap();
    Isolated { data, config }
}

fn collection(dir: &TempDir, name: &str) -> (Isolated, PathBuf) {
    let app = isolated(dir, name);
    let path = app.data.join("ranki/collection.anki2");
    let output = Command::new(env!("CARGO_BIN_EXE_ranki"))
        .env("XDG_DATA_HOME", &app.data)
        .env("XDG_CONFIG_HOME", &app.config)
        .arg("check")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(path.is_file());
    (app, path)
}

fn fixture() -> (TempDir, McpClient) {
    let dir = tempfile::tempdir().unwrap();
    let (app, _collection) = collection(&dir, "collection");
    let client = McpClient::start(&app);
    (dir, client)
}

fn add_note(
    client: &mut McpClient,
    front: &str,
    deck_name: Option<&str>,
) -> i64 {
    let mut arguments = json!({
        "model_id": 1,
        "fields": [front, "MCP back"],
        "tags": ["stdio"],
        "confirm": true,
    });
    match deck_name {
        Some(name) => {
            arguments["deck_name"] = json!(name);
        }
        None => {
            arguments["deck_id"] = json!(1);
        }
    }
    let added = client.tool("add_note", arguments);
    let ids = added["card_ids"].as_array().unwrap();
    assert_eq!(ids.len(), 1);
    ids[0].as_i64().unwrap()
}

fn add_card(client: &mut McpClient) -> i64 {
    add_note(client, "MCP front", None)
}

fn now_secs() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

#[test]
fn stdio_advertises_ranki_and_complete_tool_metadata() {
    let (_dir, mut client) = fixture();
    let initialize = client.request(
        "initialize",
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "ranki-ci", "version": "1"},
        }),
    );
    assert_eq!(initialize["result"]["serverInfo"]["name"], json!("Ranki"));
    assert!(
        initialize["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("confirm: true")
    );

    let response = client.request("tools/list", json!({}));
    let tools = response["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "add_note",
            "answer_card",
            "create_deck",
            "due_cards",
            "export_apkg",
            "find_cards",
            "get_card",
            "get_cards",
            "get_note",
            "import_apkg",
            "list_decks",
            "list_models",
            "remove_deck",
            "rename_deck",
            "stats",
            "update_note",
        ]
    );
    for tool in tools {
        let name = tool["name"].as_str().unwrap();
        assert!(tool["title"].is_string(), "{name} needs a title");
        assert_eq!(tool["outputSchema"]["type"], json!("object"), "{name}");
        assert!(tool["annotations"]["title"].is_string(), "{name}");
        assert_eq!(tool["annotations"]["openWorldHint"], json!(false));
    }
    for name in [
        "add_note",
        "answer_card",
        "create_deck",
        "export_apkg",
        "update_note",
    ] {
        let tool = tools
            .iter()
            .find(|tool| tool["name"].as_str() == Some(name))
            .unwrap();
        assert_eq!(
            tool["inputSchema"]["properties"]["confirm"]["const"],
            json!(true),
            "{name} must require confirmation"
        );
    }
}

#[test]
fn stdio_adds_answers_and_reports_one_card() {
    let (_dir, mut client) = fixture();
    let card_id = add_card(&mut client);

    let found = client.tool("find_cards", json!({"query": "MCP front"}));
    assert_eq!(found["card_ids"], json!([card_id]));
    assert_eq!(found["has_more"], json!(false));

    let due =
        client.tool("due_cards", json!({"limit": 10, "now_secs": 1_000_000}));
    assert_eq!(due["cards"][0]["id"], json!(card_id));
    assert_eq!(
        due["cards"][0]["note"]["fields"],
        json!(["MCP front", "MCP back"])
    );
    assert_eq!(due["now_secs"], json!(1_000_000));

    let lean = client.tool(
        "due_cards",
        json!({"limit": 10, "now_secs": 1_000_000, "include_notes": false}),
    );
    assert!(lean["cards"][0]["note"].is_null());

    let card = client.tool("get_card", json!({"id": card_id}));
    assert_eq!(card["note"]["fields"], json!(["MCP front", "MCP back"]));
    assert_eq!(card["note"]["tags"], json!(["stdio"]));
    let note_id = card["note"]["id"].as_i64().unwrap();

    let note = client.tool("get_note", json!({"id": note_id}));
    assert_eq!(note["fields"], json!(["MCP front", "MCP back"]));
    let updated = client.tool(
        "update_note",
        json!({
            "id": note_id,
            "expected_modified": note["modified"],
            "fields": ["Edited front", "MCP back"],
            "tags": ["edited"],
            "confirm": true,
        }),
    );
    assert_eq!(updated["fields"], json!(["Edited front", "MCP back"]));

    let stale = client.tool_failure(
        "update_note",
        json!({
            "id": note_id,
            "expected_modified": note["modified"].as_i64().unwrap() - 1,
            "fields": ["Stale", "MCP back"],
            "confirm": true,
        }),
    );
    assert!(stale.contains("note changed; read it again"), "{stale}");

    let batch = client.tool("get_cards", json!({"card_ids": [card_id, 999]}));
    assert_eq!(batch["cards"].as_array().unwrap().len(), 2);
    assert!(!batch["cards"][0]["card"].is_null());
    assert!(batch["cards"][1]["card"].is_null());

    let reviewed = client.tool(
        "answer_card",
        json!({
            "card_id": card_id,
            "rating": "good",
            "elapsed_ms": 1234,
            "now_secs": now_secs(),
            "confirm": true,
        }),
    );
    assert_eq!(reviewed["card"]["id"], json!(card_id));
    assert_eq!(reviewed["revlog"]["ease"], json!(3));
    assert_eq!(reviewed["revlog"]["time_ms"], json!(1234));

    let stats = client.tool("stats", json!({"now_secs": now_secs()}));
    assert_eq!(stats["stats"]["total_cards"], json!(1));
    assert_eq!(stats["stats"]["new_cards"], json!(0));
}

#[test]
fn stdio_discovers_models_and_accepts_names() {
    let (_dir, mut client) = fixture();
    let models = client.tool("list_models", json!({}));
    assert_ne!(
        models["models"].as_array().unwrap().as_slice(),
        &[] as &[Value]
    );
    assert!(models["models"][0]["id"].is_i64());
    assert!(models["models"][0]["fields"].is_array());
    assert_eq!(models["stock_models"].as_array().unwrap().len(), 4);
    assert!(models["stock_models"][0]["id"].is_null());

    let deck =
        client.tool("create_deck", json!({"name": "Study", "confirm": true}));
    let card_id = client.tool(
        "add_note",
        json!({
            "model_name": "basic",
            "deck_name": "study",
            "fields": ["MCP named", "MCP back"],
            "confirm": true,
        }),
    )["card_ids"][0]
        .as_i64()
        .unwrap();
    assert!(card_id > 0);
    assert_eq!(deck["name"], json!("Study"));

    let duplicate = client.tool(
        "add_note",
        json!({
            "model_name": "Basic",
            "deck_name": "Study",
            "fields": ["MCP named", ""],
            "allow_duplicate": true,
            "confirm": true,
        }),
    );
    assert_eq!(duplicate["card_ids"].as_array().unwrap().len(), 1);
}

#[test]
fn stdio_paginates_broad_queries() {
    let (_dir, mut client) = fixture();
    add_note(&mut client, "MCP page one", None);
    add_note(&mut client, "MCP page two", None);
    add_note(&mut client, "MCP page three", None);

    let first =
        client.tool("find_cards", json!({"query": "MCP page", "limit": 2}));
    assert_eq!(first["card_ids"].as_array().unwrap().len(), 2);
    assert_eq!(first["has_more"], json!(true));
    assert_eq!(first["next_offset"], json!(2));

    let second = client.tool(
        "find_cards",
        json!({"query": "MCP page", "limit": 2, "offset": 2}),
    );
    assert_eq!(second["card_ids"].as_array().unwrap().len(), 1);
    assert_eq!(second["has_more"], json!(false));
    assert!(second["next_offset"].is_null());

    let beyond = client.tool(
        "find_cards",
        json!({"query": "MCP page", "offset": 200_000}),
    );
    assert_eq!(beyond["card_ids"], json!([]));
    assert_eq!(beyond["has_more"], json!(false));

    let due =
        client.tool("due_cards", json!({"limit": 2, "now_secs": 1_000_000}));
    assert_eq!(due["cards"].as_array().unwrap().len(), 2);
    assert_eq!(due["has_more"], json!(true));
    assert_eq!(due["next_offset"], json!(2));
}

#[test]
fn stdio_rejects_unconfirmed_and_invalid_calls() {
    let (_dir, mut client) = fixture();
    let refusal = client.tool_failure(
        "add_note",
        json!({"model_id": 1, "deck_id": 1, "fields": ["x"], "confirm": false}),
    );
    assert!(refusal.contains("confirm: true"), "{refusal}");

    assert_eq!(
        client.invalid_params("find_cards", json!({"query": "  "})),
        "query must not be empty"
    );
    assert_eq!(
        client.invalid_params("due_cards", json!({"limit": 201})),
        "limit must be between 1 and 200"
    );
    assert_eq!(
        client.invalid_params("due_cards", json!({"offset": 100_001})),
        "offset must be between 0 and 100000"
    );
    assert_eq!(
        client.invalid_params(
            "answer_card",
            json!({"card_id": 1, "rating": "good", "elapsed_ms": 4_294_967_296_u64, "confirm": true})
        ),
        "elapsed_ms must be at most 4294967295"
    );
    assert_eq!(
        client.invalid_params("get_cards", json!({"card_ids": []})),
        "card_ids must contain between 1 and 200 ids"
    );
    assert_eq!(
        client.invalid_params(
            "add_note",
            json!({"deck_id": 1, "deck_name": "Default", "fields": ["x"], "confirm": true})
        ),
        "use either deck_id or deck_name, not both"
    );
    assert_eq!(
        client.invalid_params(
            "create_deck",
            json!({"name": "A::", "confirm": true})
        ),
        "invalid deck name"
    );
    assert_eq!(
        client.invalid_params(
            "import_apkg",
            json!({"path": "../escape.apkg", "confirm": true})
        ),
        "path must not contain '.' or '..'"
    );
    assert!(
        client
            .tool_failure("get_card", json!({"id": 999}))
            .contains("not found")
    );
    assert!(
        client
            .tool_failure("get_note", json!({"id": 999}))
            .contains("not found")
    );
}

#[test]
fn stdio_exports_and_imports_within_the_collection_directory() {
    let dir = tempfile::tempdir().unwrap();
    let (source, source_collection) = collection(&dir, "source");
    let media_dir =
        source_collection.parent().unwrap().join("collection.media");
    fs::create_dir_all(&media_dir).unwrap();
    fs::write(media_dir.join("image.png"), b"png").unwrap();
    let mut from = McpClient::start(&source);
    from.tool(
        "add_note",
        json!({
            "model_id": 1,
            "deck_id": 1,
            "fields": ["<img src=\"image.png\"> MCP front", "MCP back"],
            "tags": ["stdio"],
            "confirm": true,
        }),
    );

    let exported = from.tool(
        "export_apkg",
        json!({"path": "backup.apkg", "confirm": true}),
    );
    assert_eq!(exported["path"], json!("backup.apkg"));
    assert!(exported["bytes"].as_u64().unwrap() > 0);
    assert!(
        source_collection
            .parent()
            .unwrap()
            .join("backup.apkg")
            .is_file()
    );
    let duplicate = from.tool_failure(
        "export_apkg",
        json!({"path": "backup.apkg", "confirm": true}),
    );
    assert!(duplicate.contains("already exists"), "{duplicate}");
    drop(from);

    let (destination, destination_collection) = collection(&dir, "destination");
    let package = source_collection.parent().unwrap().join("backup.apkg");
    let imported_package =
        destination_collection.parent().unwrap().join("backup.apkg");
    fs::copy(&package, &imported_package).unwrap();
    let mut to = McpClient::start(&destination);
    let imported = to.tool(
        "import_apkg",
        json!({"path": "backup.apkg", "confirm": true}),
    );
    assert_eq!(imported["notes_added"], json!(1));
    assert_eq!(imported["cards_added"], json!(1));
    assert_eq!(imported["media_count"], json!(1));
    let found = to.tool("find_cards", json!({"query": "MCP front"}));
    assert_eq!(found["card_ids"].as_array().unwrap().len(), 1);

    let missing = to.tool_failure(
        "import_apkg",
        json!({"path": "missing.apkg", "confirm": true}),
    );
    assert!(missing.contains("package not found"), "{missing}");
}

#[cfg(unix)]
#[test]
fn stdio_rejects_a_symlinked_package_path() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let (app, collection) = collection(&dir, "symlink");
    let root = collection.parent().unwrap();
    let outside = root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.apkg"), b"outside").unwrap();
    symlink(outside.join("secret.apkg"), root.join("escape.apkg")).unwrap();

    let mut client = McpClient::start(&app);
    let error = client.tool_failure(
        "export_apkg",
        json!({"path": "escape.apkg", "confirm": true}),
    );
    assert!(error.contains("must not be a symlink"), "{error}");
    assert_eq!(fs::read(outside.join("secret.apkg")).unwrap(), b"outside");
}
