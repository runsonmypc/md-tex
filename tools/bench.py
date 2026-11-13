#!/usr/bin/env python3
"""The budget harness.

Usage: bench.py <md-tex binary built with --features bench> <corpus dir> [--size] [--runs N]

Build the bench binary in its own target directory, so the release binary stays as shipped:
    CARGO_TARGET_DIR=target/bench cargo build --release -p mdt-app --features bench

The corpus is `mdt-bench generate <dir>`. Measures every budget in `mdt-bench budgets`,
writes results.json next to the corpus, and exits with `mdt-bench check`'s status: zero
only when every budget is met.

- Startup: median of N launches (default 20, after one warm-up) on text-100k.md.
- Large file, typing, mode switch, preview, find, media: one scripted run each
  (hooks in crates/mdt-app/src/bench.rs).
- Memory: private memory excluding window surfaces (macOS: `footprint` minus
  IOSurface/IOAccelerator; Linux: Private_* in smaps minus /dev/dri mappings), after the
  100 KB document has rendered, and the peak while paging through the media corpus.
- Binary size: builds the release binary (target/release) and reports its size. With
  --size, also builds it once without
  each optional feature (about 6 minutes per build) and reports each feature's share.
"""
import json, os, platform, re, statistics, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MAC = platform.system() == "Darwin"


def launch(binary, doc, mode=None):
    env = dict(os.environ, MDT_LAUNCH_NS=str(time.time_ns()))
    if mode:
        env["MDT_BENCH"] = mode
    return subprocess.Popen([binary, doc], env=env, stdout=subprocess.PIPE, text=True)


def run(binary, doc, mode=None, timeout=600):
    p = launch(binary, doc, mode)
    out, _ = p.communicate(timeout=timeout)
    for line in reversed(out.splitlines()):
        if line.startswith("{"):
            return json.loads(line)
    raise RuntimeError(f"no report from {mode or 'startup'} run: {out!r}")


def private_mb(pid):
    if MAC:
        out = subprocess.run(["footprint", "-p", str(pid)], capture_output=True, text=True).stdout
        unit = {"B": 1 / 1024, "KB": 1, "MB": 1024, "GB": 1024 * 1024}
        total = excluded = 0.0
        for line in out.splitlines():
            m = re.match(r"\s*([\d.]+)\s+(B|KB|MB|GB)\s+[\d.]+\s+(?:B|KB|MB|GB)\s+[\d.]+\s+(?:B|KB|MB|GB)\s+\d+\s+(.*)$", line)
            if m and ("IOSurface" in m.group(3) or "IOAccelerator" in m.group(3)):
                excluded += float(m.group(1)) * unit[m.group(2)]
            t = re.search(r"phys_footprint:\s+([\d.]+)\s+(B|KB|MB|GB)", line)
            if t:
                total = float(t.group(1)) * unit[t.group(2)]
        return (total - excluded) / 1024
    kb, skip = 0, False
    with open(f"/proc/{pid}/smaps") as f:
        for line in f:
            if re.match(r"^[0-9a-f]+-[0-9a-f]+ ", line):
                skip = "/dev/dri/" in line
            elif not skip and line.startswith(("Private_Clean:", "Private_Dirty:")):
                kb += int(line.split()[1])
    return kb / 1024


def memory_text(binary, doc):
    p = launch(binary, doc, "hold")
    try:
        for line in p.stdout:  # ready, 3 s after the whole document is parsed
            if '"ready"' in line:
                break
        return private_mb(p.pid)
    finally:
        p.kill()
        p.wait()


def media(binary, doc):
    """Pages through the media corpus; samples memory at every page."""
    p = launch(binary, doc, "media")
    peak, result = 0.0, None
    for line in p.stdout:
        if line.startswith('{"page"'):
            peak = max(peak, private_mb(p.pid))
        elif line.startswith("{") and "equation_first_ms" in line:
            result = json.loads(line)
    p.wait()
    if result is None:
        raise RuntimeError("no report from the media run")
    result["memory_media_peak_mb"] = round(peak, 1)
    return result


def size_report(binary):
    """Release size, and each optional feature's share (each build skips one feature)."""
    full = os.path.getsize(binary) / 1e6
    features = ["math", "pikchr", "mermaid", "svg", "remote-images"]
    grammars = ["rust", "python", "javascript", "typescript", "json", "bash", "toml", "yaml", "c", "go"]
    base = (["runtime-shaders"] if MAC else []) + features + [f"hl-{g}" for g in grammars]
    shares = {}
    target = os.path.join(ROOT, "target", "size")
    for name, drop in [(f, [f]) for f in features] + [("highlighting", [f"hl-{g}" for g in grammars])]:
        keep = [f for f in base if f not in drop]
        subprocess.run(
            ["cargo", "build", "-q", "--release", "-p", "mdt-app", "--no-default-features",
             "--features", ",".join(keep)],
            cwd=ROOT, check=True, env=dict(os.environ, CARGO_TARGET_DIR=target))
        without = os.path.getsize(os.path.join(target, "release", "md-tex")) / 1e6
        shares[name] = round(full - without, 2)
        print(f"  {name}: {shares[name]:.2f} MB", file=sys.stderr)
    return shares


