// Can the agent write Rust? Through the web client, with the real model: it is asked to
// create a crate with a function and tests, then to extend it. What it wrote is then judged
// from outside: the crate must build, its own tests must pass, and tests it has never seen
// (written here) must pass against its code.
const { test, expect } = require('@playwright/test');
const { execSync } = require('child_process');
const fs = require('fs');
const path = require('path');

const run = () => JSON.parse(fs.readFileSync(path.join(__dirname, '.run.json'), 'utf8'));

/** Run a command in the project; returns its output, or throws with it. */
function sh(command, cwd) {
  try {
    return execSync(command, { cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], timeout: 5 * 60 * 1000 });
  } catch (e) {
    throw new Error(`\`${command}\` failed:\n${e.stdout || ''}\n${e.stderr || ''}`);
  }
}

/** Send a message and wait until the agent has finished its turn. */
async function ask(page, text) {
  const before = await page.locator('.msg.text').count();
  await page.fill('#draft', text);
  await page.click('#send');
  await expect(page.locator('#stop')).toBeVisible({ timeout: 30_000 });
  // The turn is over when the Send button is back; then there is a new answer.
  await expect(page.locator('#send')).toBeVisible({ timeout: 12 * 60 * 1000 });
  await expect.poll(() => page.locator('.msg.text').count(), { timeout: 10_000 }).toBeGreaterThan(before);
  const errors = await page.locator('.note.bad').allInnerTexts();
  expect(errors, `the turn reported errors: ${errors.join(' | ')}`).toEqual([]);
}

test('the agent writes a Rust crate that builds, passes its own tests and passes ours', async ({ page }) => {
  const { url, token, project, run: runDir } = run();
  const pageErrors = [];
  page.on('pageerror', (e) => pageErrors.push(String(e)));

  await test.step('sign in and wait for the model', async () => {
    await page.goto(`${url}/#token=${token}`);
    await expect(page.locator('.side')).toBeVisible();
    expect(page.url(), 'the token must not stay in the address').not.toContain('token');
    await expect(page.locator('#status-pill')).toContainText('ready', { timeout: 3 * 60 * 1000 });
  });

  await test.step('open the project, let the agent work without asking', async () => {
    await page.fill('#open-path', project);
    await page.click('#open-go');
    await expect(page.locator('.row.project.on')).toBeVisible();
    await page.selectOption('#mode', 'auto');
  });

  await test.step('ask for a crate with a function and tests', async () => {
    await ask(page,
      'Create a Rust library crate in this folder: run `cargo init --lib --name wordfreq .` first. ' +
      'In src/lib.rs write `pub fn word_freq(text: &str) -> Vec<(String, usize)>`: split the text on whitespace, ' +
      'lowercase each word, strip leading and trailing punctuation, ignore empty words, and return each word with its count, ' +
      'sorted by count descending and then alphabetically. Add at least three unit tests. ' +
      'Run `cargo test` and fix anything that fails before you answer.');
    await page.screenshot({ path: path.join(runDir, '1-crate.png') });
  });

  await test.step('what it wrote builds, and its own tests pass', async () => {
    expect(fs.existsSync(path.join(project, 'Cargo.toml')), 'no Cargo.toml').toBe(true);
    const lib = fs.readFileSync(path.join(project, 'src/lib.rs'), 'utf8');
    expect(lib).toMatch(/pub fn word_freq\s*\(\s*text\s*:\s*&str\s*\)\s*->\s*Vec<\(String,\s*usize\)>/);
    expect((lib.match(/#\[test\]/g) || []).length, 'fewer than three tests').toBeGreaterThanOrEqual(3);
    const out = sh('cargo test 2>&1', project);
    expect(out).toMatch(/test result: ok\. [3-9]\d* passed; 0 failed/);
    // Clean under the compiler's own lints, too.
    sh('cargo build 2>&1', project);
  });

  await test.step('tests it has never seen pass against its code', async () => {
    fs.mkdirSync(path.join(project, 'tests'), { recursive: true });
    fs.writeFileSync(path.join(project, 'tests/judge.rs'), `
use wordfreq::word_freq;

fn pairs(v: &[(&str, usize)]) -> Vec<(String, usize)> { v.iter().map(|(w, n)| (w.to_string(), *n)).collect() }

#[test]
fn counts_case_insensitively_and_sorts_by_count_then_word() {
    assert_eq!(word_freq("b a B c a a"), pairs(&[("a", 3), ("b", 2), ("c", 1)]));
}

#[test]
fn ties_are_alphabetical() {
    assert_eq!(word_freq("pear apple fig"), pairs(&[("apple", 1), ("fig", 1), ("pear", 1)]));
}

#[test]
fn punctuation_around_words_is_dropped_and_empty_text_is_empty() {
    assert_eq!(word_freq("Hello, world! (hello) -- world?"), pairs(&[("hello", 2), ("world", 2)]));
    assert!(word_freq("").is_empty());
    assert!(word_freq("  \\n\\t ... ").is_empty());
}
`);
    const out = sh('cargo test --test judge 2>&1', project);
    expect(out).toMatch(/test result: ok\. 3 passed; 0 failed/);
  });

  await test.step('ask it to extend its own code, and judge that too', async () => {
    await ask(page,
      'Now add `pub fn top_n(text: &str, n: usize) -> Vec<String>` to src/lib.rs: the n most frequent words, in the same order ' +
      'as word_freq (fewer if the text has fewer words). Use word_freq. Add a unit test for it and run `cargo test` again.');
    fs.writeFileSync(path.join(project, 'tests/judge_top.rs'), `
use wordfreq::top_n;

#[test]
fn the_most_frequent_words_come_first() {
    assert_eq!(top_n("b a B c a a", 2), vec!["a".to_string(), "b".to_string()]);
    assert_eq!(top_n("b a B c a a", 10), vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    assert!(top_n("anything at all", 0).is_empty());
}
`);
    const out = sh('cargo test 2>&1', project);
    expect(out).toMatch(/Running tests\/judge_top\.rs[\s\S]*?test result: ok\. 1 passed; 0 failed/);
    expect(out).not.toMatch(/test result: FAILED/);
    await page.screenshot({ path: path.join(runDir, '2-extended.png') });
  });

  await test.step('the page shows the work and kept it', async () => {
    // Tool calls are on the page, the conversation is in the sidebar, the dashboard has the project.
    expect(await page.locator('.tool').count()).toBeGreaterThanOrEqual(3);
    await expect(page.locator('.row.session')).toHaveCount(1);
    await page.getByRole('button', { name: 'Dashboard' }).click();
    await expect(page.locator('table')).toContainText('wordfreq');
    await expect(page.locator('#model-state')).toHaveText('ready');
    // Everything it did is in the project's own store.
    expect(fs.existsSync(path.join(project, '.cwbcode/cwbcode.db'))).toBe(true);
    expect(pageErrors, `the page threw: ${pageErrors.join(' | ')}`).toEqual([]);
    console.log(`project: ${project}\nscreenshots and backend log: ${runDir}`);
  });
});
