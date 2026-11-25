#!/usr/bin/env python3
"""The web client, tested in a real browser: starts the backend with a scripted model in a
scratch home folder, drives headless Chrome over its DevTools protocol through the page
(sign in, open a project, chat, approve an edit, dashboard, knowledge base, tools), and
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
    server = subprocess.Popen([f"{ROOT}/target/release/cwbcode", "serve", "--bind", f"127.0.0.1:{port}", "--script", f"{home}/script.json"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
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

        print("1. the page asks for the token, and refuses a wrong one")
        page.send("Page.navigate", url=url)
        page.wait("!!document.querySelector('#token')", "the token page")
        assert page.js("typeof Motion === 'object' && typeof Motion.animate === 'function'"), "Motion did not load"
        page.shot(f"{shots}/1-token.png")
        page.type("#token", "wrong-token"); page.click("#signin")
        page.wait("!!document.querySelector('#token') && !document.querySelector('.side')", "the token page again after a wrong token")
        time.sleep(0.5)
        assert page.js("!document.querySelector('.side')"), "a wrong token was accepted"
        assert page.errors and all("401" in e for e in page.errors), f"expected only the refused sign-in so far: {page.errors}"
        page.errors.clear()
        page.type("#token", token); page.click("#signin")
        page.wait("!!document.querySelector('.side')", "the app after signing in")
        page.wait("document.querySelector('#status-pill').innerText.includes('scripted')", "the backend's status")

        print("2. a folder outside home is refused, one under it opens")
        page.type("#open-path", "/etc"); page.click("#open-go")
        page.wait("(document.querySelector('#toast') || {}).innerText?.includes('home folder')", "the refusal of /etc")
        page.type("#open-path", "~/proj"); page.click("#open-go")
        page.wait("!!document.querySelector('.row.project.on')", "the opened project")
        assert os.path.exists(f"{home}/proj/.cwbcode/cwbcode.db"), "the project has no store"

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
        assert "answered: y" in page.text("#transcript"), "the logged approval is not shown as answered"
        assert page.js("document.querySelectorAll('.approve-yes').length") == 0, "an old approval can still be answered"

        print("5. the dashboard shows the backend")
        page.click_text(".nav button", "Dashboard")
        page.wait("document.querySelector('#protocol') && ['H2','H3'].includes(document.querySelector('#protocol').innerText)", "the protocol card")
        protocol = page.text("#protocol")
        assert page.text("#model-state") == "scripted", page.text("#model-state")
        assert "proj" in page.text("table"), "the project is not listed"
        page.shot(f"{shots}/4-dashboard.png")

        print("6. the knowledge base saves and finds a note")
        page.click_text(".nav button", "Knowledge")
        page.wait("!!document.querySelector('#kb-note')", "the knowledge page")
        page.type("#kb-title", "Deploy day"); page.type("#kb-note", "We deploy on Thursdays; Minsu owns it."); page.click("#kb-save")
        page.wait("document.querySelector('#kb-list').innerText.includes('Deploy day')", "the saved note")
        page.type("#kb-query", "when do we deploy"); page.click("#kb-search")
        page.wait("(document.querySelector('#kb-hits') || {}).innerText?.includes('Deploy day')", "the search hit")
        page.shot(f"{shots}/5-knowledge.png")

        print("7. the tools page, and a second message in a new conversation")
        page.click_text(".nav button", "Tools")
        page.wait("document.body.innerText.includes('No tools yet')", "the tools page")
        page.click_text(".nav button", "Chat"); page.click("#new-chat")
        page.type("#draft", "again"); page.click("#send")
        page.wait("document.querySelector('#transcript').innerText.includes('A second answer.')", "the second answer")
        assert not page.errors, f"the page reported errors: {page.errors}"
        print(f"All passed (the browser used {protocol}). Screenshots: {shots}")
    finally:
        chrome.terminate(); server.terminate()
        try: chrome.wait(5)
        except Exception: chrome.kill()
        server.wait(5)
        shutil.rmtree(home, ignore_errors=True)

if __name__ == "__main__":
    main()
