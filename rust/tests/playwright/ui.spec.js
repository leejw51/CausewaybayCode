// The web client's own features, through Playwright with the real model: projects under ~/
// that can be taken off the list, the typeface that changes the conversation only, the
// context bar, copying a conversation, plan mode, the user's own commands and hooks.
const { test, expect } = require('@playwright/test');
const fs = require('fs');
const os = require('os');
const path = require('path');

const run = () => JSON.parse(fs.readFileSync(path.join(__dirname, '.run.json'), 'utf8'));

/** Send a message and wait until the agent has finished its turn; returns its last answer. */
async function ask(page, text) {
  const before = await page.locator('.msg.text').count();
  await page.fill('#draft', text);
  await page.click('#send');
  await expect(page.locator('#stop')).toBeVisible({ timeout: 30_000 });
  await expect(page.locator('#send')).toBeVisible({ timeout: 10 * 60 * 1000 });
  await expect.poll(() => page.locator('.msg.text').count(), { timeout: 10_000 }).toBeGreaterThan(before);
  return page.locator('.msg.text').last().innerText();
}

test('projects, typefaces, the context bar, copying, plan mode, commands and hooks', async ({ page, context }) => {
  const { url, token, run: runDir } = run();
  const home = os.homedir();
  const folder = path.join(runDir, 'ui');
  const under = '~' + folder.slice(home.length);
  const pageErrors = [];
  page.on('pageerror', (e) => pageErrors.push(String(e)));
  await context.grantPermissions(['clipboard-read', 'clipboard-write']);

  await test.step('the model is ready', async () => {
    // Local mode: a link with the backend's token, as one types it (`make url` prints a
    // one-time code instead, which works once).
    await page.goto(`${url}/#token=${token}`);
    await expect(page.locator('.side')).toBeVisible();
    await expect(page.locator('#status-pill')).toContainText('ready', { timeout: 3 * 60 * 1000 });
  });

  await test.step('a project is opened under ~/, shown with ~, removed from the list, and opened again', async () => {
    await expect(page.locator('.open .prefix')).toHaveText('work/');
    // Typed with ~/: a folder under the home folder, made. (A bare name goes to ~/.cwbcode/work.)
    await page.fill('#open-path', under);
    await page.click('#open-go');
    await expect(page.locator('.row.project.on small')).toHaveText(under);
    expect(fs.existsSync(path.join(folder, '.cwbcode/cwbcode.db'))).toBe(true);
    // The home folder's own path is nowhere on the page.
    expect(await page.locator('body').innerText()).not.toContain(home);
    await page.locator('.row.project.on .remove').click();
    await expect(page.locator('.row.project', { hasText: under })).toHaveCount(0);
    await expect(page.locator('#notice')).toContainText(under);
    expect(fs.existsSync(path.join(folder, '.cwbcode/cwbcode.db')), 'removing from the list deleted the records').toBe(true);
    await page.fill('#open-path', under);
    await page.click('#open-go');
    await expect(page.locator('.row.project.on small')).toHaveText(under);
  });

  await test.step('the typeface changes the whole page; the size changes the messages only', async () => {
    const face = (selector) => page.locator(selector).evaluate((el) => getComputedStyle(el).fontFamily.split(',')[0].replaceAll('"', '').trim());
    const size = (selector) => page.locator(selector).evaluate((el) => parseFloat(getComputedStyle(el).fontSize));
    expect(await face('#draft')).toBe('VT323');
    await page.click('#settings');
    await page.selectOption('#font', 'geist');
    for (const part of ['#draft', '#nav-chat', '#send', '#undo', '.row.project.on']) expect(await face(part), part).toBe('Geist');
    expect(await page.evaluate(() => document.fonts.load('16px "Geist"', 'Az').then((f) => f.length))).toBeGreaterThan(0);
    const before = { draft: await size('#draft'), nav: await size('#nav-chat'), send: await size('#send'), row: await size('.row.project.on') };
    await page.click('#font-larger');
    await page.click('#font-larger');
    await expect(page.locator('html')).toHaveAttribute('data-size', 'xl');
    expect(await size('#draft')).toBeGreaterThan(before.draft * 1.2);
    expect({ nav: await size('#nav-chat'), send: await size('#send'), row: await size('.row.project.on') }).toEqual({ nav: before.nav, send: before.send, row: before.row });
    await page.click('#settings-close');
  });

  await test.step('plan mode: a plan comes back and nothing is changed', async () => {
    await page.selectOption('#mode', 'plan');
    const answer = await ask(page, 'Create a file named planned.txt containing the word tram.');
    expect(fs.existsSync(path.join(folder, 'planned.txt')), 'plan mode wrote the file').toBe(false);
    expect(answer.toLowerCase()).toMatch(/plan|step|1\./);
    // What was said is in the chosen typeface, at the larger size; the buttons are not larger.
    // Everything in the conversation is: what was asked, thought, and the tool calls.
    for (const part of ['.msg.user', '.msg.text', '.tool summary']) {
      if (await page.locator(part).count()) expect(await page.locator(part).last().evaluate((el) => getComputedStyle(el).fontFamily), part).toContain('Geist');
    }
    const answered = page.locator('.msg.text').last();
    expect(await answered.evaluate((el) => getComputedStyle(el).fontFamily)).toContain('Geist');
    expect(await answered.evaluate((el) => parseFloat(getComputedStyle(el).fontSize))).toBeGreaterThan(await page.locator('#send').evaluate((el) => parseFloat(getComputedStyle(el).fontSize)) * 1.2);
  });

  await test.step('the context bar says how full the conversation is', async () => {
    const text = await page.locator('#ctx-text').innerText();
    expect(text).toMatch(/^[0-9.]+k? \/ [0-9.]+k$/);
    const width = await page.locator('#ctx .bar2 i').evaluate((el) => el.getBoundingClientRect().width);
    expect(width).toBeGreaterThan(0);
  });

  await test.step('the work is done once the mode allows it', async () => {
    await page.selectOption('#mode', 'auto');
    await ask(page, 'Go ahead: create planned.txt containing the word tram.');
    expect(fs.readFileSync(path.join(folder, 'planned.txt'), 'utf8')).toContain('tram');
  });

  await test.step('the whole conversation is copied', async () => {
    await page.click('#copy-all');
    await expect(page.locator('#notice')).toContainText('copied');
    const copied = await page.evaluate(() => navigator.clipboard.readText());
    expect(copied).toContain('> Create a file named planned.txt containing the word tram.');
    expect(copied).toContain('write_file(planned.txt)');
    // The outline has a bar for each thing said or done.
    expect(await page.locator('#outline .o').count()).toBe(await page.locator('#transcript .it').count());
  });

  await test.step('Run runs the code, the console shows it, Clear clears', async () => {
    fs.writeFileSync(path.join(folder, 'tide.rs'), 'fn main() { println!("high tide at {}", 6 * 7); }\n');
    // (A command typed in the console is run as it is; with none, Run asks the model.)
    await page.click('#console-toggle');
    await page.fill('#console-cmd', 'rustc -O tide.rs -o "$TMPDIR/tide" && "$TMPDIR/tide"');
    await page.click('#run');
    await expect(page.locator('#console-out')).toContainText('high tide at 42', { timeout: 120_000 });
    await expect(page.locator('#console-out')).toContainText('[done]');
    await expect(page.locator('#console-out')).toContainText('rustc -O');
    await page.fill('#console-cmd', 'cat planned.txt');
    await page.keyboard.press('Enter');
    await expect(page.locator('#console-out')).toContainText('$ cat planned.txt');
    await page.click('#console-toggle');
    await expect(page.locator('#console')).toHaveCount(0);
    await page.click('#console-toggle');
    await expect(page.locator('#console-out')).toContainText('high tide at 42');
    await page.click('#clear');
    await expect(page.locator('#transcript .it')).toHaveCount(0);
    await expect(page.locator('#console-out')).not.toContainText('high tide');
    await page.click('#console-close');
  });

  await test.step("a command of the user's own is sent as the text of its file", async () => {
    fs.mkdirSync(path.join(folder, '.cwbcode/commands'), { recursive: true });
    fs.writeFileSync(path.join(folder, '.cwbcode/commands/stamp.md'), 'Create a file named $ARGUMENTS whose only content is the word STAMPED. Then say done.');
    await page.click('#new-chat');
    await ask(page, '/stamp stamped.txt');
    expect(fs.readFileSync(path.join(folder, 'stamped.txt'), 'utf8')).toContain('STAMPED');
  });

  await test.step("the user's hooks run around the agent's tools", async () => {
    fs.writeFileSync(path.join(runDir, 'server/hooks.json'), JSON.stringify({
      before: [{ tool: 'bash', run: "if grep -q harbourmaster; then echo 'the harbourmaster says no'; exit 1; fi" }],
      // (The model may write the file with either tool: the hook follows both.)
      after: [{ tool: 'write_file|edit_file|bash', run: 'echo touched > hook-ran.txt' }],
    }));
    await page.click('#new-chat');
    await ask(page, 'Run exactly this command with your bash tool and tell me what happened: echo harbourmaster');
    await page.locator('.tool summary', { hasText: 'bash' }).last().click();
    await expect(page.locator('.tool pre').last()).toContainText('the harbourmaster says no');
    // (In a new conversation: straight after a refusal the model takes a request to write a
    // file for a way around the hook, and declines.)
    await page.click('#new-chat');
    await ask(page, 'Create a file named tram-note.txt containing the word ok.');
    expect(fs.readFileSync(path.join(folder, 'tram-note.txt'), 'utf8')).toContain('ok');
    expect(fs.readFileSync(path.join(folder, 'hook-ran.txt'), 'utf8')).toContain('touched');
    fs.unlinkSync(path.join(runDir, 'server/hooks.json'));
  });

  expect(pageErrors, `the page raised errors: ${pageErrors.join(' | ')}`).toEqual([]);
});
