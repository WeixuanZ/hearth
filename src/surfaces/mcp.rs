//! `hearth mcp`: the devices as tools for AI agents, over the Model Context
//! Protocol, one JSON-RPC message a line on stdin and stdout.
//!
//! Each tool is one request to the running bridge, the same the CLI and the
//! menu bar send, so an agent can do what they can and no more: a dishwasher
//! still needs Remote Start, a device that is not connected still refuses.
//! The Home pairing code and the group are not shown to agents.

use super::{cli::parse_value, control};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::future::Future;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const PROTOCOL: &str = "2025-06-18";

const INSTRUCTIONS: &str = "Devices in this home, bridged by hearth. Call list_devices \
first: each capability has a kind, a value, its options or range, and whether it is active \
now. Change one with set_capability. A `run` starts (`start`) or stops (`stop`) a programme \
on an appliance like a dishwasher: confirm with the person before starting or stopping one.";

pub async fn serve() -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = answer(&line, control::request).await {
            out.write_all(format!("{reply}\n").as_bytes()).await?;
            out.flush().await?;
        }
    }
    Ok(())
}

/// The reply to one message, or none for a notification or a response.
/// `ask` sends a request to the bridge.
async fn answer<F, R>(line: &str, ask: F) -> Option<Value>
where
    F: Fn(Value) -> R,
    R: Future<Output = Result<Value>>,
{
    let message: Value = match serde_json::from_str(line) {
        Ok(message) => message,
        Err(error) => return Some(failure(Value::Null, -32700, &format!("not JSON: {error}"))),
    };
    let (Some(id), Some(method)) = (message.get("id").cloned(), message["method"].as_str()) else {
        return None;
    };
    let result = match method {
        "initialize" => json!({
            "protocolVersion": PROTOCOL,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "hearth", "version": env!("CARGO_PKG_VERSION") },
            "instructions": INSTRUCTIONS,
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => call(&message["params"], ask).await,
        other => return Some(failure(id, -32601, &format!("no method {other}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn tools() -> Value {
    let device = json!({ "type": "string", "description": "A device's id, name, kind (like tv or dishwasher), or part of its name" });
    let reading =
        json!({ "type": "string", "description": "A reading's id, like temperature or pm25" });
    let reads = json!({ "readOnlyHint": true, "openWorldHint": false });
    json!([
        {
            "name": "list_devices",
            "title": "List devices",
            "description": "Every device, with its state and each capability: kind, value, options or range, and whether it is active now.",
            "inputSchema": { "type": "object", "properties": {} },
            "annotations": reads,
        },
        {
            "name": "get_device",
            "title": "Get a device",
            "description": "One device, with its state and capabilities.",
            "inputSchema": { "type": "object", "properties": { "device": device }, "required": ["device"] },
            "annotations": reads,
        },
        {
            "name": "set_capability",
            "title": "Change a device",
            "description": "Sets a capability: on or off for power and switches, an option id for a choice, a number for a level, a key id for keys (pressing it), start or stop for a run. The device may refuse, and says why.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "device": device,
                    "capability": { "type": "string", "description": "The capability's id, like power, input, speed or run" },
                    "value": { "description": "on, off, start, stop, a number, or an option or key id" },
                },
                "required": ["device", "capability", "value"],
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false },
        },
        {
            "name": "reading_history",
            "title": "Reading history",
            "description": "A device's readings over the last hours, up to 720 (30 days): one sample a minute at most, stored when the value changes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "device": device,
                    "capability": reading,
                    "hours": { "type": "integer", "minimum": 1, "maximum": 720, "default": 24 },
                },
                "required": ["device"],
            },
            "annotations": reads,
        },
        {
            "name": "usual_day",
            "title": "Usual day",
            "description": "A reading's usual day from the last 30: for each hour, the range most days stayed within and their mean, with today so far.",
            "inputSchema": {
                "type": "object",
                "properties": { "device": device, "capability": reading },
                "required": ["device", "capability"],
            },
            "annotations": reads,
        },
    ])
}

/// A tool's result: the bridge's reply as JSON text, or why it failed.
async fn call<F, R>(params: &Value, ask: F) -> Value
where
    F: Fn(Value) -> R,
    R: Future<Output = Result<Value>>,
{
    let args = &params["arguments"];
    let name = params["name"].as_str().unwrap_or_default();
    let reply = match request(name, args) {
        Ok(request) => ask(request).await.map(|reply| shown(name, reply)),
        Err(error) => Err(error),
    };
    let (text, is_error) = match reply {
        Ok(reply) => (
            serde_json::to_string_pretty(&reply).unwrap_or_default(),
            false,
        ),
        Err(error) => (format!("{error:#}"), true),
    };
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// The bridge request a tool makes.
fn request(name: &str, args: &Value) -> Result<Value> {
    let text = |key: &str| {
        args[key]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("{name} needs {key}"))
    };
    Ok(match name {
        "list_devices" => json!({ "op": "status" }),
        "get_device" => json!({ "op": "get", "device": text("device")? }),
        "set_capability" => {
            // Words as the CLI takes them: on, off, start, stop, numbers.
            let value = match &args["value"] {
                Value::String(word) => parse_value(word),
                Value::Null => anyhow::bail!("set_capability needs value"),
                other => other.clone(),
            };
            json!({ "op": "set", "device": text("device")?, "capability": text("capability")?, "value": value })
        }
        "reading_history" => json!({
            "op": "history",
            "device": text("device")?,
            "capability": args["capability"].as_str(),
            "hours": args["hours"].as_u64().unwrap_or(24),
        }),
        "usual_day" => {
            json!({ "op": "normals", "device": text("device")?, "capability": text("capability")? })
        }
        other => anyhow::bail!("no tool {other}"),
    })
}

/// What an agent sees of a reply: the devices, without Home's pairing code
/// or the group.
fn shown(tool: &str, reply: Value) -> Value {
    match tool {
        "list_devices" => json!({ "devices": reply["devices"] }),
        _ => reply,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Sends `line` to a bridge that answers `reply`, and returns the answer
    /// with what the bridge was asked.
    async fn send(line: Value, reply: Value) -> (Option<Value>, Vec<Value>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let ask = |request: Value| {
            asked.lock().unwrap().push(request);
            std::future::ready(Ok(reply.clone()))
        };
        let answer = answer(&line.to_string(), ask).await;
        let asked = asked.lock().unwrap().clone();
        (answer, asked)
    }

    #[tokio::test]
    async fn it_introduces_itself_and_lists_its_tools() {
        let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} });
        let (reply, _) = send(init, Value::Null).await;
        assert_eq!(reply.unwrap()["result"]["serverInfo"]["name"], "hearth");
        let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
        let (reply, _) = send(list, Value::Null).await;
        let names: Vec<_> = reply.unwrap()["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            names,
            [
                "list_devices",
                "get_device",
                "set_capability",
                "reading_history",
                "usual_day"
            ]
        );
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert_eq!(send(note, Value::Null).await.0, None);
    }

    #[tokio::test]
    async fn a_tool_is_one_bridge_request_in_the_clis_words() {
        let call = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
            "name": "set_capability",
            "arguments": { "device": "dishwasher", "capability": "run", "value": "start" },
        }});
        let (reply, asked) = send(call, json!({ "ok": true })).await;
        assert_eq!(
            asked,
            [json!({ "op": "set", "device": "dishwasher", "capability": "run", "value": true })]
        );
        assert_eq!(reply.unwrap()["result"]["isError"], false);
    }

    #[tokio::test]
    async fn agents_see_the_devices_but_not_the_pairing_code() {
        let call = json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "list_devices", "arguments": {} } });
        let status = json!({ "pin": "111-22-333", "group": { "code": "123 456" }, "devices": [{ "id": "tv" }] });
        let (reply, _) = send(call, status).await;
        let text = reply.unwrap()["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(text.contains("\"tv\""));
        assert!(!text.contains("111-22-333") && !text.contains("123 456"));
    }

    #[tokio::test]
    async fn a_refusal_is_a_tool_error_not_a_protocol_one() {
        let ask = |_request: Value| std::future::ready(Err(anyhow!("press Remote Start first")));
        let call = json!({ "jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {
            "name": "set_capability",
            "arguments": { "device": "dishwasher", "capability": "run", "value": "start" },
        }});
        let reply = answer(&call.to_string(), ask).await.unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert!(reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Remote Start"));
    }
}
