use std::process::ExitCode;

use clap::Parser;

mod cli;
mod cmd;

fn main() -> ExitCode {
    let parsed = cli::Cli::parse();
    match dispatch(parsed) {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => ExitCode::from(code),
    }
}

fn dispatch(cli: cli::Cli) -> Result<(), u8> {
    use cli::Command;
    match (cli.command, cli.path, cli.update) {
        (Some(Command::Init { path, no_gitignore }), _, _) => {
            cmd::init::run(path.as_deref(), no_gitignore)
        }
        (Some(Command::Index), _, _) => cmd::index::run(None),
        (Some(Command::Update), _, _) => cmd::update::run(None),
        (Some(Command::Query { question, dfs, budget }), _, _) => {
            cmd::query::run(&question, dfs, budget)
        }
        (Some(Command::Follow { symbol, from, budget }), _, _) => {
            cmd::follow::run(&symbol, from.as_deref(), budget)
        }
        (Some(Command::Refs { symbol, limit }), _, _) => cmd::refs::run(&symbol, limit),
        (Some(Command::Slice { location, budget }), _, _) => {
            cmd::slice::run(&location, budget)
        }
        (Some(Command::Path { from, to }), _, _) => cmd::path::run(&from, &to),
        (Some(Command::Explain { symbol }), _, _) => cmd::explain::run(&symbol),
        (Some(Command::Map { scope, depth }), _, _) => cmd::map::run(scope.as_deref(), depth),
        (Some(Command::Meter { json, since, reset, diff, record_input, record_output }), _, _) => {
            cmd::meter::run(json, since.as_deref(), reset, diff, record_input, record_output)
        }
        (Some(Command::Watch { debounce }), _, _) => cmd::watch::run(debounce),
        (Some(Command::Add { url }), _, _) => cmd::add::run(&url),
        (Some(Command::Mcp), _, _) => cmd::mcp::run(),
        (None, Some(path), _) if !path.exists() => {
            eprintln!(
                "lens: '{}' is not a known subcommand and does not exist as a path. \
                 Run `lens --help`.",
                path.display()
            );
            Err(2)
        }
        // graphify-compat: `lens <path> --update` updates the index.
        (None, Some(path), true) => cmd::update::run(Some(&path)),
        // graphify-compat: `lens <path>` indexes that path.
        (None, Some(path), false) => cmd::index::run(Some(&path)),
        (None, None, _) => {
            println!("{}", short_help());
            Ok(())
        }
    }
}

fn short_help() -> &'static str {
    "lens — symbol-aware code map for Claude\n\n\
     USAGE:\n  \
     lens <SUBCOMMAND> [OPTIONS]\n  \
     lens [PATH] [--update]    (graphify-compat)\n\n\
     SUBCOMMANDS:\n  \
     init      create .lens/ index in the project\n  \
     index     full build\n  \
     update    incremental rebuild\n  \
     query     BFS/DFS query (graphify-compat)\n  \
     follow    Ctrl+Click — def + minimal slice for a symbol\n  \
     refs      list callers of a symbol\n  \
     slice     minimal context for a file:line\n  \
     path      shortest path between two symbols (graphify-compat)\n  \
     explain   plain-language explanation of a symbol (graphify-compat)\n  \
     add       fetch a URL into .lens/raw/ and index it (graphify-compat)\n  \
     map       architecture summary\n  \
     meter     persistent token meter\n  \
     watch     reindex on file changes\n  \
     mcp       stdio MCP server\n\n\
     Run `lens --help` for full clap-rendered help."
}
