use blink::{Blink, Command, GuestArch, RuntimeConfig, Termination};
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
    let mut arch = GuestArch::X86_64;
    let program = loop {
        match args.next().as_deref() {
            Some("--root") => root = args.next().ok_or_else(usage)?,
            Some("--arch") => {
                arch = match args.next().as_deref() {
                    Some("x86_64") => GuestArch::X86_64,
                    Some("aarch64") => GuestArch::Aarch64,
                    _ => return Err(usage()),
                }
            }
            Some("--") => break args.next().ok_or_else(usage)?,
            Some(p) => break p.to_string(),
            None => return Err(usage()),
        }
    };
    let mut runtime = Blink::create(RuntimeConfig::new(root, arch))?;
    let mut command = Command::new(program);
    command.args = args.collect();
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
    blink::Error::InvalidConfig(
        "usage: blink-cli [--root ROOT] [--arch x86_64|aarch64] [--] PROGRAM [ARGS...]".into(),
    )
}
