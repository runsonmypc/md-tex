#!/usr/bin/env python3
"""Offline test, Linux with X11: no network connections with remote images off.

Usage: offline_check.py <md-tex release binary> <corpus dir>

Copies the media-heavy document (plus three remote image references) next to the corpus
photos, runs the app under `strace -f`, and drives it with xdotool: page through the whole
document, switch to Source, type, save, quit. Every `socket`, `connect`, `sendto` and
`sendmsg` call is checked: Unix sockets (X11, D-Bus, the single-instance socket) are
expected; any internet socket fails.
Remote images must stay at their default, off (no settings file overrides it).
"""
import os, re, shutil, subprocess, sys, tempfile, time


def xdo(*args):
    return subprocess.run(["xdotool", *args], capture_output=True, text=True).stdout.strip()


def main():
    binary, corpus = sys.argv[1], sys.argv[2]
    work = tempfile.mkdtemp(prefix="md-tex-offline-")
    doc = os.path.join(work, "media-heavy.md")
    with open(os.path.join(corpus, "media", "media-heavy.md")) as f:
        text = f.read()
    remote = "".join(f"\n![remote {i}](https://example.com/photo-{i}.png)\n" for i in range(3))
    with open(doc, "w") as f:
        f.write(remote + text)
    os.symlink(os.path.abspath(os.path.join(corpus, "media", "img")), os.path.join(work, "img"))
    log = os.path.join(work, "strace.log")
    # A runtime directory of its own, so the app under test cannot hand the document to an
    # md-tex that is already running (and quit that one at the end).
    run = os.path.join(work, "run")
    os.mkdir(run, 0o700)
    env = dict(os.environ, XDG_RUNTIME_DIR=run)
    p = subprocess.Popen(["strace", "-f", "-qq", "-e", "trace=socket,connect,sendto,sendmsg", "-o", log, binary, doc],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env)
    win = ""
    for _ in range(100):
        win = xdo("search", "--name", "media-heavy.md")
        if win:
            break
        time.sleep(0.1)
    win = win.splitlines()[-1]
    xdo("windowactivate", "--sync", win)
    time.sleep(1.0)
    for _ in range(400):  # page through rendering everything
        xdo("key", "--window", win, "Page_Down")
        time.sleep(0.05)
    xdo("key", "--window", win, "ctrl+Home")
    xdo("key", "--window", win, "ctrl+e")  # Source
    time.sleep(0.5)
    xdo("type", "--window", win, "--delay", "20", "offline edit ")
    xdo("key", "--window", win, "ctrl+s")
    time.sleep(1.0)
    with open(doc) as f:
        saved = "offline edit " in f.read()
    xdo("key", "--window", win, "ctrl+q")
    try:
        p.wait(timeout=10)
    except subprocess.TimeoutExpired:
        p.kill()
        p.wait()
    connects = [l for l in open(log) if "connect(" in l]
    sockets = [l for l in open(log) if "socket(" in l]
    families = {}
    for l in connects:
        fam = re.search(r"sa_family=(AF_\w+)", l)
        key = fam.group(1) if fam else "?"
        families[key] = families.get(key, 0) + 1
    internet = [l.strip() for l in open(log) if re.search(r"AF_INET6?\b", l)]
    print(f"connect calls by family: {families}; sockets created: {len(sockets)}")
    print(f"edit saved: {saved}")
    for l in internet:
        print(f"  internet: {l}")
    ok = saved and not internet
    print("PASS: no internet connections" if ok else "FAIL")
    shutil.rmtree(work, ignore_errors=True)
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
