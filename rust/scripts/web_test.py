#!/usr/bin/env python3
"""The web client, tested in a real browser: starts the backend with a scripted model in a
scratch home folder, drives headless Chrome over its DevTools protocol through the page
(sign in with a wallet's recovery phrase, open a project, chat, approve an edit, dashboard,
knowledge base, tools, sign out; then a backend in local mode, opened with the one-time link, and
turning its remote mode on and off from the dashboard; a backend without the model; and one
that downloads it from a fake Hugging Face, watched as the Forge to Ready on two pages), and
checks what the page shows and what happened on disk. Screenshots go to the folder given
as the first argument (default: a temporary one).

Needs Google Chrome and the Python package `websockets`. Run `make build` first."""
import base64, hashlib, json, os, shutil, socket, ssl, subprocess, sys, tempfile, time, urllib.request
from websockets.sync.client import connect

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
SCRIPT = [
    {"thinking": "I will write the file the user asked for.", "calls": [
        {"name": "todo_write", "args": {"todos": [{"content": "write hello.txt", "status": "in_progress"}, {"content": "tell the user", "status": "pending"}]}},
        {"name": "write_file", "args": {"path": "hello.txt", "content": "hello from the web\n"}}]},
    {"text": "Done. I wrote `hello.txt`:\n```\nhello from the web\n```\nAnything else?"},
    {"text": "A second answer."},
]

# The well-known test wallet: this phrase's first account is this address.
PHRASE = "test test test test test test test test test test test junk"
WALLET = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
OTHER_PHRASE = "legal winner thank year wave sausage worth useful legal winner thank yellow"

REPO_PATH = "/thinkingmachines/Inkling-2-Mini/resolve/c9d028302cd0f035fdf8e7e91f089a4ce94d2195/"

def safetensors(tensors):
    """A safetensors file: a JSON header, then the tensors' bytes."""
    head, data = {"__metadata__": {"format": "pt"}}, b""
    for name, dtype, shape, raw in tensors:
        head[name] = {"dtype": dtype, "shape": shape, "data_offsets": [len(data), len(data) + len(raw)]}
        data += raw
    text = json.dumps(head).encode()
    return len(text).to_bytes(8, "little") + text + data

class FakeHub:
    """A Hugging Face of the test's own (as rust/client/tests/backend.rs has one): a small
    checkpoint whose large tensor is sent at `rate` bytes a second, with ranges; Ollama's
    registry answers 404 after `registry_delay` seconds (the check is then skipped), which
    holds the backend in "verifying" long enough to see."""
    def __init__(self, rate, registry_delay):
        import http.server, threading
        big = bytes((i * 2654435761 >> 13) & 0xff for i in range(6 << 20))
        files = {
            "model.safetensors.index.json": json.dumps({"metadata": {"total_size": 1}, "weight_map": {"model.llm.big.weight": "model-00001-of-00001.safetensors"}}).encode(),
            "model-00001-of-00001.safetensors": safetensors([("model.llm.big.weight", "BF16", [1536, 2048], big), ("model.llm.norm.weight", "BF16", [8], bytes(16))]),
            "mtp.safetensors": safetensors([("fc.weight", "BF16", [4], bytes(range(8)))]),
            "config.json": json.dumps({"architectures": ["FakeForTest"], "mtp_config": {"hidden_size": 4}}).encode(),
        }
        for name in ["generation_config.json", "hf_quant_config.json", "processor_config.json", "special_tokens_map.json", "tokenizer.json", "tokenizer_config.json"]:
            files[name] = json.dumps({"file": name}).encode()
        outer = self
        self.seen_resolve = 0
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def do_GET(self):
                if self.path.startswith("/v2/"):
                    time.sleep(registry_delay)
                    self.send_response(404); self.send_header("Content-Length", "0"); self.end_headers(); return
                name = self.path[len(REPO_PATH):] if self.path.startswith(REPO_PATH) else None
                if name not in files:
                    self.send_response(404); self.send_header("Content-Length", "0"); self.end_headers(); return
                outer.seen_resolve += 1
                data, status = files[name], 200
                wanted = self.headers.get("Range", "")
                if wanted.startswith("bytes="):
                    a, b = wanted[6:].split("-")
                    a, b = int(a), min(int(b), len(data) - 1)
                    data, status = data[a:b + 1], 206
                self.send_response(status)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                try:
                    for i in range(0, len(data), 64 << 10):
                        self.wfile.write(data[i:i + (64 << 10)])
                        if len(data) > 1 << 20: time.sleep((64 << 10) / rate)
                except OSError: pass
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

def free_port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); port = s.getsockname()[1]; s.close(); return port

class Page:
    def __init__(self, ws):
        self.ws, self.n, self.errors = ws, 0, []
    def send(self, method, **params):
        self.n += 1
        self.ws.send(json.dumps({"id": self.n, "method": method, "params": params}))
        while True:
            message = json.loads(self.ws.recv())
            if message.get("method") == "Runtime.exceptionThrown":
                self.errors.append(message["params"]["exceptionDetails"].get("exception", {}).get("description", "exception"))
            if message.get("method") == "Log.entryAdded" and message["params"]["entry"]["level"] == "error":
                self.errors.append(message["params"]["entry"]["text"])
            if message.get("id") == self.n:
                if "error" in message: raise RuntimeError(message["error"])
                return message.get("result", {})
    def js(self, expression):
        result = self.send("Runtime.evaluate", expression=expression, returnByValue=True, awaitPromise=True)
        if "exceptionDetails" in result: raise RuntimeError(result["exceptionDetails"].get("exception", {}).get("description", str(result)))
        return result["result"].get("value")
    def wait(self, expression, what, seconds=15):
        end = time.time() + seconds
        while time.time() < end:
            if self.js(expression): return
            time.sleep(0.1)
        raise AssertionError(f"timed out waiting for {what}; the page shows:\n{self.js('document.body.innerText')[:1500]}")
    def type(self, selector, text):
        # As a person would: set the value and let the page hear about it.
        self.js(f"""(() => {{ const el = document.querySelector({json.dumps(selector)}); el.focus();
            const proto = el.tagName === 'TEXTAREA' ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
            Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, {json.dumps(text)});
            el.dispatchEvent(new Event('input', {{bubbles: true}})); }})()""")
    def click(self, selector):
        self.js(f"document.querySelector({json.dumps(selector)}).click()")
    def mouse(self, selector):
        """A real click: the mouse pressed and released on the middle of the element, as a
        person's is (not the element's own click(), which skips what a pointer goes through)."""
        box = json.loads(self.js(f"(() => {{ const e = document.querySelector({json.dumps(selector)}); if (!e) return 'null'; e.scrollIntoView({{block: 'center'}}); const r = e.getBoundingClientRect(); return JSON.stringify([r.left + r.width / 2, r.top + r.height / 2, r.width, r.height]); }})()"))
        assert box and box[2] > 0 and box[3] > 0, f"nothing to click at {selector}: {box}"
        hit = self.js(f"(() => {{ const e = document.elementFromPoint({box[0]}, {box[1]}); return !!e && !!e.closest({json.dumps(selector)}); }})()")
        assert hit, f"{selector} is covered by something else where it would be clicked"
        for kind in ("mousePressed", "mouseReleased"):
            self.send("Input.dispatchMouseEvent", type=kind, x=box[0], y=box[1], button="left", clickCount=1)
    def click_text(self, selector, text):
        self.js(f"[...document.querySelectorAll({json.dumps(selector)})].find(e => e.textContent.trim() === {json.dumps(text)}).click()")
    def text(self, selector):
        return self.js(f"(document.querySelector({json.dumps(selector)}) || {{}}).innerText || ''")
    def shot(self, path, settle=0.8):
        time.sleep(settle)  # let the entrance movement finish
        with open(path, "wb") as f: f.write(base64.b64decode(self.send("Page.captureScreenshot", format="png")["data"]))

