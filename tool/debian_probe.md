# Debian userspace diagnostics on macOS ARM64

These probes use a separately prepared, disposable Debian trixie-slim AMD64
rootfs. They require a VFS-enabled Blink (`./configure --enable-vfs`). They do
not download or unpack an image. Blink exposes host resources; use trusted
executables only. The rootfs should contain a usable `/etc/resolv.conf` for DNS.

```sh
python3 tool/debian_probe.py --blink o/blink/blink --rootfs /path/to/rootfs \
  --output o/probe-dns --case dns --trace
python3 tool/debian_probe.py --blink o/blink/blink --rootfs /path/to/rootfs \
  --output o/probe-apt --case apt --repeat 10
python3 tool/debian_probe.py --blink o/blink/blink --rootfs /path/to/rootfs \
  --output o/probe-fork --case fork
```

Each invocation requires a new output directory and runs both `-m` and `-jm`.
It retains stdout, stderr, Blink logs and a JSON summary. Timeouts kill the
process group. The APT probe requires no package indexes/caches and expects exit
100 (missing packages); any SIGBUS in child stderr still makes the probe fail.
It is not a package installation success test. Use a fresh rootfs without APT
indexes so packages cannot install. DNS requires network access; the fork probe
and APT missing-package probe do not need network access.

## Initial findings after the brk page-size fix

- Host DNS succeeds, but Debian `getent ahostsv4 deb.debian.org` exits 2 in
  both modes. Its trace ends with `setsockopt(SOL_IP, IP_RECVERR)` returning
  EINVAL, after which glibc closes the socket without sending a DNS query.
- macOS does not expose Linux's IP_RECVERR option. glibc's
  [resolver reopen path](https://github.com/bminor/glibc/blob/release/2.41/master/resolv/res_send.c)
  treats failure of `__res_enable_icmp` as fatal. Merely changing EINVAL to
  ENOPROTOOPT would not enable this path. Proper error-reporting compatibility
  needs investigation; the current PR does not pretend the option is supported.
- In an initial 10-run APT error-path sample, SIGBUS appeared twice with JIT
  and zero times without JIT. This suggests a JIT interaction but does not prove
  the cause. Traced runs and simple Bash fork/pipe runs passed. APT can still
  return 100 when its child crashes, so checking only the top-level status misses
  this problem.
- A subsequent five-run sample per mode using this tool did not reproduce
  SIGBUS. The failure remains intermittent; a passing run does not establish
  that it is fixed.

The tested base manifest was
`sha256:4876fdea29cac1a33a3195f1eb1952feb1fca7dd6dd00be3e2f1879cf20d34d2`.
These are opt-in diagnostic tools, separate from the deterministic function-test
suite.

## Fork/JIT follow-up

Temporary macOS ARM64 signal instrumentation located the host fault PC and fault
address inside `g_code`, reached from `ExecuteInstruction` while calling an
inherited compiled path immediately after guest clone/fork. That instrumentation
is not part of the patch.

The fork child now restores executable JIT access with `FixJitProtection`, using
the existing Apple write-protection helper. A clean control build reproduced
SIGBUS in 5/30 JIT APT probes and 0/30 interpreter probes. With the fix, all 30
probes per mode passed with no SIGBUS. This is integration evidence for an
intermittent failure, not proof against every possible SIGBUS. Use `--repeat 30` with
the command above to repeat this integration check. Simple warmed-code fork
smokes also pass before the fix, so they are not a regression reproducer.

## Next Debian build barriers

Setting guest `RES_OPTIONS=use-vc` enables glibc's TCP DNS path and permits APT
index downloads. It does not implement UDP IP_RECVERR compatibility.

After downloading the trixie indexes, `apt-cache gencaches` fails when growing
its default cache. The syscall trace contains:

```text
mremap(0x2000011ec000, 0x1800000, 0x1900000, 0x1, 0) -> -ENOMEM
```

`SysMremap` currently returns ENOMEM unconditionally. The diagnostic command
`apt-cache -o APT::Cache-Start=134217728 gencaches` succeeds with those indexes.
Starting with a larger allocation bypasses this particular growth request; it
does not add mremap support or establish package installation compatibility.
