//! `hearth mcp`: the CLI as tools for AI agents, over the Model Context
//! Protocol, one JSON-RPC message a line on stdin and stdout.
//!
//! The tools are made from the CLI's own definition: each subcommand is a
//! tool, its arguments the tool's input, its help their descriptions. A call
//! runs that command with `--json`. So a new command reaches agents with no
//! code here, and an agent can do what the CLI can and no more: a dishwasher
//! still needs Remote Start. Commands an agent must not run are [`WITHHELD`].

use anyhow::{anyhow, Result};
use clap::ArgAction;
use serde_json::{json, Map, Value};
use std::future::Future;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const PROTOCOL: &str = "2025-06-18";

const INSTRUCTIONS: &str = "Devices in this home, bridged by hearth. Each tool is a hearth \
command. Call devices first: each capability has a kind, a value, its options or range, and \
whether it is active now. A `run` starts (start) or stops (stop) a programme on an appliance \
like a dishwasher: confirm with the person before starting or stopping one.";

/// Commands that are not tools, and why.
const WITHHELD: &[(&str, &str)] = &[
    ("run", "runs the bridge itself"),
    ("mcp", "is this server"),
    ("pin", "prints Home's pairing code"),
    ("unpair", "takes the bridge out of Home"),
    ("remove", "deletes a device on every computer"),
    ("group", "lets computers into the group"),
    ("add", "asks a person questions"),
    ("dyson", "signs in to an account"),
    ("frame", "needs Allow pressed on the TV"),
    ("bosch", "signs in to an account"),
    ("help", "is clap's own"),
];

/// Commands that change something; the rest only look.
const CHANGES: &[&str] = &["set", "rename"];

/// What an agent never sees in a reply.
const PRIVATE: &[&str] = &["pin", "group"];

pub async fn serve(cli: clap::Command) -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = answer(&cli, &line, run).await {
            out.write_all(format!("{reply}\n").as_bytes()).await?;
            out.flush().await?;
        }
    }
    Ok(())
}

/// Runs this binary with `argv`, as a person would at the prompt.
async fn run(argv: Vec<String>) -> Result<Value> {
    let output = tokio::process::Command::new(std::env::current_exe()?)
        .args(&argv)
        .env("NO_COLOR", "1")
        .output()
        .await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let said = stderr.trim().trim_start_matches("Error: ");
        anyhow::bail!("{}", if said.is_empty() { stdout.trim() } else { said });
    }
    Ok(serde_json::from_str(&stdout).unwrap_or_else(|_| json!({ "output": stdout.trim() })))
}

