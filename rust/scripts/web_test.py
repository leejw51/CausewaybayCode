#!/usr/bin/env python3
"""The web client, tested in a real browser: starts the backend with a scripted model in a
scratch home folder, drives headless Chrome over its DevTools protocol through the page
(sign in with a wallet's recovery phrase, open a project, chat, approve an edit, dashboard,
knowledge base, tools, sign out; then a backend in local mode, which asks for no login, and
turning its remote mode on and off from the dashboard), and
checks what the page shows and what happened on disk. Screenshots go to the folder given
as the first argument (default: a temporary one).

Needs Google Chrome and the Python package `websockets`. Run `make build` first."""
import base64, json, os, shutil, socket, subprocess, sys, tempfile, time, urllib.request
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
    def click_text(self, selector, text):
        self.js(f"[...document.querySelectorAll({json.dumps(selector)})].find(e => e.textContent.trim() === {json.dumps(text)}).click()")
    def text(self, selector):
        return self.js(f"(document.querySelector({json.dumps(selector)}) || {{}}).innerText || ''")
    def shot(self, path):
        time.sleep(0.8)  # let the entrance movement finish
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
        # The browser keeps the session and, as "remember on this device" is ticked, the phrase.
        assert page.js("/^[0-9a-f]{64}$/.test(localStorage.getItem('cwbcode-token') || '')"), "no session was stored"
        kept = json.loads(page.js("localStorage.getItem('cwbcode-phrase')") or "{}")
        assert kept == {"phrase": PHRASE, "index": 0}, f"the phrase was not remembered: {kept}"
        # A refresh comes straight back in; so does a session that has ended, by the phrase.
        page.send("Page.navigate", url=url)
        page.wait("!!document.querySelector('.side')", "the app after a refresh")
        page.js("localStorage.setItem('cwbcode-token', 'an-ended-session')")
        page.send("Page.navigate", url=url)
        page.wait("!!document.querySelector('.side') && /^[0-9a-f]{64}$/.test(localStorage.getItem('cwbcode-token') || '')", "signing in again with the remembered phrase")
        page.errors.clear()  # (the ended session was refused once, as it should be)

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

        print("2b. light and dark, side by side and stacked, eight languages; all kept across a refresh")
        assert page.js("document.documentElement.dataset.theme") == "dark"
        page.click("#theme-toggle")
        page.wait("document.documentElement.dataset.theme === 'light'", "the light theme")
        page.wait("getComputedStyle(document.body).color === 'rgb(19, 23, 34)'", "dark text on the light theme")
        page.click("#layout-toggle")
        page.wait("document.querySelector('.app').classList.contains('vertical')", "the stacked layout")
        assert page.js("document.querySelector('.side').getBoundingClientRect().width > document.querySelector('.side').getBoundingClientRect().height"), "the sidebar did not become a header"
        seen = {}
        for code, chat in [("ko", "채팅"), ("cs", "Chat"), ("ja", "チャット"), ("yue", "對話"), ("zh", "对话"), ("es", "Chat"), ("tr", "Sohbet")]:
            page.js(f"(() => {{ const s = document.querySelector('#lang'); s.value = '{code}'; s.dispatchEvent(new Event('change', {{bubbles: true}})); }})()")
            page.wait(f"document.querySelector('#nav-chat').innerText.trim() === {json.dumps(chat)} && document.documentElement.lang === '{code}'", f"the interface in {code}")
            seen[code] = page.text("#nav-knowledge")
        assert len(set(seen.values())) >= 6, seen
        page.shot(f"{shots}/2b-light-stacked-turkish.png")
        page.js("(() => { const s = document.querySelector('#lang'); s.value = 'ko'; s.dispatchEvent(new Event('change', {bubbles: true})); })()")
        page.click("#nav-knowledge")
        page.wait("!!document.querySelector('#kb-note')", "the knowledge view")
        prefs = json.loads(page.js("localStorage.getItem('cwbcode-prefs')"))
        assert (prefs["theme"], prefs["layout"], prefs["lang"], prefs["view"]) == ("light", "vertical", "ko", "knowledge") and prefs["project"].endswith("/proj"), prefs
        page.send("Page.navigate", url=url)
        page.wait("!!document.querySelector('#kb-note') && document.querySelector('.app').classList.contains('vertical')", "the same view and layout after a refresh")
        assert page.js("document.documentElement.dataset.theme") == "light" and page.text("#nav-chat") == "채팅"
        page.wait("document.querySelector('.row.project.on')?.innerText.includes('proj')", "the same project after a refresh")
        page.wait("getComputedStyle(document.body).color === 'rgb(19, 23, 34)'", "the light theme's colours after a refresh")
        # Back to English, dark, side by side for the rest.
        page.js("(() => { const s = document.querySelector('#lang'); s.value = 'en'; s.dispatchEvent(new Event('change', {bubbles: true})); })()")
        page.click("#theme-toggle"); page.wait("document.documentElement.dataset.theme === 'dark'", "the dark theme")
        page.click("#layout-toggle"); page.wait("!document.querySelector('.app').classList.contains('vertical')", "side by side")
        page.click("#nav-chat")
        page.wait("!!document.querySelector('#draft')", "the chat view")

        print("3. a message streams, an edit waits for approval, and Allow lets it through")
        page.type("#draft", "please write hello.txt"); page.click("#send")
        page.wait("!!document.querySelector('.approval .approve-yes')", "the approval card")
        assert "Create hello.txt" in page.text(".approval"), page.text(".approval")
        assert not os.path.exists(f"{home}/proj/hello.txt"), "the file was written before approval"
        assert "write hello.txt" in page.text("#todos"), "the task list is not shown"
        page.shot(f"{shots}/2-approval.png")
        page.click(".approval .approve-yes")
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
        page.click(".row.session")
        page.wait("document.querySelector('#transcript').innerText.includes('Anything else?')", "the conversation loaded from the log")
        # A refresh comes back to this conversation.
        page.send("Page.navigate", url=url)
        page.wait("document.querySelector('#transcript')?.innerText.includes('Anything else?')", "the same conversation after a refresh")
        assert "answered: y" in page.text("#transcript"), "the logged approval is not shown as answered"
        assert page.js("document.querySelectorAll('.approve-yes').length") == 0, "an old approval can still be answered"

        print("5. the dashboard shows the backend")
        page.click("#nav-dashboard")
        page.wait("document.querySelector('#protocol') && ['H2','H3'].includes(document.querySelector('#protocol').innerText)", "the protocol card")
        protocol = page.text("#protocol")
        assert page.text("#model-state") == "scripted", page.text("#model-state")
        assert "proj" in page.text("table"), "the project is not listed"
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

        print("9. a backend without a wallet is in local mode: no login at all")
        port2 = free_port()
        # (No address is given: the backend chooses it by its mode, and moves when the mode changes.)
        local = subprocess.Popen([f"{ROOT}/target/release/cwbcode", "serve", "--port", str(port2), "--script", f"{home}/script.json", "--env-file", "/dev/null"], env=dict(env, CWBCODE_HOME=f"{home}/.cwbcode-local"), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", port2), timeout=0.2).close(); break
            except OSError: time.sleep(0.1)
        page.send("Page.navigate", url=f"https://127.0.0.1:{port2}")
        page.wait("!!document.querySelector('.side')", "the app, straight away")
        # (Another address is another site to the browser: its language starts from the system's.)
        page.js("(() => { const s = document.querySelector('#lang'); s.value = 'en'; s.dispatchEvent(new Event('change', {bubbles: true})); })()")
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
        page.wait("!!document.querySelector('.side') && !document.querySelector('#signout')", "local mode again, with no login")
        page.wait("(document.querySelector('#remote') || {}).innerText?.includes('Local mode')", "the card in local mode")
        time.sleep(1.5)
        listening = subprocess.run(["lsof", "-nP", f"-iTCP:{port2}", "-sTCP:LISTEN"], capture_output=True, text=True).stdout
        assert f"127.0.0.1:{port2}" in listening and f"*:{port2}" not in listening, f"still listening beyond this machine: {listening}"
        assert [json.loads(line)["value"] for line in open(settings)][-1] == ""
        assert not [e for e in page.errors if "401" not in e and "ERR_" not in e], f"the page reported errors: {page.errors}"
        page.errors.clear()
        assert not page.errors, f"the page reported errors: {page.errors}"
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
