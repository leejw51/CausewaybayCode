const fs = require('fs');
const path = require('path');

module.exports = async () => {
  const file = path.join(__dirname, '.run.json');
  if (!fs.existsSync(file)) return;
  const { pid } = JSON.parse(fs.readFileSync(file, 'utf8'));
  try { process.kill(pid, 'SIGTERM'); } catch (_) { /* already gone */ }
  // Give it a moment to stop what it is doing and exit.
  for (let i = 0; i < 100; i++) {
    try { process.kill(pid, 0); } catch (_) { break; }
    await new Promise((r) => setTimeout(r, 100));
  }
  fs.unlinkSync(file);
};
