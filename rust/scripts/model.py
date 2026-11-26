#!/usr/bin/env python3
"""The model cwbcode runs: find it, say what and where it is, and download it.

    model.py status              where the model is and what it is (exit 1 when it is missing)
    model.py ensure              the same; when it is missing, download it the fastest way: when
                                 both sources can be used, each is measured for a few seconds
                                 and the faster is taken (and said)
    model.py download [--source ask|ollama|hf] [--force]
                                 download it; by default asks which source to use

Two sources give the same files:

  Ollama        `ollama pull` through Ollama's own API, into Ollama's store
                (~/.ollama/models, or $OLLAMA_MODELS).
  Hugging Face  the original checkpoint, thinkingmachines/Inkling-2-Mini, with the token in
                $HF_TOKEN (or $HUGGING_FACE_HUB_TOKEN, or the one `hf auth login` saved) when
                there is one and without one otherwise. It is written, as it arrives, in the
                layout of Ollama's store under ~/.cwbcode/models (or $CWBCODE_MODELS): one
                small safetensors file per tensor, named by its SHA-256. The bytes are the same
                as Ollama's; only the labels of the 4- and 8-bit expert weights differ, and they
                are written as Ollama writes them, so every tensor file is byte for byte the one
                in Ollama's registry, and is checked against it.

The backend looks in Ollama's store first and then in ~/.cwbcode/models. Only the Python
standard library is used.
"""

import argparse
import concurrent.futures
import hashlib
import json
import os
import shutil
import signal
import struct
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

MODEL = os.environ.get("CWBCODE_MODEL") or "inkling-2-mini:26b-a5b-nvfp4"
# The Hugging Face checkpoint of each model that can come from there, at a fixed revision: the
# conversion below is checked against that revision's files.
HF_SOURCES = {
    "inkling-2-mini:26b-a5b-nvfp4": ("thinkingmachines/Inkling-2-Mini", "c9d028302cd0f035fdf8e7e91f089a4ce94d2195"),
}
HF_JSON_FILES = ["config.json", "generation_config.json", "hf_quant_config.json", "processor_config.json",
                 "special_tokens_map.json", "tokenizer.json", "tokenizer_config.json"]
HF_DRAFT_FILE = "mtp.safetensors"
WORKERS = 8           # tensor files written at once
PIECES = 16           # connections fetching pieces of the large tensors
PIECE = 32 << 20      # bytes in a piece
USER_AGENT = "cwbcode-model-download/1"

TENSOR = "application/vnd.ollama.image.tensor"
JSON = "application/vnd.ollama.image.json"


def ollama_store() -> Path:
    return Path(os.environ.get("OLLAMA_MODELS") or Path.home() / ".ollama/models")


def own_store() -> Path:
    return Path(os.environ.get("CWBCODE_MODELS") or Path.home() / ".cwbcode/models")


def manifest_in(store: Path, model: str) -> Path:
    name, _, tag = model.partition(":")
    return store / "manifests/registry.ollama.ai/library" / name / (tag or "latest")


def find(model: str):
    """(store, manifest path, where it came from) of the first store holding the model."""
    for store, source in ((ollama_store(), "Ollama's store"), (own_store(), "cwbcode's store (from Hugging Face)")):
        path = manifest_in(store, model)
        if path.is_file():
            return store, path, source
    return None


def human(n: float) -> str:
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if abs(n) < 1000 or unit == "TB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1000
    return f"{n:.1f} TB"


def short(path: Path) -> str:
    home = str(Path.home())
    return "~" + str(path)[len(home):] if str(path).startswith(home + "/") else str(path)


# ---------- what the model is ----------

