use blink::{Blink, Command, GuestArch, RuntimeConfig, Termination};
use std::{path::PathBuf, time::Duration};
const USAGE:&str="usage: blink-cli [--root ROOT] [--cwd DIR] [--arch x86_64|aarch64] [--timeout-ms N] [--max-instructions N] [--] PROGRAM [ARGS...]";
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("blink-rs: {e}");
            std::process::exit(125);
        }
    }
}
fn run() -> blink::Result<i32> {
    let mut args = std::env::args().skip(1);
    let mut root = "/".to_string();
    let mut root_set = false;
    let mut cwd = None;
    let mut arch = GuestArch::X86_64;
    let mut timeout = None;
    let mut budget = None;
    let mut trace = false;
    let mut rootless_metadata = false;
    let program = loop {
        match args.next().as_deref() {
            Some("--help" | "-h") => {
                println!("{USAGE}");
                return Ok(0);
            }
            Some("--trace") => trace = true,
            Some("--rootless-metadata") => rootless_metadata = true,
            Some("--root") => {
                root = args.next().ok_or_else(usage)?;
                root_set = true;
            }
            Some("--cwd") => cwd = Some(PathBuf::from(args.next().ok_or_else(usage)?)),
            Some("--arch") => {
                arch = match args.next().as_deref() {
                    Some("x86_64") => GuestArch::X86_64,
                    Some("aarch64") => GuestArch::Aarch64,
                    _ => return Err(usage()),
                }
            }
            Some("--timeout-ms") => {
                timeout = Some(Duration::from_millis(
                    args.next()
                        .ok_or_else(usage)?
                        .parse()
                        .map_err(|_| usage())?,
                ))
            }
            Some("--max-instructions") => {
                budget = Some(
                    args.next()
                        .ok_or_else(usage)?
                        .parse()
                        .map_err(|_| usage())?,
                )
            }
            Some("--") => break args.next().ok_or_else(usage)?,
            Some(p) if !p.starts_with('-') => break p.to_string(),
            _ => return Err(usage()),
        }
    };
    let mut config = RuntimeConfig::new(root, arch);
    config.trace = trace;
    config.rootless_metadata = rootless_metadata;
    if let Some(b) = budget {
        config.max_instructions = b;
    }
    let mut runtime = Blink::create(config)?;
    let mut command = Command::new(program);
    command.args = args.collect();
    command.timeout = timeout;
    command.cwd = match cwd {
        Some(c) => c,
        None if !root_set => std::env::current_dir()?,
        _ => PathBuf::from("/"),
    };
    let outcome = runtime.run(command)?;
    runtime.close()?;
    Ok(match outcome.termination {
        Termination::Exited(c) => c,
        Termination::Signaled(s) => 128 + s,
        Termination::TimedOut => 124,
        Termination::Cancelled => 130,
        Termination::InstructionLimit => {
            eprintln!("blink-rs: instruction limit exceeded");
            125
        }
    })
}
fn usage() -> blink::Error {
    blink::Error::InvalidConfig(USAGE.into())
}