/// The reply to one message, or none for a notification or a response.
/// `run` runs a command line.
async fn answer<F, R>(cli: &clap::Command, line: &str, run: F) -> Option<Value>
where
    F: Fn(Vec<String>) -> R,
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
        "tools/list" => json!({ "tools": tools(cli) }),
        "tools/call" => call(cli, &message["params"], run).await,
        other => return Some(failure(id, -32601, &format!("no method {other}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn offered(cli: &clap::Command) -> impl Iterator<Item = &clap::Command> {
    cli.get_subcommands()
        .filter(|command| !WITHHELD.iter().any(|(name, _)| *name == command.get_name()))
}

/// The arguments a person types, without clap's own and the global ones.
fn inputs(command: &clap::Command) -> impl Iterator<Item = &clap::Arg> {
    command
        .get_arguments()
        .filter(|arg| !arg.is_global_set() && !["help", "version"].contains(&arg.get_id().as_str()))
}

fn tools(cli: &clap::Command) -> Vec<Value> {
    offered(cli)
        .map(|command| {
            let name = command.get_name();
            let properties: Map<String, Value> = inputs(command)
                .map(|arg| (arg.get_id().to_string(), schema(arg)))
                .collect();
            let required: Vec<&str> = inputs(command)
                .filter(|arg| arg.is_required_set())
                .map(|arg| arg.get_id().as_str())
                .collect();
            let reads = !CHANGES.contains(&name);
            json!({
                "name": name,
                "description": command.get_about().map(ToString::to_string).unwrap_or_default(),
                "inputSchema": { "type": "object", "properties": properties, "required": required },
                "annotations": { "readOnlyHint": reads, "destructiveHint": false, "openWorldHint": false },
            })
        })
        .collect()
}

/// An argument's JSON Schema, from its type, choices, default and help.
fn schema(arg: &clap::Arg) -> Value {
    let mut schema = Map::new();
    let kind = if matches!(arg.get_action(), ArgAction::SetTrue) {
        "boolean"
    } else if arg.get_value_parser().type_id()
        == clap::builder::ValueParser::new(clap::value_parser!(u64)).type_id()
    {
        "integer"
    } else {
        "string"
    };
    schema.insert("type".into(), json!(kind));
    let choices: Vec<String> = arg
        .get_possible_values()
        .iter()
        .map(|value| value.get_name().to_owned())
        .collect();
    if !choices.is_empty() && kind == "string" {
        schema.insert("enum".into(), json!(choices));
    }
    if let Some(default) = arg.get_default_values().first() {
        let default = default.to_string_lossy();
        let default = default
            .parse::<u64>()
            .map_or(json!(default), |number| json!(number));
        schema.insert("default".into(), default);
    }
    if let Some(help) = arg.get_help() {
        schema.insert("description".into(), json!(help.to_string()));
    }
    Value::Object(schema)
}

/// A tool's result: the command's JSON, or what it said went wrong.
async fn call<F, R>(cli: &clap::Command, params: &Value, run: F) -> Value
where
    F: Fn(Vec<String>) -> R,
    R: Future<Output = Result<Value>>,
{
    let name = params["name"].as_str().unwrap_or_default();
    let reply = match argv(cli, name, &params["arguments"]) {
        Ok(argv) => run(argv).await.map(private_removed),
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

/// The command line a tool call stands for: positionals in order, then
/// options, then `--json`.
fn argv(cli: &clap::Command, name: &str, args: &Value) -> Result<Vec<String>> {
    if let Some((_, why)) = WITHHELD.iter().find(|(withheld, _)| *withheld == name) {
        anyhow::bail!("{name} is not offered to agents: it {why}");
    }
    let command = offered(cli)
        .find(|command| command.get_name() == name)
        .ok_or_else(|| anyhow!("no tool {name}"))?;
    let text = |value: &Value| match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let mut argv = vec![name.to_owned()];
    let (positional, options): (Vec<_>, Vec<_>) =
        inputs(command).partition(|arg| arg.is_positional());
    for arg in positional {
        match args
            .get(arg.get_id().as_str())
            .filter(|value| !value.is_null())
        {
            Some(value) => argv.push(text(value)),
            None if arg.is_required_set() => anyhow::bail!("{name} needs {}", arg.get_id()),
            None => break,
        }
    }
    for arg in options {
        let Some(value) = args
            .get(arg.get_id().as_str())
            .filter(|value| !value.is_null())
        else {
            continue;
        };
        let flag = format!("--{}", arg.get_long().unwrap_or(arg.get_id().as_str()));
        match arg.get_action() {
            ArgAction::SetTrue if value == &json!(true) => argv.push(flag),
            ArgAction::SetTrue => {}
            _ => argv.extend([flag, text(value)]),
        }
    }
    argv.push("--json".into());
    Ok(argv)
}

fn private_removed(mut reply: Value) -> Value {
    if let Some(object) = reply.as_object_mut() {
        PRIVATE.iter().for_each(|key| {
            object.remove(*key);
        });
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn cli() -> clap::Command {
        crate::command()
    }

    /// Sends `line` to a server whose commands print `reply`, and returns the
    /// answer with the command lines it ran.
    async fn send(line: Value, reply: Value) -> (Option<Value>, Vec<Vec<String>>) {
        let ran = Arc::new(Mutex::new(Vec::new()));
        let run = |argv: Vec<String>| {
            ran.lock().unwrap().push(argv);
            std::future::ready(Ok(reply.clone()))
        };
        let answer = answer(&cli(), &line.to_string(), run).await;
        let ran = ran.lock().unwrap().clone();
        (answer, ran)
    }

    fn call(name: &str, arguments: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": 9, "method": "tools/call",
                "params": { "name": name, "arguments": arguments } })
    }

    #[tokio::test]
    async fn the_tools_are_the_clis_commands_less_the_withheld() {
        let list = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
        let (reply, _) = send(list, Value::Null).await;
        let reply = reply.unwrap();
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["devices", "get", "history", "set", "doctor", "rename"]
        );

        let tool = |name: &str| tools.iter().find(|tool| tool["name"] == name).unwrap();
        let set = tool("set");
        assert_eq!(
            set["inputSchema"]["required"],
            json!(["device", "capability", "value"])
        );
        assert_eq!(set["annotations"]["readOnlyHint"], false);
        let history = &tool("history")["inputSchema"]["properties"];
        assert_eq!(history["hours"]["type"], "integer");
        assert_eq!(history["hours"]["default"], 24);
        assert_eq!(history["usual"]["type"], "boolean");
        assert_eq!(tool("devices")["annotations"]["readOnlyHint"], true);
    }

    #[tokio::test]
    async fn a_call_is_its_command_line_with_json() {
        let (_, ran) = send(
            call(
                "set",
                json!({ "device": "dishwasher", "capability": "run", "value": "start" }),
            ),
            json!({ "ok": true }),
        )
        .await;
        assert_eq!(ran, [["set", "dishwasher", "run", "start", "--json"]]);
        let (_, ran) = send(
            call(
                "history",
                json!({ "device": "fan", "capability": "pm25", "usual": true }),
            ),
            json!({}),
        )
        .await;
        assert_eq!(ran, [["history", "fan", "pm25", "--usual", "--json"]]);
        let (_, ran) = send(
            call("history", json!({ "device": "fan", "hours": 48 })),
            json!({}),
        )
        .await;
        assert_eq!(ran, [["history", "fan", "--hours", "48", "--json"]]);
    }

    #[tokio::test]
    async fn agents_never_see_the_pairing_code_or_run_withheld_commands() {
        let status = json!({ "pin": "111-22-333", "group": { "code": "123 456" }, "devices": [] });
        let (reply, _) = send(call("devices", json!({})), status).await;
        let text = reply.unwrap()["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        assert!(!text.contains("111-22-333") && !text.contains("123 456"));

        let (reply, ran) = send(call("pin", json!({})), Value::Null).await;
        let result = &reply.unwrap()["result"];
        assert_eq!(result["isError"], true);
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("pairing code"));
        assert!(ran.is_empty());
    }

    #[tokio::test]
    async fn a_refusal_is_a_tool_error_not_a_protocol_one() {
        let run = |_argv: Vec<String>| std::future::ready(Err(anyhow!("press Remote Start first")));
        let line = call(
            "set",
            json!({ "device": "dishwasher", "capability": "run", "value": "start" }),
        );
        let reply = answer(&cli(), &line.to_string(), run).await.unwrap();
        assert_eq!(reply["result"]["isError"], true);
        assert!(reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Remote Start"));
    }
}