def describe(model: str) -> bool:
    found = find(model)
    if not found:
        print(f"model     {model}: not downloaded")
        print(f"          looked in {short(ollama_store())} and {short(own_store())}; run `make download`")
        return False
    store, path, source = found
    manifest = json.loads(path.read_text())
    layers = manifest.get("layers", [])
    files = {l.get("name"): store / "blobs" / l["digest"].replace(":", "-") for l in layers if l.get("name")}
    size = sum(l.get("size", 0) for l in layers)
    tensors = sum(1 for l in layers if l.get("mediaType") == TENSOR)
    missing = [n for n, p in files.items() if not p.is_file()]

    kind = "?"
    try:
        cfg = json.loads(files["config.json"].read_text())
        text = cfg.get("text_config", {})
        arch = (cfg.get("architectures") or ["?"])[0]
        experts, active = text.get("n_routed_experts"), text.get("num_experts_per_tok")
        quant = set()
        for name, p in files.items():
            if name.endswith("experts.w13_weight") and p.is_file():
                with open(p, "rb") as f:
                    n = struct.unpack("<Q", f.read(8))[0]
                    meta = json.loads(f.read(n)).get("__metadata__") or {}
                quant.add(meta.get("quant_type", "?").upper())
        draft = "draft/config.json" in files
        vision = any(n.startswith("model.visual.") for n in files)
        kind = (f"{arch}: {text.get('num_hidden_layers')} layers, {experts} experts ({active} per token), "
                f"experts in {' + '.join(sorted(quant)) or '?'}, the rest BF16/F32; safetensors for MLX"
                f"{'; vision' if vision else ''}{'; draft network for speculative decoding' if draft else ''}")
    except Exception as e:  # a store written by something else: say what can be said
        kind = f"(could not read its config: {e})"

    print(f"model     {model}")
    print(f"type      {kind}")
    print(f"size      {human(size)} in {tensors} tensor files")
    print(f"location  {short(store)}  ({source})")
    print(f"manifest  {short(path)}")
    if missing:
        print(f"broken    {len(missing)} of its files are missing (first: {missing[0]}); run `make download`")
        return False
    return True


# ---------- the progress bar ----------

class Progress:
    """One line, redrawn ten times a second: spinner, bar, percent, bytes, speed, time left."""

    SPIN = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏"
    PARTS = " ▏▎▍▌▋▊▉█"

    def __init__(self, label: str, total: int = 0, done: int = 0):
        self.label, self.total, self.done = label, total, done
        self.note = ""
        self.lock = threading.Lock()
        self.tty = sys.stdout.isatty()
        self.start = time.monotonic()
        self.samples = [(self.start, done)]
        self.initial = done
        self.stopped = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.frame = 0
        self.last_plain = 0.0

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *exc):
        self.stopped.set()
        self.thread.join()
        self.draw(final=True)
        print()

    def add(self, n: int):
        with self.lock:
            self.done += n

    def set(self, done: int, total: int | None = None, note: str | None = None):
        with self.lock:
            self.done = done
            if total is not None:
                self.total = total
            if note is not None:
                self.note = note

    def speed(self) -> float:
        # Bytes per second over the last five seconds.
        now = time.monotonic()
        with self.lock:
            self.samples.append((now, self.done))
            while len(self.samples) > 2 and now - self.samples[0][0] > 5:
                self.samples.pop(0)
            (t0, d0), (t1, d1) = self.samples[0], self.samples[-1]
        return (d1 - d0) / (t1 - t0) if t1 > t0 else 0.0

    def run(self):
        while not self.stopped.wait(0.1):
            self.draw()

    def draw(self, final: bool = False):
        speed = self.speed()
        with self.lock:
            done, total, note = self.done, self.total, self.note
        frac = min(done / total, 1.0) if total else 0.0
        left = (total - done) / speed if speed > 0 and total > done else 0
        eta = time.strftime("%H:%M:%S", time.gmtime(left)) if left else "--:--:--"
        if final:
            elapsed = time.monotonic() - self.start
            eta = "took " + time.strftime("%H:%M:%S", time.gmtime(elapsed))
            speed = (done - self.initial) / elapsed if elapsed > 0 else speed
        spin = "✔" if final and total and done >= total else self.SPIN[self.frame % len(self.SPIN)]
        self.frame += 1
        stats = f" {frac * 100:5.1f}%  {human(done)} / {human(total) if total else '?'}  {human(speed)}/s  {eta}"
        if note:
            stats += f"  {note}"
        if not self.tty:
            # Not a terminal (a log): a plain line every ten seconds.
            now = time.monotonic()
            if final or now - self.last_plain >= 10:
                self.last_plain = now
                print(f"{self.label}{stats}", flush=True)
            return
        cols = shutil.get_terminal_size((100, 20)).columns
        # A fixed width, narrower only when the terminal is.
        width = max(10, min(30, cols - len(self.label) - 62))
        cells = frac * width
        full = int(cells)
        bar = "█" * full
        if full < width:
            bar += self.PARTS[int((cells - full) * 8)] + " " * (width - full - 1)
        line = f"{spin} {self.label} \x1b[36m▕{bar}▏\x1b[0m{stats}"
        sys.stdout.write("\r\x1b[2K" + line)
        sys.stdout.flush()


