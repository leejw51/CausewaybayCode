// Starts the backend (./bin/cwbcode, built by `make build`) with the real model, on a port
// and with a store of its own, so that ~/.cwbcode is not touched. The project the agent
// works in is a new folder under ~/.cache (projects must be under the home folder, and
// outside this repository so that cargo does not take it for a member of this workspace).
const { spawn, execSync } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const net = require('net');

function freePort() {
  return new Promise((resolve) => {
    const server = net.createServer();
    server.listen(0, '127.0.0.1', () => { const { port } = server.address(); server.close(() => resolve(port)); });
  });
}

module.exports = async () => {
  const root = path.resolve(__dirname, '../..');
  const binary = path.join(root, 'bin/cwbcode');
  if (!fs.existsSync(binary)) throw new Error(`${binary} is missing: run \`make build\` first`);
  try {
    execSync('pgrep -f "cwbcode serve"', { stdio: 'ignore' });
    throw new Error('a cwbcode backend is already running; stop it first (make stop): two copies of the model do not fit in memory');
  } catch (e) { if (e.message.includes('already running')) throw e; }

  const run = fs.mkdtempSync(path.join(fs.mkdirSync(path.join(os.homedir(), '.cache/cwbcode-playwright'), { recursive: true }) || path.join(os.homedir(), '.cache/cwbcode-playwright'), 'run-'));
  const project = path.join(run, 'wordfreq');
  fs.mkdirSync(project);
  const home = path.join(run, 'server');
  const port = await freePort();
  const log = fs.openSync(path.join(run, 'server.log'), 'w');
  const server = spawn(binary, ['serve', '--env-file', '/dev/null', '--bind', `127.0.0.1:${port}`], { env: { ...process.env, CWBCODE_HOME: home, CWBCODE_WALLET: '' }, stdio: ['ignore', log, log], detached: false });

  // The token appears once the backend has started.
  const tokenFile = path.join(home, 'token');
  for (let i = 0; i < 200 && !fs.existsSync(tokenFile); i++) await new Promise((r) => setTimeout(r, 100));
  if (!fs.existsSync(tokenFile)) { server.kill(); throw new Error(`the backend did not start: ${fs.readFileSync(path.join(run, 'server.log'), 'utf8')}`); }

  fs.writeFileSync(path.join(__dirname, '.run.json'), JSON.stringify({ pid: server.pid, url: `https://127.0.0.1:${port}`, token: fs.readFileSync(tokenFile, 'utf8').trim(), project, run }));
};