def main():
    args = sys.argv[1:]
    size = "--size" in args
    runs = int(args[args.index("--runs") + 1]) if "--runs" in args else 20
    binary, corpus = [a for a in args if not a.startswith("--") and not a.isdigit()][:2]
    small, mid, large = (os.path.join(corpus, f"text-{s}.md") for s in ("100k", "1m", "10m"))
    heavy = os.path.join(corpus, "media", "media-heavy.md")
    release = os.path.join(ROOT, "target", "release", "md-tex")

    def step(name):
        print(f"{name}...", file=sys.stderr)

    step("startup")
    run(binary, small)  # warm the file cache
    startup = [run(binary, small)["startup_first_text_frame_ms"] for _ in range(runs)]
    step("large file")
    run(binary, large)
    big = run(binary, large, "large")
    step("scroll")
    scroll = run(binary, large, "scroll")
    step("idle (65 s)")
    idle = run(binary, small, "idle")
    step("typing")
    typing = run(binary, mid, "type")
    step("mode switch")
    switch = run(binary, mid, "switch")
    step("preview")
    preview = run(binary, mid, "preview")
    step("find")
    find = run(binary, large, "find")
    step("memory")
    mem_text = memory_text(binary, small)
    step("media corpus")
    rich = media(binary, heavy)
    step("binary size")
    subprocess.run(["cargo", "build", "-q", "--release", "-p", "mdt-app"], cwd=ROOT, check=True)
    shares = size_report(release) if size else None

    metrics = {
        "startup_first_text_frame_ms": round(statistics.median(startup), 2),
        "large_file_first_frame_ms": big["large_file_first_frame_ms"],
        "large_file_headings_ready_ms": big["large_file_headings_ready_ms"],
        "scroll_frame_cpu_p99_ms": scroll["scroll_frame_cpu_p99_ms"],
        "scroll_missed_frames": scroll["scroll_missed_frames"],
        "idle_frames_presented": idle["idle_frames_presented"],
        "idle_cpu_percent": idle["idle_cpu_percent"],
        "input_latency_median_ms": typing["input_latency_median_ms"],
        "input_latency_p99_ms": typing["input_latency_p99_ms"],
        "mode_switch_ms": switch["mode_switch_ms"],
        "preview_update_ms": preview["preview_update_ms"],
        "find_keystroke_ms": find["find_keystroke_ms"],
        "memory_text_mb": round(mem_text, 1),
        "memory_media_peak_mb": rich["memory_media_peak_mb"],
        "equation_first_ms": rich["equation_first_ms"],
        "equation_median_ms": rich["equation_median_ms"],
        "diagram_ms": rich["diagram_ms"],
        "jpeg_12mp_ms": rich["jpeg_12mp_ms"],
        "binary_size_mb": round(os.path.getsize(release) / 1e6, 2),
    }
    detail = {
        "machine": f"{platform.system()} {platform.release()} {platform.machine()}",
        "startup_runs": runs,
        "startup_min_ms": round(min(startup), 2),
        "startup_max_ms": round(max(startup), 2),
        "scroll_frame_period_ms": scroll["scroll_frame_period_ms"],
        "mode_switch_median_ms": switch["mode_switch_median_ms"],
        "mode_switch_to_reader_max_ms": switch["mode_switch_to_reader_max_ms"],
        "preview_update_median_ms": preview["preview_update_median_ms"],
        "find_keystroke_median_ms": find["find_keystroke_median_ms"],
        **{k: rich[k] for k in ("equation_max_ms", "equations_rendered", "diagram_median_ms",
                                "diagrams_rendered", "jpeg_12mp_median_ms", "images_rendered",
                                "media_pages")},
    }
    if shares is not None:
        detail["feature_size_mb"] = shares
    results = {"platform": "macos" if MAC else "linux", "metrics": metrics, "detail": detail}
    out = os.path.join(corpus, "results.json")
    with open(out, "w") as f:
        json.dump(results, f, indent=1)
    print(json.dumps(results, indent=1))
    check = subprocess.run(["cargo", "run", "-q", "-p", "mdt-bench", "--", "check", out], cwd=ROOT)
    sys.exit(check.returncode)


if __name__ == "__main__":
    main()
