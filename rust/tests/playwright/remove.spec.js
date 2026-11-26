// A conversation is removed from the web client's sidebar, as a person does it: the mouse on
// its ×, then on Yes. Run by remove.config.js in Chromium and WebKit, against a backend with a
// scripted model in a scratch folder (nothing of ~/.cwbcode is touched).
const { test, expect } = require('@playwright/test');
const { spawn } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const net = require('net');

const freePort = () => new Promise((resolve) => {
  const server = net.createServer();
  server.listen(0, '127.0.0.1', () => { const { port } = server.address(); server.close(() => resolve(port)); });
});

let backend, url, token, home, project;

test.beforeAll(async () => {
  const binary = path.resolve(__dirname, '../../bin/cwbcode');
  if (!fs.existsSync(binary)) throw new Error(`${binary} is missing: run \`make build\` first`);
  // Projects live under the home folder: the backend is given a scratch one.
  home = fs.mkdtempSync(path.join(os.tmpdir(), 'cwbcode-remove-'));
  home = fs.realpathSync(home);
  project = path.join(home, 'proj');
  fs.mkdirSync(project);
  const script = path.join(home, 'script.json');
  // Enough answers for both browsers.
  fs.writeFileSync(script, JSON.stringify(Array.from({ length: 12 }, (_, i) => ({ text: `Answer ${i + 1}.` }))));
  const port = await freePort();
  const log = fs.openSync(path.join(home, 'server.log'), 'w');
  backend = spawn(binary, ['serve', '--env-file', '/dev/null', '--bind', `127.0.0.1:${port}`, '--no-h3', '--script', script],
    { env: { ...process.env, HOME: home, CWBCODE_HOME: path.join(home, '.cwbcode'), CWBCODE_WALLET: '' }, stdio: ['ignore', log, log] });
  const tokenFile = path.join(home, '.cwbcode/token');
  for (let i = 0; i < 200 && !fs.existsSync(tokenFile); i++) await new Promise((r) => setTimeout(r, 100));
  if (!fs.existsSync(tokenFile)) throw new Error(`the backend did not start: ${fs.readFileSync(path.join(home, 'server.log'), 'utf8')}`);
  token = fs.readFileSync(tokenFile, 'utf8').trim();
  url = `https://127.0.0.1:${port}`;
});

test.afterAll(async () => {
  if (backend) backend.kill();
});

test('a conversation is removed with its ×, after Yes', async ({ page }, info) => {
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  page.on('dialog', (dialog) => { errors.push(`a browser dialog was shown: ${dialog.message()}`); dialog.dismiss(); });

  await page.goto(`${url}/#token=${token}`);
  await expect(page.locator('.side')).toBeVisible();
  await page.locator('#open-path').fill('~/proj');
  await page.locator('#open-go').click();
  await expect(page.locator('.row.project.on')).toBeVisible();

  // Two conversations, each with one message and its answer.
  const say = async (text) => {
    await page.locator('#draft').fill(text);
    await page.locator('#send').click();
    await expect(page.locator('#transcript')).toContainText(/Answer \d+\./);
    await expect(page.locator('#send')).toBeVisible();
  };
  await page.locator('#new-chat').click();
  await say(`first of ${info.project.name}`);
  await expect(page.locator('.row.session')).not.toHaveCount(0);
  const before = await page.locator('.row.session').count();
  await page.locator('#new-chat').click();
  await say(`second of ${info.project.name}`);
  await expect(page.locator('.row.session')).toHaveCount(before + 1);
  const rows = before + 1;

  // The one that is not open: × asks on the row; No keeps it.
  // (Rows are found by their number, so that a click that opens the wrong one is noticed.)
  const numberOf = async (row) => (await row.locator('small').innerText()).split(' ')[0];
  const number = await numberOf(page.locator('.row.session:not(.on)').first());
  const opened = await numberOf(page.locator('.row.session.on'));
  expect(number).not.toBe(opened);
  const other = page.locator('.row.session', { has: page.locator('small', { hasText: `${number} ` }) });
  const openRow = page.locator('.row.session', { has: page.locator('small', { hasText: `${opened} ` }) });
  await other.locator('.remove').click();
  await expect(other.locator('.remove-yes')).toBeVisible();
  await expect(other.locator('.remove-no')).toBeVisible();
  // Neither the × nor No opens the conversation they are on.
  await expect(openRow).toHaveClass(/\bon\b/);
  await other.locator('.remove-no').click();
  await expect(page.locator('.remove-yes')).toHaveCount(0);
  await expect(page.locator('.row.session')).toHaveCount(rows);
  await expect(openRow).toHaveClass(/\bon\b/);
  await expect(other).not.toHaveClass(/\bon\b/);

  // Yes removes it, and only it: gone from the list, said so, the open one still open, no
  // error shown, and still gone after a reload.
  await other.locator('.remove').click();
  await other.locator('.remove-yes').click();
  await expect(page.locator('.row.session')).toHaveCount(rows - 1);
  await expect(page.locator('#notice')).toContainText('is removed');
  await expect(openRow).toHaveClass(/\bon\b/);
  await expect(page.locator('#toast')).toHaveCount(0);
  await expect(page.locator('.row.session small', { hasText: `${number} ` })).toHaveCount(0);
  await page.reload();
  await expect(page.locator('.side')).toBeVisible();
  await expect(page.locator('.row.session')).toHaveCount(rows - 1);
  await expect(page.locator('.row.session small', { hasText: `${number} ` })).toHaveCount(0);

  // The open one: removed, and an empty new conversation is shown.
  await page.locator('.row.session').first().click();
  const open = page.locator('.row.session.on');
  await expect(open).toBeVisible();
  await open.locator('.remove').click();
  await open.locator('.remove-yes').click();
  await expect(page.locator('.row.session')).toHaveCount(rows - 2);
  await expect(page.locator('#transcript')).toContainText('What shall we work on?');

  // The stacked layout too (the sidebar as a strip of chips): the × is there and works.
  await say(`third of ${info.project.name}`);
  await expect(page.locator('.row.session')).toHaveCount(rows - 1);
  await page.locator('#settings').click();
  await page.locator('#layout-toggle').click();
  await expect(page.locator('.app.vertical')).toBeVisible();
  await page.keyboard.press('Escape');
  const chip = page.locator('.row.session').first();
  await chip.locator('.remove').click();
  await chip.locator('.remove-yes').click();
  await expect(page.locator('.row.session')).toHaveCount(rows - 2);
  await page.screenshot({ path: path.join(home, `removed-${info.project.name}.png`) });

  expect(errors).toEqual([]);
});
