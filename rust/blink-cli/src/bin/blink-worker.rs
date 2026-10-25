//! Trusted host-side JSON-lines adapter. Guest output never shares the protocol.
use blink::{Blink, Command, GuestArch, RuntimeConfig, Termination};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{self, BufRead, Write},
    path::PathBuf,
    time::Duration,
};
const MAX_REQUEST: u64 = 1024 * 1024;
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Create {
        context: String,
        rootfs: PathBuf,
        #[serde(default)]
        rootless_metadata: bool,
        #[serde(default)]
        network: bool,
        #[serde(default = "memory_limit")]
        max_memory: usize,
        #[serde(default = "virtual_limit")]
        max_virtual_memory: usize,
        #[serde(default = "image_limit")]
        max_image_size: usize,
        #[serde(default = "instruction_limit")]
        max_instructions: u64,
    },
    Run {
        context: String,
        argv: Vec<String>,
        #[serde(default = "root_directory")]
        cwd: PathBuf,
        #[serde(default)]
        env: BTreeMap<String, String>,
        #[serde(default)]
        uid: u32,
        #[serde(default)]
        gid: u32,
        timeout_ms: u64,
        stdout: PathBuf,
        stderr: PathBuf,
    },
    Close {
        context: String,
    },
}
fn memory_limit() -> usize {
    512 * 1024 * 1024
}
fn virtual_limit() -> usize {
    4 * 1024 * 1024 * 1024
}
fn image_limit() -> usize {
    256 * 1024 * 1024
}
fn instruction_limit() -> u64 {
    1_000_000_000
}
fn root_directory() -> PathBuf {
    PathBuf::from("/")
}
fn dispatch(contexts: &mut BTreeMap<String, Blink>, request: Request) -> Result<Value, String> {
    match request {
        Request::Create {
            context,
            rootfs,
            rootless_metadata,
            network,
            max_memory,
            max_virtual_memory,
            max_image_size,
            max_instructions,
        } => {
            if context.is_empty() || contexts.contains_key(&context) {
                return Err("context is empty or already exists".into());
            }
            if contexts.len() >= 64 {
                return Err("worker context limit exceeded".into());
            }
            let mut config = RuntimeConfig::new(rootfs, GuestArch::X86_64);
            config.rootless_metadata = rootless_metadata;
            config.network = network;
            config.max_memory = max_memory;
            config.max_virtual_memory = max_virtual_memory;
            config.max_image_size = max_image_size;
            config.max_instructions = max_instructions;
            contexts.insert(context, Blink::create(config).map_err(|e| e.to_string())?);
            Ok(json!({"ok":true}))
        }
        Request::Run {
            context,
            argv,
            cwd,
            env,
            uid,
            gid,
            timeout_ms,
            stdout,
            stderr,
        } => {
            let runtime = contexts.get_mut(&context).ok_or("unknown context")?;
            if argv.is_empty() || timeout_ms == 0 {
                return Err("nonempty argv and positive timeout_ms required".into());
            }
            if stdout == stderr {
                return Err("stdout and stderr must be distinct files".into());
            }
            let command = Command::new(&argv[0])
                .args(argv[1..].iter().cloned())
                .cwd(cwd)
                .uid(uid)
                .gid(gid)
                .envs(env)
                .timeout(Duration::from_millis(timeout_ms));
            let mut out = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(stdout)
                .map_err(|e| e.to_string())?;
            let mut err = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(stderr)
                .map_err(|e| e.to_string())?;
            let result = runtime
                .run_with_io(command, &mut out, &mut err)
                .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())?;
            err.flush().map_err(|e| e.to_string())?;
            let (termination, code) = match result.termination {
                Termination::Exited(code) => ("exited", Some(code)),
                Termination::Signaled(signal) => ("signaled", Some(signal)),
                Termination::TimedOut => ("timed_out", None),
                Termination::Cancelled => ("cancelled", None),
                Termination::InstructionLimit => ("instruction_limit", None),
            };
            Ok(
                json!({"ok":true,"termination":termination,"code":code,"instructions":result.instructions}),
            )
        }
        Request::Close { context } => {
            let mut runtime = contexts.remove(&context).ok_or("unknown context")?;
            runtime.close().map_err(|e| e.to_string())?;
            Ok(json!({"ok":true}))
        }
    }
}
fn serve(input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    let mut input = input;
    let mut contexts = BTreeMap::new();
    let result = (|| {
        loop {
            let mut line = Vec::new();
            // Bounded reads also protect against a peer that never sends a newline.
            let count = io::Read::take(&mut input, MAX_REQUEST + 1).read_until(b'\n', &mut line)?;
            if count == 0 {
                break;
            }
            if line.len() as u64 > MAX_REQUEST {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "worker request exceeds 1 MiB",
                ));
            }
            let reply = serde_json::from_slice::<Request>(&line)
                .map_err(|e| e.to_string())
                .and_then(|request| dispatch(&mut contexts, request));
            let value = match reply {
                Ok(value) => value,
                Err(error) => json!({"ok":false,"error":error}),
            };
            serde_json::to_writer(&mut output, &value)?;
            output.write_all(b"\n")?;
            output.flush()?;
        }
        Ok(())
    })();
    let mut cleanup = Ok(());
    for (_, mut runtime) in contexts {
        if let Err(error) = runtime.close() {
            cleanup = Err(io::Error::other(error.to_string()));
        }
    }
    result.and(cleanup)
}
fn main() {
    if let Err(error) = serve(io::stdin().lock(), io::stdout().lock()) {
        eprintln!("blink-worker: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_requests_do_not_desynchronize_protocol() {
        let mut replies = Vec::new();
        serve(
            io::Cursor::new(b"not-json\n{\"op\":\"close\",\"context\":\"missing\"}\n"),
            &mut replies,
        )
        .unwrap();
        let replies: Vec<Value> = replies
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["ok"], false);
        assert_eq!(replies[1]["error"], "unknown context");
    }
    #[test]
    fn oversized_request_is_rejected_before_json_allocation() {
        let request = vec![b' '; MAX_REQUEST as usize + 1];
        assert_eq!(
            serve(io::Cursor::new(request), Vec::new())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