# ---------- Ollama ----------

def ollama_host() -> str:
    host = os.environ.get("OLLAMA_HOST") or "127.0.0.1:11434"
    if "://" not in host:
        host = "http://" + host
    return host.replace("0.0.0.0", "127.0.0.1").rstrip("/")


def ollama_up() -> bool:
    try:
        with urllib.request.urlopen(ollama_host() + "/api/version", timeout=2) as r:
            return r.status == 200
    except Exception:
        return False


def registry_manifest(model: str):
    name, _, tag = model.partition(":")
    req = urllib.request.Request(f"https://registry.ollama.ai/v2/library/{name}/manifests/{tag or 'latest'}",
                                 headers={"Accept": "application/vnd.docker.distribution.manifest.v2+json", "User-Agent": USER_AGENT})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def pull_with_ollama(model: str):
    if not shutil.which("ollama") and not ollama_up():
        raise SystemExit("Ollama is not installed (https://ollama.com, or `brew install ollama`); use --source hf")
    server = None
    if not ollama_up():
        print("starting `ollama serve` for the download")
        server = subprocess.Popen(["ollama", "serve"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        for _ in range(100):
            if ollama_up():
                break
            time.sleep(0.2)
        else:
            server.terminate()
            raise SystemExit("`ollama serve` did not start")
    try:
        total = 0
        try:
            total = sum(l.get("size", 0) for l in registry_manifest(model)["layers"])
        except Exception:
            pass  # Ollama reports the sizes as it goes
        print(f"source    Ollama ({ollama_host()}), into {short(ollama_store())}")
        req = urllib.request.Request(ollama_host() + "/api/pull", data=json.dumps({"model": model, "stream": True}).encode(),
                                     headers={"Content-Type": "application/json"})
        sizes, got = {}, {}
        with Progress(f"pulling {model}", total) as bar, urllib.request.urlopen(req, timeout=600) as r:
            for raw in r:
                msg = json.loads(raw)
                if "error" in msg:
                    raise SystemExit(f"\nOllama: {msg['error']}")
                digest = msg.get("digest")
                if digest and msg.get("total"):
                    sizes[digest] = msg["total"]
                    got[digest] = msg.get("completed", 0)
                status = msg.get("status", "")
                note = "" if status.startswith("pulling ") and status != "pulling manifest" else status
                bar.set(sum(got.values()), max(total, sum(sizes.values())), note)
                if status == "success":
                    bar.set(max(total, sum(sizes.values())), note="")
    finally:
        if server:
            server.terminate()


# ---------- Hugging Face ----------

def hf_token():
    """(token, where it came from), or (None, None)."""
    for var in ("HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"):
        if os.environ.get(var, "").strip():
            return os.environ[var].strip(), f"${var}"
    home = Path(os.environ.get("HF_HOME") or Path.home() / ".cache/huggingface")
    for path in (Path(os.environ["HF_TOKEN_PATH"]) if os.environ.get("HF_TOKEN_PATH") else None, home / "token"):
        if path and path.is_file() and path.read_text().strip():
            return path.read_text().strip(), short(path)
    return None, None


class KeepAuthOnHub(urllib.request.HTTPRedirectHandler):
    """Follow redirects, but send the token only to huggingface.co, never to the CDN it redirects to."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        new = super().redirect_request(req, fp, code, msg, headers, newurl)
        if new is not None and urllib.parse.urlparse(newurl).hostname != "huggingface.co":
            new.remove_header("Authorization")
        return new


class Hub:
    def __init__(self, repo: str, revision: str, token: str | None):
        self.repo, self.revision, self.token = repo, revision, token
        self.opener = urllib.request.build_opener(KeepAuthOnHub)
        self.signed = {}  # file -> (url on the CDN, when it was given)
        self.lock = threading.Lock()

    def url(self, file: str) -> str:
        return f"https://huggingface.co/{self.repo}/resolve/{self.revision}/{urllib.parse.quote(file)}"

    def request(self, url: str, headers: dict | None = None, auth: bool = True):
        h = {"User-Agent": USER_AGENT, **(headers or {})}
        if auth and self.token:
            h["Authorization"] = f"Bearer {self.token}"
        return self.opener.open(urllib.request.Request(url, headers=h), timeout=60)

    def cdn(self, file: str, fresh: bool = False) -> str:
        """Where the file's bytes are: the signed CDN address huggingface.co redirects to, kept
        for twenty minutes (they are good for about an hour)."""
        with self.lock:
            hit = self.signed.get(file)
            if hit and not fresh and time.monotonic() - hit[1] < 1200:
                return hit[0]
        with self.request(self.url(file), {"Range": "bytes=0-0"}) as r:
            final = r.geturl()
        with self.lock:
            self.signed[file] = (final, time.monotonic())
        return final

    def get(self, file: str) -> bytes:
        with self.request(self.url(file)) as r:
            return r.read()

    def range(self, file: str, start: int, end: int, sink, tries: int = 6):
        """Bytes [start, end) of a file to sink(chunk), picking up where a broken connection left off."""
        pos, attempt = start, 0
        while pos < end:
            try:
                url = self.cdn(file, fresh=attempt > 0)
                with self.request(url, {"Range": f"bytes={pos}-{end - 1}"}, auth=urllib.parse.urlparse(url).hostname == "huggingface.co") as r:
                    if r.status != 206 and pos != 0:
                        raise IOError(f"the server ignored the range (HTTP {r.status})")
                    while pos < end:
                        chunk = r.read(min(1 << 20, end - pos))
                        if not chunk:
                            break
                        sink(chunk)
                        pos += len(chunk)
                if pos < end:
                    raise IOError("the connection closed early")
            except (urllib.error.URLError, IOError, TimeoutError, ConnectionError) as e:
                attempt += 1
                if attempt >= tries:
                    raise IOError(f"{file} bytes {pos}-{end}: {e}") from e
                time.sleep(min(2 ** attempt, 20))

    def header(self, file: str):
        """A safetensors file's header and where its data starts."""
        head = bytearray()
        self.range(file, 0, 8, head.extend)
        n = struct.unpack("<Q", head)[0]
        body = bytearray()
        self.range(file, 8, 8 + n, body.extend)
        return json.loads(body), 8 + n


def tensor_header(entries) -> bytes:
    """A safetensors header exactly as Ollama writes one: compact JSON, metadata first, padded
    with spaces to a multiple of eight."""
    raw = json.dumps(entries, separators=(",", ":")).encode()
    raw += b" " * (-len(raw) % 8)
    return struct.pack("<Q", len(raw)) + raw


def plan_tensors(hub: Hub):
    """Every tensor file of the model: (name, [(source file, start, end, tensor name, dtype, shape)], metadata)."""
    index = json.loads(hub.get("model.safetensors.index.json"))
    shards = sorted(set(index["weight_map"].values()))
    groups = []
    for shard in shards:
        head, base = hub.header(shard)
        head.pop("__metadata__", None)
        names = set(head)
        for name in sorted(head):
            if name.endswith("_scale") and name[: -len("_scale")] in names:
                continue  # goes with its weight
            t = head[name]
            parts = [(shard, base + t["data_offsets"][0], base + t["data_offsets"][1], name, t["dtype"], t["shape"])]
            meta = None
            scale = head.get(name + "_scale")
            if scale is not None:
                # A quantized expert bank: NVFP4 packed in bytes (U8) or MXFP8 (F8_E4M3), with
                # E4M3 or E8M0 scales. Ollama labels the packed weight as 32-bit words and the
                # scales as bytes; the bytes are the same.
                if t["dtype"] == "U8":
                    meta = {"group_size": "16", "quant_type": "nvfp4"}
                elif t["dtype"] == "F8_E4M3":
                    meta = {"group_size": "32", "quant_type": "mxfp8"}
                else:
                    raise SystemExit(f"{name}: unexpected quantized dtype {t['dtype']}")
                shape = t["shape"][:-1] + [t["shape"][-1] // 4]
                parts = [(shard, base + t["data_offsets"][0], base + t["data_offsets"][1], name, "U32", shape),
                         (shard, base + scale["data_offsets"][0], base + scale["data_offsets"][1], name + "_scale", "U8", scale["shape"])]
            groups.append((name, parts, meta))
    head, base = hub.header(HF_DRAFT_FILE)
    head.pop("__metadata__", None)
    for name in sorted(head):
        t = head[name]
        groups.append(("draft." + name, [(HF_DRAFT_FILE, base + t["data_offsets"][0], base + t["data_offsets"][1], "draft." + name, t["dtype"], t["shape"])], None))
    return groups


_piece_pool = None


def piece_pool():
    global _piece_pool
    if _piece_pool is None:
        _piece_pool = concurrent.futures.ThreadPoolExecutor(PIECES)
    return _piece_pool


def write_blob(blobs: Path, data_or_parts, hub: Hub | None = None, meta=None, progress: Progress | None = None):
    """Write one blob (bytes, or tensor parts fetched from the hub), named by its SHA-256.
    A large one is fetched in pieces over several connections at once, each written at its
    place in the file, and hashed when it is whole."""
    tmp = blobs / f".partial-{threading.get_ident()}-{time.monotonic_ns()}"
    if isinstance(data_or_parts, bytes):
        tmp.write_bytes(data_or_parts)
    else:
        entries, offset = ({"__metadata__": meta} if meta else {}), 0
        for _, start, end, name, dtype, shape in data_or_parts:
            entries[name] = {"dtype": dtype, "shape": shape, "data_offsets": [offset, offset + end - start]}
            offset += end - start
        head = tensor_header(entries)
        with open(tmp, "wb") as f:
            f.write(head)
            f.truncate(len(head) + offset)
        fd = os.open(tmp, os.O_WRONLY)
        try:
            jobs, at = [], len(head)
            for source, start, end, *_ in data_or_parts:
                for lo in range(start, end, PIECE):
                    hi = min(lo + PIECE, end)
                    jobs.append((source, lo, hi, at + lo - start))
                at += end - start

            def fetch(job):
                source, lo, hi, place = job
                where = place

                def put(chunk):
                    nonlocal where
                    os.pwrite(fd, chunk, where)
                    where += len(chunk)
                    if progress:
                        progress.add(len(chunk))
                hub.range(source, lo, hi, put)

            if len(jobs) == 1:
                fetch(jobs[0])
            else:
                for f in [piece_pool().submit(fetch, job) for job in jobs]:
                    f.result()
        finally:
            os.close(fd)
    sha = hashlib.sha256()
    with open(tmp, "rb") as f:
        while chunk := f.read(8 << 20):
            sha.update(chunk)
    digest = "sha256:" + sha.hexdigest()
    size = tmp.stat().st_size
    tmp.replace(blobs / digest.replace(":", "-"))
    return digest, size


def download_from_hf(model: str):
    if model not in HF_SOURCES:
        raise SystemExit(f"{model} has no Hugging Face source here; use --source ollama")
    repo, revision = HF_SOURCES[model]
    token, where = hf_token()
    store = own_store()
    blobs = store / "blobs"
    blobs.mkdir(parents=True, exist_ok=True)
    print(f"source    Hugging Face {repo} (revision {revision[:12]}), "
          f"{'with the token from ' + where if token else 'without a token (none found)'}")
    print(f"into      {short(store)}")
    hub = Hub(repo, revision, token)
    try:
        groups = plan_tensors(hub)
    except urllib.error.HTTPError as e:
        if e.code in (401, 403) and token:
            print(f"the token from {where} was refused (HTTP {e.code}); trying without it")
            hub = Hub(repo, revision, None)
            groups = plan_tensors(hub)
        else:
            raise SystemExit(f"Hugging Face: HTTP {e.code} for {repo}: {e.reason}")

    total = sum(sum(end - start for _, start, end, *_ in parts) for _, parts, _ in groups)
    need = total + (2 << 30)
    free = shutil.disk_usage(blobs).free
    if free < need:
        raise SystemExit(f"not enough disk space in {short(blobs)}: {human(need)} needed, {human(free)} free")

    # Picked up where an earlier download stopped: what is done is kept in this file.
    state_path = store / f".download-{model.replace(':', '-').replace('/', '-')}.json"
    state = json.loads(state_path.read_text()) if state_path.is_file() else {}
    if state.get("revision") != revision:
        state = {"revision": revision, "done": {}}
    done = state["done"]
    for name in list(done):
        if not (blobs / done[name]["digest"].replace(":", "-")).is_file():
            del done[name]
    lock = threading.Lock()

    def save():
        tmp = state_path.with_suffix(".tmp")
        tmp.write_text(json.dumps(state))
        tmp.replace(state_path)

    already = sum(sum(end - start for _, start, end, *_ in parts) for name, parts, _ in groups if name in done)
    todo = [g for g in groups if g[0] not in done]
    # The largest first, so the many small ones fill in at the end.
    todo.sort(key=lambda g: -sum(end - start for _, start, end, *_ in g[1]))
    if already:
        print(f"resuming  {len(groups) - len(todo)} of {len(groups)} tensor files are already here")

    def one(group):
        name, parts, meta = group
        digest, size = write_blob(blobs, parts, hub, meta, bar)
        with lock:
            done[name] = {"digest": digest, "size": size}
            save()

    with Progress(f"downloading {model}", total, already) as bar:
        bar.set(already, note=f"{len(done)}/{len(groups)} files")
        with concurrent.futures.ThreadPoolExecutor(WORKERS) as pool:
            futures = [pool.submit(one, g) for g in todo]
            try:
                for f in concurrent.futures.as_completed(futures):
                    f.result()
                    bar.set(bar.done, note=f"{len(done)}/{len(groups)} files")
            except BaseException:
                for f in futures:
                    f.cancel()
                raise

    # The config and tokenizer files, and the draft network's config (its part of config.json).
    layers = []
    for name, parts, meta in groups:
        layers.append({"mediaType": TENSOR, "digest": done[name]["digest"], "size": done[name]["size"], "name": name})
    config = None
    for file in HF_JSON_FILES:
        data = hub.get(file)
        if file == "config.json":
            config = json.loads(data)
        digest, size = write_blob(blobs, data)
        layers.append({"mediaType": JSON, "digest": digest, "size": size, "name": file})
    draft = dict(config["mtp_config"], architectures=["Inkling2ForConditionalGenerationMTP"])
    digest, size = write_blob(blobs, json.dumps(draft).encode())
    layers.append({"mediaType": JSON, "digest": digest, "size": size, "name": "draft/config.json"})
    image_config = json.dumps({"model_format": "safetensors", "capabilities": ["completion", "vision", "tools", "thinking"],
                               "draft": {"model_format": "safetensors", "architecture": "Inkling2ForConditionalGenerationMTP",
                                         "tensor_prefix": "draft.", "config": "draft/config.json"},
                               "source": f"huggingface.co/{repo}@{revision}"}).encode()
    digest, size = write_blob(blobs, image_config)
    manifest = {"schemaVersion": 2, "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
                "config": {"mediaType": "application/vnd.docker.container.image.v1+json", "digest": digest, "size": size},
                "layers": layers}

    # Every tensor file should be the one in Ollama's registry, byte for byte.
    try:
        theirs = {l["name"]: l["digest"] for l in registry_manifest(model)["layers"] if l.get("mediaType") == TENSOR}
        ours = {l["name"]: l["digest"] for l in layers if l["mediaType"] == TENSOR}
        same = sum(1 for n, d in ours.items() if theirs.get(n) == d)
        if same == len(ours) == len(theirs):
            print(f"verified  all {same} tensor files match Ollama's registry (SHA-256)")
        else:
            bad = [n for n, d in ours.items() if theirs.get(n) != d]
            for n in bad:
                done.pop(n, None)
            save()
            raise SystemExit(f"{len(bad)} of {len(ours)} tensor files do not match Ollama's registry "
                             f"(first: {bad[0] if bad else 'a missing one'}); run the download again to fetch them")
    except (urllib.error.URLError, TimeoutError, KeyError) as e:
        print(f"note      could not check against Ollama's registry ({e}); sizes are as Hugging Face gave them")

    path = manifest_in(store, model)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(manifest))
    state_path.unlink(missing_ok=True)
    for stale in blobs.glob(".partial-*"):
        stale.unlink(missing_ok=True)


# ---------- choosing ----------

def measure(url_for, probe_bytes: int, streams: int = 4, seconds: float = 4.0, headers=None) -> float:
    """Bytes per second from `streams` connections reading different parts of a large file for
    a few seconds: what a download from there would get now."""
    deadline = time.monotonic() + seconds
    got = [0] * streams

    def one(i):
        try:
            url = url_for()
            lo = (probe_bytes // streams) * i
            req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT, "Range": f"bytes={lo}-{lo + (256 << 20) - 1}", **(headers or {})})
            with urllib.request.urlopen(req, timeout=10) as r:
                while time.monotonic() < deadline:
                    chunk = r.read(1 << 16)
                    if not chunk:
                        break
                    got[i] += len(chunk)
        except Exception:
            pass

    start = time.monotonic()
    threads = [threading.Thread(target=one, args=(i,)) for i in range(streams)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    return sum(got) / max(time.monotonic() - start, 1e-6)


def measure_ollama(model: str) -> float:
    name, _, _ = model.partition(":")
    biggest = max(registry_manifest(model)["layers"], key=lambda l: l.get("size", 0))
    url = f"https://registry.ollama.ai/v2/library/{name}/blobs/{biggest['digest']}"
    return measure(lambda: url, biggest["size"])


def measure_hf(model: str) -> float:
    repo, revision = HF_SOURCES[model]
    token, _ = hf_token()
    hub = Hub(repo, revision, token)
    file = "model-00001-of-00001.safetensors"
    size = int(json.loads(hub.get("model.safetensors.index.json"))["metadata"]["total_size"])
    return measure(lambda: hub.cdn(file), size)


def choose_source(model: str):
    """The fastest way to get the model from here, and the measurements, as (source, speeds)."""
    has_ollama = bool(shutil.which("ollama")) or ollama_up()
    can_hf = model in HF_SOURCES
    if not can_hf:
        return "ollama", {}
    if not has_ollama:
        return "hf", {}
    print("measuring both sources for a few seconds, to take the faster ...", flush=True)
    speeds = {}
    for source, fn in (("ollama", measure_ollama), ("hf", measure_hf)):
        try:
            speeds[source] = fn(model)
        except Exception:
            speeds[source] = 0.0
    best = max(speeds, key=speeds.get)
    return best, speeds


NAMES = {"ollama": "Ollama", "hf": "Hugging Face"}


def tell_choice(source: str, speeds: dict):
    token, where = hf_token()
    how = {"ollama": f"Ollama, into {short(ollama_store())}",
           "hf": f"Hugging Face, {'with the token from ' + where if token else 'without a token (none found; none needed)'}, into {short(own_store())}"}[source]
    if speeds:
        measured = ", ".join(f"{NAMES[k]} {human(v)}/s" for k, v in speeds.items())
        print(f"fastest   {how}  (measured: {measured})")
    elif source == "hf":
        print(f"using     {how}  (Ollama is not installed)")
    else:
        print(f"using     {how}  (this model has no Hugging Face source here)")


def ask_source(model: str) -> str:
    has_ollama = bool(shutil.which("ollama")) or ollama_up()
    token, where = hf_token()
    best, speeds = choose_source(model)
    speed = lambda k: f"  measured {human(speeds[k])}/s" if k in speeds else ""
    print(f"Where should {model} (about 20 GB) come from?")
    print(f"  1) Ollama        {'installed' if has_ollama else 'not installed'}; into {short(ollama_store())}{speed('ollama')}")
    print(f"  2) Hugging Face  {HF_SOURCES[model][0] if model in HF_SOURCES else 'not available for this model'}; "
          f"{'token from ' + where if token else 'no token found, none needed'}; into {short(own_store())}{speed('hf')}")
    default = "1" if best == "ollama" else "2"
    if speeds:
        print(f"  {NAMES[best]} is faster from here right now.")
    if not sys.stdin.isatty():
        print(f"(not a terminal: taking {default})")
        return "ollama" if default == "1" else "hf"
    while True:
        try:
            answer = input(f"choose 1 or 2 [{default}]: ").strip() or default
        except EOFError:
            answer = default
        if answer in ("1", "ollama"):
            return "ollama"
        if answer in ("2", "hf", "huggingface"):
            return "hf"
        print("1 or 2, please")


def download(model: str, source: str):
    if source == "ask":
        source = ask_source(model)
    if source == "ollama":
        pull_with_ollama(model)
    else:
        download_from_hf(model)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("command", choices=["status", "ensure", "download"])
    parser.add_argument("--model", default=MODEL)
    parser.add_argument("--source", choices=["ask", "ollama", "hf"], default=None)
    parser.add_argument("--force", action="store_true", help="download even when the model is already there")
    args = parser.parse_args()
    signal.signal(signal.SIGINT, lambda *_: (print("\nstopped; run it again to pick up where it left off"), os._exit(130)))

    if args.command == "status":
        sys.exit(0 if describe(args.model) else 1)
    found = find(args.model)
    if found and not args.force:
        print(f"found     {args.model} in {found[2]}: using it, nothing to download"
              + (" (--force to download again)" if args.command == "download" else ""))
        if describe(args.model):
            return
    if args.command == "ensure":
        print(f"{args.model} is not downloaded yet: downloading it now (about 20 GB)")
        if args.source:
            source = args.source
        else:
            source, speeds = choose_source(args.model)
            tell_choice(source, speeds)
    else:
        source = args.source or "ask"
    download(args.model, source)
    if not describe(args.model):
        sys.exit(1)


if __name__ == "__main__":
    main()