def main():
    shots = sys.argv[1] if len(sys.argv) > 1 else tempfile.mkdtemp(prefix="cwbcode-web-shots-")
    os.makedirs(shots, exist_ok=True)
    home = os.path.realpath(tempfile.mkdtemp(prefix="cwbcode-web-home-"))
    os.makedirs(f"{home}/proj"); open(f"{home}/proj/README.md", "w").write("# proj\n")
    json.dump(SCRIPT, open(f"{home}/script.json", "w"))
    port, debug = free_port(), free_port()
    env = dict(os.environ, HOME=home, CWBCODE_HOME=f"{home}/.cwbcode")
    env.pop("CWBCODE_WALLET", None)
    # The backend under test belongs to a wallet; a second one, in local mode, comes last.
    server = subprocess.Popen([f"{ROOT}/target/release/cwbcode", "serve", "--bind", f"127.0.0.1:{port}", "--script", f"{home}/script.json", "--env-file", "/dev/null"], env=dict(env, CWBCODE_WALLET=WALLET), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    local = None
    chrome = subprocess.Popen([CHROME, "--headless=new", f"--remote-debugging-port={debug}", "--ignore-certificate-errors", f"--user-data-dir={home}/chrome", "--window-size=1440,900", "--no-first-run", "--remote-allow-origins=*", "about:blank"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(100):
            try:
                tabs = json.load(urllib.request.urlopen(f"http://127.0.0.1:{debug}/json")); break
            except Exception: time.sleep(0.1)
        token = None
        for _ in range(100):
            if os.path.exists(f"{home}/.cwbcode/token"): token = open(f"{home}/.cwbcode/token").read().strip(); break
            time.sleep(0.1)
        assert token, "the backend did not start"
        page = Page(connect([t for t in tabs if t["type"] == "page"][0]["webSocketDebuggerUrl"], max_size=50_000_000))
        for domain in ("Runtime", "Log", "Page"): page.send(f"{domain}.enable")
        # A first visit follows the system's light or dark; the checks below start from dark,
        # whatever the Mac is set to (it changes with the time of day when set to Auto).
        page.send("Emulation.setEmulatedMedia", features=[{"name": "prefers-color-scheme", "value": "dark"}])
        url = f"https://127.0.0.1:{port}"

        print("1. the page asks for the wallet's recovery phrase; only the right one signs in")
        page.send("Page.navigate", url=url)
        page.wait("!!document.querySelector('#phrase')", "the sign-in page")
        page.js("(() => { const s = document.querySelector('#lang'); s.value = 'en'; s.dispatchEvent(new Event('change', {bubbles: true})); })()")
        assert page.js("typeof Motion === 'object' && typeof Motion.animate === 'function'"), "Motion did not load"
        assert page.text("#backend-wallet").lower() == WALLET.lower(), page.text("#backend-wallet")
        page.shot(f"{shots}/1-signin.png")
        page.type("#phrase", "these are not the words"); page.click("#signin")
        page.wait("(document.querySelector('#login-problem') || {}).innerText?.includes('not a valid recovery phrase')", "the refusal of words that are no phrase")
        page.type("#phrase", OTHER_PHRASE); page.click("#signin")
        page.wait("(document.querySelector('#login-problem') || {}).innerText?.includes('belongs to')", "the refusal of another wallet's phrase")
        # The right phrase, but its second account: still another wallet.
        page.type("#phrase", PHRASE); page.type("#phrase-index", "1"); page.click("#signin")
        page.wait("(document.querySelector('#login-problem') || {}).innerText?.includes('account 1')", "the refusal of the phrase's other account")
        assert page.js("!document.querySelector('.side')"), "a wrong phrase signed in"
        page.type("#phrase-index", "0"); page.click("#signin")
        page.wait("!!document.querySelector('.side')", "the app after signing in")
        page.wait("document.querySelector('#status-pill').innerText.includes('scripted')", "the backend's status")
        assert "wallet 0xf39F" in page.text("#login-state"), page.text("#login-state")
        # The browser keeps the session ("stay signed in" is ticked), and never the phrase:
        # nothing in its storage holds any word of it.
        assert page.js("/^[0-9a-f]{64}$/.test(localStorage.getItem('cwbcode-token') || '')"), "no session was stored"
        stored = page.js("JSON.stringify(Object.assign({}, localStorage, sessionStorage))")
        assert "junk" not in stored and "cwbcode-phrase" not in stored, f"the phrase was stored: {stored[:300]}"
        # A refresh comes straight back in; a session that has ended asks for the phrase again.
        page.send("Page.navigate", url=url)
        page.wait("!!document.querySelector('.side')", "the app after a refresh")
        page.js("localStorage.setItem('cwbcode-token', 'an-ended-session')")
        page.send("Page.navigate", url=url)
        page.wait("!!document.querySelector('#phrase') && !localStorage.getItem('cwbcode-token')", "the sign-in page for an ended session")
        page.errors.clear()  # (the ended session was refused once, as it should be)
        # A phrase an earlier version left in storage is deleted when the page loads.
        page.js(f"localStorage.setItem('cwbcode-phrase', {json.dumps(json.dumps({'phrase': PHRASE, 'index': 0}))})")
        page.send("Page.navigate", url=url)
        page.wait("!!document.querySelector('#phrase') && localStorage.getItem('cwbcode-phrase') === null", "the old phrase deleted")
        page.type("#phrase", PHRASE); page.click("#signin")
        page.wait("!!document.querySelector('.side')", "the app after signing in again")

        # A progressive web app: a manifest, and a service worker in charge of the page.
        assert page.js("document.querySelector('link[rel=manifest]').getAttribute('href')") == "/manifest.webmanifest"
        page.js("window.__sw = null; navigator.serviceWorker.ready.then(r => window.__sw = r.active ? r.active.scriptURL : 'none', () => window.__sw = 'failed')")
        page.wait("window.__sw !== null", "the service worker")
        assert page.js("window.__sw").endswith("/sw.js"), page.js("window.__sw")
        print("2. a folder outside home is refused, one under it opens")
        page.type("#open-path", "/etc"); page.click("#open-go")
        page.wait("(document.querySelector('#toast') || {}).innerText?.includes('home folder')", "the refusal of /etc")
        # A folder that does not exist is made, with the folders above it.
        page.type("#open-path", "~/made/by/opening"); page.click("#open-go")
        page.wait("(document.querySelector('#notice') || {}).innerText?.includes('made/by/opening')", "the notice that the folder was created")
        assert os.path.isdir(f"{home}/made/by/opening"), "the folder was not created"
        page.type("#open-path", "~/proj"); page.click("#open-go")
        page.wait("document.querySelector('.row.project.on')?.innerText.includes('proj')", "the opened project")
        assert os.path.exists(f"{home}/proj/.cwbcode/cwbcode.db"), "the project has no store"
        # A bare name is a folder in the backend's work folder; paths are shown with ~.
        assert page.text(".open .prefix") == "work/"
        page.type("#open-path", "plain/name"); page.click("#open-go")
        page.wait("document.querySelector('.row.project.on')?.innerText.includes('name')", "a folder typed without ~/")
        assert os.path.isdir(f"{home}/.cwbcode/work/plain/name"), "a bare name was not taken as a folder in the work folder"
        page.type("#open-path", f"{os.path.realpath(home)}/proj"); page.click("#open-go")
        page.wait("document.querySelector('.row.project.on .what')?.innerText.startsWith('proj')", "the project typed as a whole path")
        shown = page.js("JSON.stringify([...document.querySelectorAll('.row.project small')].map(e => e.innerText))")
        assert "~/proj" in json.loads(shown) and all(path.startswith("~/") for path in json.loads(shown)), shown
        assert os.path.basename(home) not in page.js("document.body.innerText"), "the home folder's own path is on the page"
        # A project is taken off the list; its folder and its records stay, and it opens again.
        page.js("[...document.querySelectorAll('.row.project')].find(r => r.innerText.includes('name')).querySelector('.remove').click()")
        page.wait("![...document.querySelectorAll('.row.project')].some(r => r.innerText.includes('plain/name'))", "the project off the list")
        page.wait("(document.querySelector('#notice') || {}).innerText?.includes('~/.cwbcode/work/plain/name')", "the notice that it was removed")
        assert os.path.exists(f"{home}/.cwbcode/work/plain/name/.cwbcode/cwbcode.db"), "removing a project from the list deleted its records"
        page.type("#open-path", "~/proj"); page.click("#open-go")
        page.wait("document.querySelector('.row.project.on .what')?.innerText.startsWith('proj')", "the project again")
        # The frame is as tall as what the browser shows, and nothing is wider than it.
        assert page.js("document.querySelector('.app').getBoundingClientRect().bottom <= innerHeight + 1"), "the bottom edge is below the window"

        print("2b. light and dark, side by side and stacked, eight languages; all kept across a refresh")
        assert page.js("document.documentElement.dataset.theme") == "dark"
        page.click("#theme-toggle")
        page.wait("document.documentElement.dataset.theme === 'light'", "the light theme")
        page.wait("getComputedStyle(document.body).color === 'rgb(20, 28, 72)'", "dark text on the light theme")
        page.click("#layout-toggle")
        page.wait("document.querySelector('.app').classList.contains('vertical')", "the stacked layout")
        assert page.js("document.querySelector('.side').getBoundingClientRect().width > document.querySelector('.side').getBoundingClientRect().height"), "the sidebar did not become a header"
        seen = {}
        for code, chat in [("ko", "채팅"), ("cs", "Chat"), ("ja", "チャット"), ("yue", "對話"), ("zh", "对话"), ("es", "Chat"), ("tr", "Sohbet")]:
            page.js(f"(() => {{ const s = document.querySelector('#lang'); s.value = '{code}'; s.dispatchEvent(new Event('change', {{bubbles: true}})); }})()")
            page.wait(f"document.querySelector('#nav-chat').innerText.trim() === {json.dumps(chat)} && document.documentElement.lang === '{code}'", f"the interface in {code}")
            seen[code] = page.text("#nav-knowledge")
        assert len(set(seen.values())) >= 6, seen
        # The sidebar folds away, and the view takes the whole window.
        wide = page.js("document.querySelector('.main').getBoundingClientRect().height")
        page.click("#side-toggle")
        page.wait("getComputedStyle(document.querySelector('.side')).display === 'none'", "the sidebar folded away")
        assert page.js("document.querySelector('.main').getBoundingClientRect().height") > wide, "the view did not grow"
        assert json.loads(page.js("localStorage.getItem('cwbcode-prefs')"))["side"] is False
        page.click("#side-toggle")
        page.wait("getComputedStyle(document.querySelector('.side')).display !== 'none'", "the sidebar back")
        # Buttons are rounded.
        assert page.js("parseFloat(getComputedStyle(document.querySelector('#open-go')).borderRadius)") >= 6
        # The typeface and the size: each changes what is drawn, and both are kept.
        assert (page.js("document.documentElement.dataset.font"), page.js("document.documentElement.dataset.size")) == ("pixel", "m")
        assert "VT323" in page.js("getComputedStyle(document.body).fontFamily")
        chat_face = "getComputedStyle(document.querySelector('#draft')).fontFamily.split(',')[0].replaceAll('\"', '').trim()"
        # Every typeface on the list is in the program, loads, and is the one then drawn.
        codes = json.loads(page.js("JSON.stringify([...document.querySelectorAll('#font option')].map(o => o.value))"))
        assert len(codes) == 55 and codes[0] == "pixel" and codes[-1] == "system", codes
        faces = {}
        for font in codes[1:] + ["pixel"]:
            page.js(f"(() => {{ const s = document.querySelector('#font'); s.value = '{font}'; s.dispatchEvent(new Event('change', {{bubbles: true}})); }})()")
            page.wait(f"document.documentElement.dataset.font === '{font}'", f"the {font} typeface")
            faces[font] = page.js(chat_face)
            # The whole page is set in the face chosen: the interface and the conversation.
            if font != "system":
                assert all(faces[font] in page.js(f"getComputedStyle(document.querySelector('{part}')).fontFamily") for part in ["#nav-chat", "#send", "body"]), font
            if font != "system":
                page.js(f"window.__face = null; document.fonts.load('16px \"{faces[font]}\"', 'Az').then(f => window.__face = f.length, () => window.__face = -1)")
                page.wait("window.__face !== null", f"the {font} typeface to load")
                assert page.js("window.__face") >= 1, f"the {font} typeface ({faces[font]}) is not in the program"
        assert len(set(faces.values())) == len(codes), faces
        assert page.js("document.fonts.check('16px VT323') && document.fonts.check('10px PressStart2P')"), "the pixel faces did not load"
        page.js("(() => { const s = document.querySelector('#font'); s.value = 'mono'; s.dispatchEvent(new Event('change', {bubbles: true})); })()")
        # The size is the conversation's alone: what is typed and said grows, the interface does not.
        said = "parseFloat(getComputedStyle(document.querySelector('#draft')).fontSize)"
        interface = "['#nav-chat', '#send', '#open-go'].map(q => getComputedStyle(document.querySelector(q)).fontSize).join()"
        sizes, around = [page.js(said)], page.js(interface)
        # Seven steps: two down from the middle one, four up.
        for size in ["s", "xs"]:
            page.click("#font-smaller"); page.wait(f"document.documentElement.dataset.size === '{size}'", f"size {size}")
            sizes.insert(0, page.js(said))
        assert page.js("document.querySelector('#font-smaller').disabled"), "there is nothing smaller than the smallest"
        for size in ["s", "m", "l", "xl", "2xl", "3xl"]:
            page.click("#font-larger"); page.wait(f"document.documentElement.dataset.size === '{size}'", f"size {size}")
            if size not in ("s", "m"):
                sizes.append(page.js(said))
            assert page.js(interface) == around, f"the interface changed size with the messages ({size})"
        assert sizes == sorted(sizes) and len(set(sizes)) == 7 and page.js("document.querySelector('#font-larger').disabled"), sizes
        assert page.js("document.documentElement.scrollWidth <= innerWidth"), "the largest text makes the page scroll sideways"
        page.shot(f"{shots}/2b-light-stacked-turkish.png")
        page.js("(() => { const s = document.querySelector('#lang'); s.value = 'ko'; s.dispatchEvent(new Event('change', {bubbles: true})); })()")
        page.click("#nav-knowledge")
        page.wait("!!document.querySelector('#kb-note')", "the knowledge view")
        prefs = json.loads(page.js("localStorage.getItem('cwbcode-prefs')"))
        assert (prefs["theme"], prefs["layout"], prefs["lang"], prefs["view"]) == ("light", "vertical", "ko", "knowledge") and prefs["project"].endswith("/proj"), prefs
        assert (prefs["font"], prefs["size"]) == ("mono", "3xl"), prefs
        page.send("Page.navigate", url=url)
        page.wait("!!document.querySelector('#kb-note') && document.querySelector('.app').classList.contains('vertical')", "the same view and layout after a refresh")
        assert page.js("document.documentElement.dataset.theme") == "light" and page.text("#nav-chat") == "채팅"
        assert (page.js("document.documentElement.dataset.font"), page.js("document.documentElement.dataset.size")) == ("mono", "3xl"), "the typeface and size were not kept"
        page.js("(() => { const s = document.querySelector('#font'); s.value = 'pixel'; s.dispatchEvent(new Event('change', {bubbles: true})); })()")
        for _ in range(4): page.click("#font-smaller")  # 3xl, 2xl, xl, l, m
        page.wait("document.documentElement.dataset.size === 'm' && document.documentElement.dataset.font === 'pixel'", "the usual typeface and size")
        page.wait("document.querySelector('.row.project.on')?.innerText.includes('proj')", "the same project after a refresh")
        page.wait("getComputedStyle(document.body).color === 'rgb(20, 28, 72)'", "the light theme's colours after a refresh")
        # Back to English, dark, side by side for the rest.
        page.js("(() => { const s = document.querySelector('#lang'); s.value = 'en'; s.dispatchEvent(new Event('change', {bubbles: true})); })()")
        page.click("#theme-toggle"); page.wait("document.documentElement.dataset.theme === 'dark'", "the dark theme")
        page.click("#layout-toggle"); page.wait("!document.querySelector('.app').classList.contains('vertical')", "side by side")
        page.click("#nav-chat")
        page.wait("!!document.querySelector('#draft')", "the chat view")

        # The settings sheet closes with Escape, and with a click outside it.
        page.click("#settings")
        page.wait("document.querySelector('#settings-sheet').classList.contains('open')", "the settings sheet")
        page.js("window.dispatchEvent(new KeyboardEvent('keydown', {key: 'Escape', bubbles: true}))")
        page.wait("!document.querySelector('#settings-sheet').classList.contains('open')", "the sheet closed by Escape")
        page.click("#settings")
        page.wait("document.querySelector('#settings-sheet').classList.contains('open')", "the settings sheet again")
        page.js("document.querySelector('#settings-sheet select').click()")
        assert page.js("document.querySelector('#settings-sheet').classList.contains('open')"), "a click inside closed the sheet"
        page.js("document.querySelector('.main').click()")
        page.wait("!document.querySelector('#settings-sheet').classList.contains('open')", "the sheet closed by a click outside")
        # Enter on a project's × is the ×'s (a button's own Enter clicks it), not the row's.
        before = page.js("JSON.stringify([document.querySelector('.row.project.on .what').innerText, document.querySelector('#nav .on').id])")
        other = page.js("document.querySelector('.row.project:not(.on) .what')?.innerText || ''")
        assert other and page.js("!!document.querySelector('.row.project:not(.on) .remove')"), "there is no other project with a × to try Enter on"
        page.js("document.querySelector('.row.project:not(.on) .remove').dispatchEvent(new KeyboardEvent('keydown', {key: 'Enter', bubbles: true}))")
        time.sleep(0.5)
        assert page.js("JSON.stringify([document.querySelector('.row.project.on .what').innerText, document.querySelector('#nav .on').id])") == before, "Enter on × opened the project"
        # And a click on the × still takes that project off the list.
        page.click(".row.project:not(.on) .remove")
        page.wait(f"![...document.querySelectorAll('.row.project .what')].some(e => e.innerText === {json.dumps(other)})", "the project off the list after a click on its ×")
        assert page.js("document.querySelector('.row.project.on .what').innerText") == json.loads(before)[0], "removing another project changed the one open"
        # Every view's button is named, also where its word is hidden.
        assert page.js("[...document.querySelectorAll('#nav button')].every(b => b.getAttribute('aria-label'))"), "a view's button has no name"

        print("3. a message streams, an edit waits for approval, and Allow lets it through (on a phone)")
        # A phone: 390 by 844, touch. The sidebar becomes a drawer, closed.
        page.send("Emulation.setDeviceMetricsOverride", width=390, height=844, deviceScaleFactor=3, mobile=True)
        page.send("Emulation.setTouchEmulationEnabled", enabled=True, maxTouchPoints=5)
        page.send("Emulation.setEmulatedMedia", features=[{"name": "prefers-color-scheme", "value": "dark"}, {"name": "pointer", "value": "coarse"}, {"name": "hover", "value": "none"}])
        page.wait("innerWidth === 390 && getComputedStyle(document.querySelector('.side')).display === 'none'", "the drawer closed on a phone")
        # Nothing is wider than the screen (the page would be zoomed out to fit it).
        wide = page.js("JSON.stringify([...document.querySelectorAll('body *')].filter(e => e.getBoundingClientRect().width > 0 && e.getBoundingClientRect().right > innerWidth + 1).map(e => e.id || e.className).slice(0, 5))")
        assert wide == "[]", f"wider than the phone: {wide}"
        # The drawer opens over the view, and a choice in it closes it.
        page.click("#side-toggle")
        page.wait("getComputedStyle(document.querySelector('.side')).position === 'absolute' && getComputedStyle(document.querySelector('.side')).display !== 'none'", "the drawer open")
        # Only the height changes (the keyboard or the browser's bars come and go): it stays open.
        page.send("Emulation.setDeviceMetricsOverride", width=390, height=600, deviceScaleFactor=3, mobile=True)
        page.wait("innerHeight === 600", "the shorter phone window")
        time.sleep(0.3)
        assert page.js("getComputedStyle(document.querySelector('.side')).display !== 'none'"), "the drawer closed when only the height changed"
        page.send("Emulation.setDeviceMetricsOverride", width=390, height=844, deviceScaleFactor=3, mobile=True)
        page.wait("innerHeight === 844", "the phone window again")
        page.click(".row.project.on")
        page.wait("getComputedStyle(document.querySelector('.side')).display === 'none'", "the drawer closed after a choice")
        # (Choosing the project starts a new conversation; a message sent before that is done
        # would be cleared with it.)
        page.wait("document.querySelector('#transcript')?.innerText.includes('What shall we work on?')", "the project's new conversation")
        time.sleep(0.5)
        assert page.js("matchMedia('(pointer: coarse)').matches"), "touch was not emulated"
        page.type("#draft", "please write hello.txt"); page.click("#send")
        page.wait("!!document.querySelector('.approval .approve-yes')", "the approval card")
        assert "Create hello.txt" in page.text(".approval"), page.text(".approval")
        assert not os.path.exists(f"{home}/proj/hello.txt"), "the file was written before approval"
        assert "write hello.txt" in page.text("#todos"), "the task list is not shown"
        # Allow and No are on screen, as large as each other, and big enough to touch.
        page.wait("(() => { const r = document.querySelector('.approve-no').getBoundingClientRect(); return r.top >= 0 && r.bottom <= innerHeight; })()", "Allow and No on screen")
        time.sleep(1)  # (the card's entrance movement scales it up to full size)
        boxes = json.loads(page.js("JSON.stringify(['.approve-yes', '.approve-no'].map(s => { const r = document.querySelector(s).getBoundingClientRect(); return [r.top, r.bottom, r.width, r.height]; }))"))
        assert all(0 <= top and bottom <= 844 and height >= 44 for top, bottom, _, height in boxes), f"Allow and No: {boxes}"
        assert abs(boxes[0][2] - boxes[1][2]) < 1 or min(b[2] for b in boxes) >= 84, f"No is smaller than Allow: {boxes}"
        page.shot(f"{shots}/2-approval-phone.png")
        # Seen from another view: a badge on Chat, the tab's title, and the status pill.
        page.click("#nav-dashboard")
        page.wait("!!document.querySelector('#waiting-badge')", "the badge on Chat")
        assert page.text("#waiting-badge") == "1", page.text("#waiting-badge")
        assert page.js("document.title") == "(1) CC · Causewaybay Code", page.js("document.title")
        assert "waiting for you" in page.text("#status-pill"), page.text("#status-pill")
        page.click("#nav-chat")
        page.wait("!document.querySelector('#waiting-badge')", "no badge in the chat itself")
        # An iPad, upright and on its side, with touch: nothing wider than the screen either.
        for width, height, name in [(820, 1180, "ipad-portrait"), (1366, 1024, "ipad-landscape")]:
            page.send("Emulation.setDeviceMetricsOverride", width=width, height=height, deviceScaleFactor=2, mobile=True)
            page.wait(f"innerWidth === {width}", f"the {name} width")
            if page.js("getComputedStyle(document.querySelector('.side')).display === 'none'"):
                page.click("#side-toggle")
            time.sleep(0.5)
            wide = page.js("JSON.stringify([...document.querySelectorAll('body *')].filter(e => e.getBoundingClientRect().width > 0 && e.getBoundingClientRect().right > innerWidth + 1).map(e => e.id || e.className).slice(0, 5))")
            assert wide == "[]", f"wider than the {name} screen: {wide}"
            page.shot(f"{shots}/2-approval-{name}.png")
        # Back to the desktop, with the sidebar open again.
        page.send("Emulation.clearDeviceMetricsOverride")
        page.send("Emulation.setTouchEmulationEnabled", enabled=False)
        page.send("Emulation.setEmulatedMedia", features=[{"name": "prefers-color-scheme", "value": "dark"}])
        page.wait("innerWidth > 640", "the desktop width again")
        if page.js("getComputedStyle(document.querySelector('.side')).display === 'none'"):
            page.click("#side-toggle")
        page.wait("getComputedStyle(document.querySelector('.side')).display !== 'none'", "the sidebar again")
        page.shot(f"{shots}/2-approval.png")
        page.click(".approval .approve-yes")
        page.wait("document.title === 'CC · Causewaybay Code'", "the page's own title once answered")
        page.wait("document.querySelector('#transcript').innerText.includes('Anything else?')", "the answer")
        assert open(f"{home}/proj/hello.txt").read() == "hello from the web\n"
        assert page.js("document.querySelectorAll('.msg.text pre').length") == 1, "the code block is not shown as code"
        assert "I will write the file" in page.text(".msg.thinking")
        page.wait("!!document.querySelector('.row.session')", "the conversation in the sidebar")
        page.wait("!!document.querySelector('#send')", "the send button back")
        page.shot(f"{shots}/3-chat.png")

        print("4. undo restores the file; the conversation reloads from the log")
        page.click("#undo")
        page.wait("document.querySelector('#transcript').innerText.includes('removed hello.txt')", "the undo note")
        assert not os.path.exists(f"{home}/proj/hello.txt"), "undo left the file"
        page.click("#new-chat")
        page.wait("document.querySelector('#transcript').innerText.includes('What shall we work on?')", "an empty conversation")
        # A conversation is reached with the keyboard too: Tab stops on it, Enter opens it.
        assert page.js("document.querySelector('.row.session').tabIndex") == 0
        page.js("(() => { const r = document.querySelector('.row.session'); r.focus(); r.dispatchEvent(new KeyboardEvent('keydown', {key: 'Enter', bubbles: true})); })()")
        page.wait("document.querySelector('#transcript').innerText.includes('Anything else?')", "the conversation loaded from the log")
        # A refresh comes back to this conversation.
        page.send("Page.navigate", url=url)
        page.wait("document.querySelector('#transcript')?.innerText.includes('Anything else?')", "the same conversation after a refresh")
        assert "answered: y" in page.text("#transcript"), "the logged approval is not shown as answered"
        assert page.js("document.querySelectorAll('.approve-yes').length") == 0, "an old approval can still be answered"

        # The conversation's outline has a bar for each item; the context meter is drawn; the
        # whole conversation is copied in one tap; nothing is wider than the transcript.
        page.click("#nav-chat")
        page.wait("!!document.querySelector('#transcript .it')", "the conversation")
        assert page.js("document.querySelectorAll('#outline .o').length === document.querySelectorAll('#transcript .it').length"), "the outline does not follow the conversation"
        # The outline is to scale, and a frame on it marks the part of the conversation on screen.
        page.wait("document.querySelector('#outline').classList.contains('scaled') && !!document.querySelector('#outline-view')", "the outline to scale")
        page.js("document.querySelector('.chatbody').style.maxHeight = '160px'")
        page.wait("document.querySelector('#transcript').scrollHeight > document.querySelector('#transcript').clientHeight + 40", "a conversation longer than its box")
        frame = "(() => { const o = document.querySelector('#outline').getBoundingClientRect(), f = document.querySelector('#outline-view').getBoundingClientRect(); return [(f.top - o.top) / o.height, f.height / o.height]; })()"
        page.js("document.querySelector('#transcript').scrollTop = 0")
        page.wait(f"{frame}[0] < 0.02 && {frame}[1] < 0.95", "the frame at the top of the outline")
        page.wait(f"Math.abs({frame}[1] - document.querySelector('#transcript').clientHeight / document.querySelector('#transcript').scrollHeight) < 0.05", "the frame as tall as the part on screen")
        page.js("document.querySelector('#transcript').scrollTop = 1e6")
        page.wait(f"Math.abs({frame}[0] + {frame}[1] - 1) < 0.03", "the frame at the bottom after scrolling to the end")
        # Each bar sits where its item is.
        assert page.js("(() => { const bars = [...document.querySelectorAll('#outline .o')].map(b => b.getBoundingClientRect().top); return bars.every((t, i) => i === 0 || t >= bars[i - 1] - 0.5); })()"), "the bars are out of order"
        # Pressing on the outline scrolls the conversation there.
        page.js("(() => { const o = document.querySelector('#outline').getBoundingClientRect(); const at = { bubbles: true, clientX: o.left + 5, clientY: o.top + 2, pointerId: 1 }; document.querySelector('#outline').dispatchEvent(new PointerEvent('pointerdown', at)); document.dispatchEvent(new PointerEvent('pointerup', at)); })()")
        page.wait("document.querySelector('#transcript').scrollTop < 5", "the top of the conversation after pressing the top of the outline")
        page.js("document.querySelector('.chatbody').style.maxHeight = ''")
        assert page.js("!!document.querySelector('#ctx .bar2 i') && document.querySelector('#ctx-text').innerText.includes('/')"), page.text("#ctx")
        assert page.js("document.querySelector('#transcript').scrollWidth <= document.querySelector('#transcript').clientWidth + 1"), "the transcript is wider than its box"
        assert page.js("getComputedStyle(document.querySelector('#undo')).borderTopWidth !== '0px' && !document.querySelector('#undo').classList.contains('ghost')"), "undo does not look like a button"
        page.click("#copy-all")
        page.wait("(document.querySelector('#notice') || {}).innerText?.includes('copied') || (document.querySelector('#toast') || {}).innerText?.includes('copy')", "an answer to copying")
        assert "copied" in page.text("#notice"), page.text("#toast")
        # Run, Console, Clear: the code is run and what it printed is in the console; Clear
        # empties the console and starts a new conversation.
        with open(f"{home}/proj/hello.sh", "w") as f: f.write("echo run from the button\n")
        os.utime(f"{home}/proj/hello.sh")
        assert page.js("!document.querySelector('#console')"), "the console is open before it is asked for"
        page.click("#run")
        page.wait("(document.querySelector('#console-out') || {}).innerText?.includes('[done]')", "the run to finish in the console")
        ran = page.text("#console-out")
        assert "$ sh 'hello.sh'" in ran and "run from the button" in ran, ran
        page.type("#console-cmd", "echo typed; exit 4")
        page.click("#run")
        page.wait("document.querySelector('#console-out').innerText.includes('[exit status 4]')", "the typed command's end")
        assert "run from the button" in page.text("#console-out"), "the earlier run is gone from the console"
        page.click("#console-toggle"); page.wait("!document.querySelector('#console')", "the console hidden")
        page.click("#console-toggle"); page.wait("document.querySelector('#console-out').innerText.includes('typed')", "the console back, with what it had")
        assert page.js("document.querySelectorAll('#transcript .it').length") > 0
        page.click("#clear")
        page.wait("document.querySelectorAll('#transcript .it').length === 0 && document.querySelector('#console-out').innerText.includes('Nothing has been run')", "everything cleared")
        page.click("#console-close")
        page.js("[...document.querySelectorAll('.list.sessions .row')][0].click()")
        page.wait("document.querySelectorAll('#transcript .it').length > 0", "the conversation still there after Clear")
        # The typeface chosen is the whole conversation's: what was said, its code, the tool calls.
        page.click("#clear"); page.js("[...document.querySelectorAll('.list.sessions .row')].at(-1).click()")
        page.wait("!!document.querySelector('#transcript .msg pre') && !!document.querySelector('#transcript .tool summary')", "a conversation with code and a tool call")
        parts = ["body", "#nav-chat", ".msg.user", ".msg.text", ".msg pre", ".tool summary", ".note", "#draft"]
        face_of = lambda: json.loads(page.js("JSON.stringify(%s.map(q => getComputedStyle(document.querySelector(q)).fontFamily.split(',')[0].replaceAll('\"', '').trim()))" % json.dumps(parts)))
        assert set(face_of()) == {"VT323"}, face_of()
        for font, family in [("geist", "Geist"), ("fira", "FiraCode")]:
            page.js(f"(() => {{ const s = document.querySelector('#font'); s.value = '{font}'; s.dispatchEvent(new Event('change', {{bubbles: true}})); }})()")
            page.wait(f"document.documentElement.dataset.font === '{font}'", f"the {font} typeface")
            assert set(face_of()) == {family}, dict(zip(parts, face_of()))
        page.js("(() => { const s = document.querySelector('#font'); s.value = 'pixel'; s.dispatchEvent(new Event('change', {bubbles: true})); })()")
        print("5. the dashboard shows the backend")
        page.click("#nav-dashboard")
        page.wait("document.querySelector('#protocol') && ['HTTP/2 · TCP','HTTP/3 · UDP'].includes(document.querySelector('#protocol').innerText)", "the protocol card")
        protocol = page.text("#protocol")
        assert page.text("#model-state") == "scripted", page.text("#model-state")
        assert "proj" in page.text("table"), "the project is not listed"
        # How to trust this backend on a device: its certificate authority, from the page.
        assert page.js("document.querySelector('#trust-download').getAttribute('href')") == "/ca.crt"
        trusting = ssl.create_default_context(cafile=f"{home}/.cwbcode/ca.pem")
        ca = urllib.request.urlopen(f"https://localhost:{port}/ca.crt", context=trusting).read().decode()
        assert ca == open(f"{home}/.cwbcode/ca.pem").read(), "the downloaded authority is not the backend's"
        # Beside it, its SHA-256, to compare with the `ca` line of `cwbcode url`.
        ca_sha = hashlib.sha256(ssl.PEM_cert_to_DER_cert(ca)).hexdigest()
        assert page.text("#trust-fingerprint") == ca_sha, page.text("#trust-fingerprint")
        printed = subprocess.run([f"{ROOT}/target/release/cwbcode", "url", "--bind", f"127.0.0.1:{port}", "--env-file", "/dev/null"], env=dict(env, CWBCODE_WALLET=WALLET), capture_output=True, text=True).stdout
        assert f"(sha256 {ca_sha})" in next(line for line in printed.splitlines() if line.startswith("ca ")), printed
        page.shot(f"{shots}/4-dashboard.png")

        print("6. the knowledge base saves and finds a note")
        page.click("#nav-knowledge")
        page.wait("!!document.querySelector('#kb-note')", "the knowledge page")
        page.type("#kb-title", "Deploy day"); page.type("#kb-note", "We deploy on Thursdays; Minsu owns it."); page.click("#kb-save")
        page.wait("document.querySelector('#kb-list').innerText.includes('Deploy day')", "the saved note")
        page.type("#kb-query", "when do we deploy"); page.click("#kb-search")
        page.wait("(document.querySelector('#kb-hits') || {}).innerText?.includes('Deploy day')", "the search hit")
        page.shot(f"{shots}/5-knowledge.png")

        print("7. the tools page, and a second message in a new conversation")
        page.click("#nav-tools")
        page.wait("document.body.innerText.includes('No tools yet')", "the tools page")
        page.click("#nav-chat"); page.click("#new-chat")
        page.type("#draft", "again"); page.click("#send")
        page.wait("document.querySelector('#transcript').innerText.includes('A second answer.')", "the second answer")
        # A conversation is removed with its ×, by real mouse clicks. The × asks on the row itself
        # ("Remove?" Yes / No; no browser dialog, which the Mac app's window would not show).
        rows = "document.querySelectorAll('.row.session').length"
        before = page.js(rows)
        assert before >= 2, before
        page.js("window.confirm = () => { window.usedConfirm = true; return false; }")
        other = ".row.session:not(.on)"
        page.mouse(f"{other} .remove")
        page.wait("!!document.querySelector('.row.session .remove-yes')", "the question on the row")
        assert page.js(rows) == before, "a conversation was removed before a yes"
        # No: the row is as it was.
        page.mouse(".row.session .remove-no")
        page.wait("!document.querySelector('.row.session .remove-yes')", "the question gone after No")
        assert page.js(rows) == before, "No removed a conversation"
        # Yes: it is gone, from the list and from the backend.
        gone = page.js(f"document.querySelector('{other} small').innerText.split(' ')[0]")
        page.mouse(f"{other} .remove")
        page.wait("!!document.querySelector('.row.session .remove-yes')", "the question again")
        page.shot(f"{shots}/7b-remove-ask.png")
        page.mouse(".row.session .remove-yes")
        page.wait(f"{rows} === {before - 1}", "one conversation fewer")
        page.wait("(document.querySelector('#notice') || {}).innerText?.includes('is removed')", "the notice that it was removed")
        assert gone not in page.js("[...document.querySelectorAll('.row.session small')].map(e => e.innerText).join(' ')"), f"{gone} is still listed"
        # It stays gone after a reload: the backend has deleted it.
        page.send("Page.navigate", url=url)
        page.wait(f"!!document.querySelector('.side') && {rows} === {before - 1}", "still gone after a reload")
        # The one that is open: removing it leaves an empty, new conversation.
        page.mouse(".row.session")
        page.wait("!!document.querySelector('.row.session.on')", "a conversation open")
        page.mouse(".row.session.on .remove")
        page.wait("!!document.querySelector('.row.session .remove-yes')", "the question for the open one")
        page.mouse(".row.session .remove-yes")
        page.wait(f"{rows} === {before - 2} && document.querySelector('#transcript').innerText.includes('What shall we work on?')", "a new conversation after removing the open one")
        assert not page.js("window.usedConfirm"), "the browser's own confirm box was used"
        print("8. signing out ends the session; a reload stays signed out")
        session = page.js("localStorage.getItem('cwbcode-token')")
        page.click("#signout")
        page.wait("!!document.querySelector('#phrase') && !document.querySelector('.side')", "the sign-in page after signing out")
        assert page.js("localStorage.getItem('cwbcode-token')") is None
        assert page.js("localStorage.getItem('cwbcode-phrase')") is None, "signing out left the phrase on the device"
        page.send("Page.navigate", url=url)
        page.wait("!!document.querySelector('#phrase')", "the sign-in page after a reload")
        # The old session no longer opens the backend.
        page.js(f"localStorage.setItem('cwbcode-token', {json.dumps(session)})")
        page.send("Page.navigate", url=url)
        page.wait("!!document.querySelector('#phrase')", "the sign-in page with the ended session")
        assert not [e for e in page.errors if "401" not in e], f"the page reported errors: {page.errors}"
        page.errors.clear()

        print("9. a backend without a wallet is in local mode: the one-time link, no wallet")
        port2 = free_port()
        # (No address is given: the backend chooses it by its mode, and moves when the mode changes.)
        local = subprocess.Popen([f"{ROOT}/target/release/cwbcode", "serve", "--port", str(port2), "--script", f"{home}/script.json", "--env-file", "/dev/null"], env=dict(env, CWBCODE_HOME=f"{home}/.cwbcode-local"), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", port2), timeout=0.2).close(); break
            except OSError: time.sleep(0.1)
        # Without the token: only the page that asks for it.
        page.send("Page.navigate", url=f"https://127.0.0.1:{port2}")
        page.wait("!!document.querySelector('#token') && !document.querySelector('.side')", "the token page in local mode")
        local_token = open(f"{home}/.cwbcode-local/token").read().strip()
        # The link `cwbcode url` prints carries a one-time code, not the token (the browser's
        # history keeps addresses); the page exchanges it for the token and takes it out of
        # the address.
        def open_link():
            printed = subprocess.run([f"{ROOT}/target/release/cwbcode", "url", "--port", str(port2), "--env-file", "/dev/null"], env=dict(env, CWBCODE_HOME=f"{home}/.cwbcode-local"), capture_output=True, text=True).stdout
            link = next(line.split()[1] for line in printed.splitlines() if line.startswith("open "))
            assert link.startswith(f"https://127.0.0.1:{port2}/#code=") and local_token not in printed, printed
            return link
        link = open_link()
        # Pasted into this same tab (only the part after # changes, the page does not reload).
        page.send("Page.navigate", url=link)
        page.wait("!!document.querySelector('.side')", "the app from the link pasted into the open tab")
        assert page.js("location.hash") == "", "the code stayed in the address"
        assert page.js("localStorage.getItem('cwbcode-token')") == local_token, "the token the code was exchanged for was not kept"
        # (Another address is another site to the browser: its language starts from the system's.)
        page.js("(() => { const s = document.querySelector('#lang'); s.value = 'en'; s.dispatchEvent(new Event('change', {bubbles: true})); })()")
        # The same link again: the code was used up. The page says so, and keeps the token it
        # has; so does a token in a link that is not the backend's.
        page.send("Page.navigate", url=link)
        page.wait("(document.querySelector('#toast') || {}).innerText?.includes('did not work')", "the refusal of a used link")
        page.send("Page.navigate", url=f"https://127.0.0.1:{port2}/#token=not-the-token")
        time.sleep(1)
        assert page.js("location.hash") == "" and page.js("!!document.querySelector('.side')"), "a bad link signed the page out"
        assert page.js("localStorage.getItem('cwbcode-token')") == local_token, "a bad link replaced the token kept"
        # A fresh link opened where a token that no longer works is kept: the code's token
        # replaces it (the old one's refusal, arriving after, does not undo that).
        page.js("localStorage.setItem('cwbcode-token', 'an-old-token')")
        page.send("Page.navigate", url="about:blank"); time.sleep(0.3)
        page.send("Page.navigate", url=open_link())
        page.wait("!!document.querySelector('.side') && localStorage.getItem('cwbcode-token') === " + json.dumps(local_token), "the app from a new link over an old token")
        time.sleep(1)
        assert page.js("!!document.querySelector('.side')") and page.js("localStorage.getItem('cwbcode-token')") == local_token, "the old token's refusal signed the page out"
        # A token typed into the address by hand still works.
        page.js("localStorage.removeItem('cwbcode-token')")
        page.send("Page.navigate", url="about:blank"); time.sleep(0.3)
        page.send("Page.navigate", url=f"https://127.0.0.1:{port2}/#token={local_token}")
        page.wait("!!document.querySelector('.side') && localStorage.getItem('cwbcode-token') === " + json.dumps(local_token), "the app from a typed token link")
        assert page.js("location.hash") == "", "the token stayed in the address"
        assert page.js("!document.querySelector('#phrase') && !document.querySelector('#token') && !document.querySelector('#signout')"), "local mode showed a login"
        page.wait("document.querySelector('#login-state') && document.querySelector('#login-state').innerText.includes('local mode')", "the local-mode label")
        page.type("#open-path", "~/proj"); page.click("#open-go")
        page.wait("!!document.querySelector('.row.project.on')", "the project in local mode")
        page.shot(f"{shots}/6-local-mode.png")

        print("10. remote mode is turned on from the dashboard, asks for the wallet, and is turned off again")
        settings = f"{home}/.cwbcode-local/settings.jsonl"
        page.click("#nav-dashboard")
        page.wait("!!document.querySelector('#remote-wallet')", "the remote access card")
        assert "Local mode" in page.text("#remote") and f"127.0.0.1:{port2}" in page.text("#remote"), page.text("#remote")
        page.type("#remote-wallet", "0x1234"); page.click("#remote-on")
        page.wait("(document.querySelector('#remote-problem') || {}).innerText?.includes('address')", "the refusal of what is no address")
        assert not os.path.exists(settings), "a bad address was saved"
        # The recovery phrase works the address out in the browser; only the address is sent.
        page.type("#remote-wallet", PHRASE)
        page.wait("(document.querySelector('#remote-derived') || {}).innerText?.toLowerCase().includes('" + WALLET.lower() + "')", "the address of the phrase")
        page.shot(f"{shots}/7-remote-setup.png")
        page.click("#remote-on")
        page.wait("!!document.querySelector('#phrase') && !document.querySelector('.side')", "the sign-in page once remote mode is on")
        assert page.text("#backend-wallet").lower() == WALLET.lower()
        saved = [json.loads(line) for line in open(settings)]
        assert saved[-1]["set"] == "wallet" and saved[-1]["value"].lower() == WALLET.lower() and "junk" not in open(settings).read(), saved
        # The backend moved to every interface without a restart.
        time.sleep(1.5)
        listening = subprocess.run(["lsof", "-nP", f"-iTCP:{port2}", "-sTCP:LISTEN"], capture_output=True, text=True).stdout
        assert f"*:{port2}" in listening, f"not listening on every interface: {listening}"
        page.type("#phrase", PHRASE); page.click("#signin")
        page.wait("!!document.querySelector('.side')", "the app after signing in to remote mode")
        page.click("#nav-dashboard")
        page.wait("(document.querySelector('#remote') || {}).innerText?.includes('Remote mode')", "the card in remote mode")
        page.wait(f"document.querySelector('#remote').innerText.includes('0.0.0.0:{port2}')", "the new address on the card")
        page.shot(f"{shots}/8-remote-on.png")
        page.click("#remote-off")
        # The wallet's session ended with remote mode: local mode asks for this machine's token.
        page.wait("!!document.querySelector('#token')", "the token page once remote mode is off")
        page.send("Page.navigate", url="about:blank"); time.sleep(0.3)
        page.send("Page.navigate", url=open_link())
        page.wait("!!document.querySelector('.side') && !document.querySelector('#signout')", "local mode again, from a new link")
        page.click("#nav-dashboard")
        page.wait("(document.querySelector('#remote') || {}).innerText?.includes('Local mode')", "the card in local mode")
        time.sleep(1.5)
        listening = subprocess.run(["lsof", "-nP", f"-iTCP:{port2}", "-sTCP:LISTEN"], capture_output=True, text=True).stdout
        assert f"127.0.0.1:{port2}" in listening and f"*:{port2}" not in listening, f"still listening beyond this machine: {listening}"
        assert [json.loads(line)["value"] for line in open(settings)][-1] == ""
        assert not [e for e in page.errors if "401" not in e and "ERR_" not in e], f"the page reported errors: {page.errors}"
        page.errors.clear()
        assert not page.errors, f"the page reported errors: {page.errors}"

        print("11. a backend without the model offers to download it, and says why a download stopped")
        local.terminate(); local.wait(5)
        port3 = free_port()
        closed = free_port()  # nothing listens there: the "Hugging Face" this download asks
        bare = dict(env, CWBCODE_HOME=f"{home}/.cwbcode-bare", CWBCODE_MODELS=f"{home}/models", OLLAMA_MODELS=f"{home}/no-ollama",
                    HF_HOME=f"{home}/no-hf", CWBCODE_HF_ENDPOINT=f"http://127.0.0.1:{closed}", CWBCODE_OLLAMA_REGISTRY=f"http://127.0.0.1:{closed}")
        for k in ("HF_TOKEN", "HUGGING_FACE_HUB_TOKEN", "HF_TOKEN_PATH", "CWBCODE_DOWNLOAD_MODEL"): bare.pop(k, None)
        # (--no-download-model: the model is "missing", and the page offers the download.)
        local = subprocess.Popen([f"{ROOT}/target/release/cwbcode", "serve", "--port", str(port3), "--no-h3", "--env-file", "/dev/null", "--no-download-model"], env=bare, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(100):
            if os.path.exists(f"{home}/.cwbcode-bare/token"): break
            time.sleep(0.1)
        bare_token = open(f"{home}/.cwbcode-bare/token").read().strip()
        page.send("Page.navigate", url=f"https://127.0.0.1:{port3}/#token={bare_token}")
        page.wait("!!document.querySelector('.side')", "the app of the backend without a model")
        page.click("#nav-chat")
        page.wait("!!document.querySelector('#model-download #dl-start')", "the button that downloads the model, in the chat view")
        assert "20 GB" in page.text("#dl-start"), page.text("#dl-start")
        assert page.js("!document.querySelector('#empty-chat')"), "the empty page beside the download"
        page.shot(f"{shots}/9-model-missing.png")
        page.click("#dl-start")
        page.wait("!!document.querySelector('#model-download #dl-retry')", "the retry after a download that could not start", seconds=30)
        assert str(closed) in page.text("#dl-error") or "error" in page.text("#dl-error").lower(), page.text("#dl-error")
        page.shot(f"{shots}/9-model-download-failed.png")
        page.click("#nav-dashboard")
        page.wait("!!document.querySelector('#model-download-card #dl-retry')", "the retry on the model's card")
        assert page.text("#model-state") == "missing", page.text("#model-state")
        assert not [e for e in page.errors if "ERR_" not in e], f"the page reported errors: {page.errors}"
        print("12. a backend downloading the model from a fake Hugging Face: the Forge, on two pages at once, to Ready")
        local.terminate(); local.wait(5)
        hub = FakeHub(rate=1_200_000, registry_delay=4.0)
        port4 = free_port()
        forge_env = dict(bare, CWBCODE_HOME=f"{home}/.cwbcode-forge", CWBCODE_MODELS=f"{home}/forge-models",
                         CWBCODE_HF_ENDPOINT=f"http://127.0.0.1:{hub.port}", CWBCODE_OLLAMA_REGISTRY=f"http://127.0.0.1:{hub.port}")
        # The script stands in for the model once it is downloaded: the download runs to Ready.
        local = subprocess.Popen([f"{ROOT}/target/release/cwbcode", "serve", "--port", str(port4), "--no-h3", "--env-file", "/dev/null",
                                  "--script", f"{home}/script.json", "--script-after-download"], env=forge_env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(100):
            if os.path.exists(f"{home}/.cwbcode-forge/token"): break
            time.sleep(0.1)
        forge_token = open(f"{home}/.cwbcode-forge/token").read().strip()
        forge_url = f"https://127.0.0.1:{port4}/#token={forge_token}"
        page.send("Page.navigate", url=forge_url)
        page.wait("!!document.querySelector('.side')", "the app of the backend that downloads")
        page.js("(() => { const s = document.querySelector('#lang'); s.value = 'en'; s.dispatchEvent(new Event('change', {bubbles: true})); })()")
        page.click("#nav-chat")
        # A second page, phone-sized, on the same backend: both are told the same, at once.
        target = json.load(urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:{debug}/json/new?about:blank", method="PUT")))
        phone = Page(connect(target["webSocketDebuggerUrl"], max_size=50_000_000))
        for domain in ("Runtime", "Log", "Page"): phone.send(f"{domain}.enable")
        phone.send("Emulation.setDeviceMetricsOverride", width=390, height=844, deviceScaleFactor=2, mobile=True)
        phone.send("Emulation.setEmulatedMedia", features=[{"name": "prefers-color-scheme", "value": "dark"}])
        phone.send("Page.navigate", url=forge_url)
        phone.wait("!!document.querySelector('#model-download')", "the Forge on the phone")
        phone.js("(() => { const s = document.querySelector('#lang'); if (s) { s.value = 'en'; s.dispatchEvent(new Event('change', {bubbles: true})); } })()")
        # (A page drawn in a tab behind another is not drawn: each is brought to the front.)
        page.send("Page.bringToFront")
        page.wait("document.querySelector('#model-download')?.dataset.stage === 'hammer'", "the hammering stage", seconds=30)
        # The scene is drawn with WebGL, frame after frame; the robot gets its parts.
        page.wait("document.querySelector('#forge-scene')?.dataset.webgl === 'yes' && !!document.querySelector('#forge-scene canvas')", "the 3D scene")
        frames = page.js("+document.querySelector('#forge-scene').dataset.frames")
        time.sleep(0.5)
        assert page.js("+document.querySelector('#forge-scene').dataset.frames") > frames, "the scene is not being drawn"
        first = page.js("parseFloat(document.querySelector('#model-download').dataset.percent)")
        page.wait(f"parseFloat(document.querySelector('#model-download').dataset.percent) > {first + 10}", "the percent rising", seconds=20)
        assert "Hammering the weights" in page.text("#forge-stage") and "STAGE 2 OF 5" in page.text("#forge-step"), page.text("#model-download")
        assert "pieces" in page.text("#dl-files") and "MB/s" in page.text("#dl-speed") and "GB" in page.text("#dl-bytes"), page.text("#model-download")
        assert "without a token" in page.text("#dl-source"), page.text("#dl-source")
        assert "hf auth login" in page.text("#dl-tip"), "no tip without a token"
        assert "forging the model" in page.text("#status-pill"), page.text("#status-pill")
        page.wait("+document.querySelector('#forge-scene').dataset.parts >= 2", "the robot's first parts")
        assert page.js("document.documentElement.scrollWidth <= innerWidth"), "the Forge is wider than the window"
        page.shot(f"{shots}/12-forge-hammer.png")
        phone.send("Page.bringToFront")
        phone.wait("document.querySelector('#model-download')?.dataset.stage === 'hammer'", "the phone told the same")
        wide = phone.js("JSON.stringify([...document.querySelectorAll('body *')].filter(e => e.getBoundingClientRect().width > 0 && e.getBoundingClientRect().right > innerWidth + 1).map(e => e.id || e.className || e.tagName + ':' + e.textContent.slice(0, 40)).slice(0, 5))")
        assert wide == "[]", f"wider than the phone: {wide}"
        assert phone.js("Math.round(document.querySelector('#forge-scene').getBoundingClientRect().height)") == 360
        phone.shot(f"{shots}/12-forge-hammer-phone.png")
        # The tip can be put away, and stays away.
        page.send("Page.bringToFront")
        page.click("#dl-tip-hide")
        page.wait("!document.querySelector('#dl-tip')", "the tip hidden")
        for stage, name in [("temper", "Tempering"), ("wake", "Waking it up")]:
            page.send("Page.bringToFront")
            page.wait(f"document.querySelector('#model-download')?.dataset.stage === '{stage}'", f"the {stage} stage", seconds=30)
            assert name in page.text("#forge-stage"), page.text("#forge-stage")
            page.shot(f"{shots}/12-forge-{stage}.png", settle=0.3)
            phone.send("Page.bringToFront")
            phone.wait(f"document.querySelector('#model-download')?.dataset.stage === '{stage}'", f"the {stage} stage on the phone")
            phone.shot(f"{shots}/12-forge-{stage}-phone.png", settle=0.3)
        page.send("Page.bringToFront")
        page.wait("document.querySelector('#model-download')?.dataset.stage === 'ready'", "Ready", seconds=20)
        page.shot(f"{shots}/12-forge-ready.png", settle=0.4)
        phone.send("Page.bringToFront")
        phone.wait("document.querySelector('#model-download')?.dataset.stage === 'ready' || !document.querySelector('#model-download')", "Ready on the phone")
        phone.shot(f"{shots}/12-forge-ready-phone.png", settle=0.1)
        # Then the scene fades, and the chat is there, its composer in focus.
        page.send("Page.bringToFront")
        page.wait("!document.querySelector('#model-download') && document.activeElement?.id === 'draft'", "the chat after Ready", seconds=10)
        assert page.text("#status-pill").startswith("ready"), page.text("#status-pill")
        page.shot(f"{shots}/12-chat-after-ready.png")
        assert not [e for e in page.errors + phone.errors if "ERR_" not in e], f"the page reported errors: {page.errors + phone.errors}"
        assert hub.seen_resolve > 0, "the fake Hugging Face was not asked"
        print(f"All passed (the browser used {protocol}). Screenshots: {shots}")
    finally:
        chrome.terminate(); server.terminate()
        if local: local.terminate()
        try: chrome.wait(5)
        except Exception: chrome.kill()
        server.wait(5)
        shutil.rmtree(home, ignore_errors=True)

if __name__ == "__main__":
    main()
