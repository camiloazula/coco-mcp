//! The one binary. `coco-mcp`, alone or with `--desktop`, opens the window;
//! `coco-mcp --cli …` runs the command line, with everything after the flag
//! passed on as it is. Everything else lives in the two libraries.

#![forbid(unsafe_code)]
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::process::ExitCode;

const USAGE: &str = "Usage: coco-mcp [--desktop]          open the window
       coco-mcp --cli <COMMAND>…     run a command; `coco-mcp --cli --help` lists them
       coco-mcp --version | --help";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next();
    match mode.as_deref().and_then(|mode| mode.to_str()) {
        None | Some("--desktop") => {
            let filter = tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,mcp_core=debug".into());
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(std::io::stderr)
                .init();
            match coco_mcp::run() {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("error: {error:#}");
                    ExitCode::from(2)
                }
            }
        }
        Some("--cli") => coco_cli::run_cli(args),
        Some("--version" | "-V") => {
            println!("{} {}", coco_mcp::APP_NAME, env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            println!("{}\n\n{USAGE}", coco_mcp::APP_NAME);
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "{}: unknown argument `{}`\n\n{USAGE}",
                coco_mcp::APP_NAME,
                mode.unwrap_or_default().to_string_lossy()
            );
            ExitCode::from(2)
        }
    }
}
