#!/usr/bin/env python3
"""The Mac app, run for real (after `make package`): started as the Finder starts it (no
shell environment), with a scratch backend folder and no model, twice. Checks that

  - the window's page is the app's own (tauri://localhost, from the bundle) and reaches the
    backend the app started, on ports the system gave (127.0.0.1 only), across origins: the
    page gets the token from the app, asks the backend's status with it, and keeps it in its
    own storage;
  - a second start gets other ports, and the same page origin: what the page kept is there;
  - quitting the app stops its backend.

The page's storage is WebKit's, for the app (~/Library/WebKit/app.causewaybay.code): read
only, to find the token the page kept. Nothing else outside the scratch folder is touched.

  python3 scripts/app_test.py [path/to/Causewaybay Code.app]"""
import glob, os, re, signal, sqlite3, shutil, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BUNDLE = sys.argv[1] if len(sys.argv) > 1 else f"{ROOT}/desktop/target/release/bundle/macos/Causewaybay Code.app"
WEBKIT = os.path.expanduser("~/Library/WebKit/app.causewaybay.code/WebsiteData/Default")

def wait(check, what, seconds=30):
    end = time.time() + seconds
    while time.time() < end:
        value = check()
        if value: return value
        time.sleep(0.2)
    raise AssertionError(f"timed out waiting for {what}")

def page_storage():
    """What the page on tauri://localhost keeps (key -> text), read from a copy."""
    for origin in glob.glob(f"{WEBKIT}/*/*/origin"):
        if b"tauri" in open(origin, "rb").read() and b"localhost" in open(origin, "rb").read():
            db = os.path.join(os.path.dirname(origin), "LocalStorage", "localstorage.sqlite3")
            if not os.path.exists(db): continue
            copy = tempfile.mkdtemp(prefix="cwbcode-app-ls-")
            for f in glob.glob(db + "*"): shutil.copy(f, copy)
            try:
                rows = sqlite3.connect(f"{copy}/localstorage.sqlite3").execute("select key, value from ItemTable").fetchall()
            finally:
                shutil.rmtree(copy, ignore_errors=True)
            return {k: (v.decode("utf-16-le") if isinstance(v, bytes) else v) for k, v in rows}
    return {}

def connections_to(port):
    out = subprocess.run(["lsof", "-nP", f"-iTCP@127.0.0.1:{port}", "-sTCP:ESTABLISHED"], capture_output=True, text=True).stdout
    return [line.split()[0] for line in out.splitlines()[1:]]

def start(scratch):
    env = {"HOME": os.environ["HOME"], "SHELL": "/bin/zsh", "PATH": "/usr/bin:/bin",
           "CWBCODE_HOME": f"{scratch}/home", "CWBCODE_MODELS": f"{scratch}/models", "OLLAMA_MODELS": f"{scratch}/none",
           "HF_HOME": f"{scratch}/none", "CWBCODE_APP_ARGS": "--no-model"}
    out = open(f"{scratch}/app.out", "a")
    app = subprocess.Popen([f"{BUNDLE}/Contents/MacOS/cwbcode-desktop"], env=env, stdout=out, stderr=subprocess.STDOUT)
    said = lambda: open(f"{scratch}/app.out").read()
    line = wait(lambda: re.search(r"the backend: 127\.0\.0\.1:(\d+) for the page, 127\.0\.0\.1:(\d+) for HTTPS", said().split("--- start")[-1]), "the backend's ports")
    page, https = int(line.group(1)), int(line.group(2))
    wait(lambda: f"the backend on 127.0.0.1:{page} showed it holds the token: opening the page" in said(), "the page to open")
    starting = re.findall(r"starting page: (\S+)", said())[-1]
    backend = wait(lambda: subprocess.run(["pgrep", "-f", f"cwbcode serve --http-port {page} "], capture_output=True, text=True).stdout.split(), "the backend's process")
    return app, page, https, starting, int(backend[0])

def quit(app, backend):
    app.send_signal(signal.SIGTERM)
    app.wait(30)
    wait(lambda: subprocess.run(["kill", "-0", str(backend)], capture_output=True).returncode != 0, "the backend to stop with the app", 20)

def main():
    assert os.path.isdir(BUNDLE), f"no app at {BUNDLE}: run make package"
    scratch = os.path.realpath(tempfile.mkdtemp(prefix="cwbcode-app-test-"))
    app = None
    try:
        seen = []
        for run in (1, 2):
            open(f"{scratch}/app.out", "a").write("--- start\n")
            print(f"{run}. the app, started as the Finder starts it")
            app, page, https, starting, backend = start(scratch)
            token = open(f"{scratch}/home/token").read().strip()
            assert starting == "tauri://localhost/desktop/index.html", starting
            # Its backend: on this machine only, on the ports the app chose.
            listening = subprocess.run(["lsof", "-nP", "-a", "-p", str(backend), "-iTCP", "-sTCP:LISTEN"], capture_output=True, text=True).stdout
            assert f"127.0.0.1:{page}" in listening and f"127.0.0.1:{https}" in listening and "*:" not in listening, listening
            # The page reaches it (the web view's own process holds connections to its port),
            # and keeps the token it was given once the backend took it.
            wait(lambda: any("WebKit" in c or c.startswith("com.apple") for c in connections_to(page)), "the page's connections to the backend")
            wait(lambda: page_storage().get("cwbcode-token") == token, "the page to keep the backend's token")
            stored = page_storage()
            assert "cwbcode-prefs" in stored, sorted(stored)
            seen.append((page, https, stored))
            print(f"   page {starting}, backend 127.0.0.1:{page} (HTTPS {https}): reached, token kept")
            quit(app, backend)
            app = None
            print("   quit: the backend stopped with it")
        (p1, h1, s1), (p2, h2, s2) = seen
        assert (p1, h1) != (p2, h2), "the second start got the same ports"
        assert s1["cwbcode-prefs"] == s2["cwbcode-prefs"], "the page's preferences did not stay"
        print(f"All passed: other ports the second time ({p1} -> {p2}), the same page origin, its storage kept.")
    finally:
        if app and app.poll() is None:
            app.kill()
        subprocess.run(["pkill", "-f", f"{scratch}/home"], capture_output=True)
        shutil.rmtree(scratch, ignore_errors=True)

if __name__ == "__main__":
    main()
