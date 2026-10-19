// SPDX-License-Identifier: Apache-2.0

//! Stdio MCP server.
//!
//! Search and browse are always offered. Export and upload are offered only
//! when the matching flag is set for this process, and both go through
//! `copy_verify`. They return a plan and write nothing unless `--apply` or
//! `--execute` is set. There is no delete tool.

use std::io::BufReader;
use std::path::PathBuf;
use std::process::ExitCode;

use albumen_mcp::io;
use albumen_mcp::Library;
use clap::Parser;
use serde_json::{json, Value};

#[derive(Debug, Parser)]
#[command(
    name = "albumen-mcp",
    about = "MCP server for an Albumen library. Read-only unless write flags are set. Writes also require --apply or --execute. No delete tool."
)]
struct Cli {
    /// Library directory. Files here are not modified by search, browse, or export.
    #[arg(long)]
    library: PathBuf,
    /// Offer the export tool. Off unless this flag is present.
    #[arg(long)]
    enable_export: bool,
    /// Offer the upload tool. Off unless this flag is present.
    #[arg(long)]
    enable_upload: bool,
    /// Let export and upload write. Without this or `--execute`, those tools return a plan.
    #[arg(long)]
    apply: bool,
    /// Same as `--apply`.
    #[arg(long)]
    execute: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let library = match Library::new(
        cli.library,
        cli.enable_export,
        cli.enable_upload,
        cli.apply || cli.execute,
    ) {
        Ok(library) => library,
        Err(err) => {
            eprintln!("albumen-mcp: {err}");
            return ExitCode::from(2);
        }
    };
    let stdin = std::io::stdin();
    let mut input = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut output = std::io::BufWriter::new(stdout.lock());
    let mut initialized = false;
    loop {
        let message = match io::read_message(&mut input) {
            Ok(Some(message)) => message,
            Ok(None) => return ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("albumen-mcp: {err}");
                let _ =
                    io::write_message(&mut output, &error(Value::Null, -32700, err.to_string()));
                return ExitCode::from(1);
            }
        };
        if message.get("method").is_none() {
            continue;
        }
        let method = message["method"].as_str().unwrap_or("");
        let Some(id) = message.get("id") else {
            continue;
        };
        if method != "initialize" && !initialized {
            let _ = io::write_message(
                &mut output,
                &error(id.clone(), -32600, "server not initialized"),
            );
            continue;
        }
        let response = match method {
            "initialize" => {
                initialized = true;
                ok(id.clone(), initialize_result(&message["params"]))
            }
            "ping" => ok(id.clone(), json!({})),
            "tools/list" => ok(id.clone(), json!({"tools": library.tool_defs()})),
            "tools/call" => ok(id.clone(), call_tool(&library, &message["params"])),
            _ => error(id.clone(), -32601, format!("method not found: {method}")),
        };
        if io::write_message(&mut output, &response).is_err() {
            return ExitCode::from(1);
        }
    }
}

fn initialize_result(params: &Value) -> Value {
    let requested = params["protocolVersion"].as_str().unwrap_or("");
    let supported = ["2024-11-05", "2025-03-26", "2025-06-18"];
    let version = if supported.contains(&requested) {
        requested
    } else {
        "2024-11-05"
    };
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": "albumen-mcp", "version": "0.1.1"},
    })
}

fn call_tool(library: &Library, params: &Value) -> Value {
    let Some(name) = params["name"].as_str() else {
        return tool_error("missing tool name");
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match library.call(name, &arguments) {
        Ok(value) => tool_ok(value),
        Err(err) => tool_error(err),
    }
}

fn tool_ok(value: Value) -> Value {
    json!({
        "content": [{"type": "text", "text": serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string())}],
        "isError": false,
    })
}

fn tool_error(message: impl Into<String>) -> Value {
    json!({
        "content": [{"type": "text", "text": message.into()}],
        "isError": true,
    })
}

fn ok(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: Value, code: i32, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message.into()},
    })
}
