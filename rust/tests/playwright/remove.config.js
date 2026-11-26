// Removing a conversation in the web client, driven by Playwright in Chromium and in WebKit
// (the engine of Safari and of the Mac app's window). No model: remove.spec.js starts a
// backend of its own with a scripted one, in a scratch folder. `make build` first.
//   npx playwright test -c remove.config.js
const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: '.',
  testMatch: 'remove.spec.js',
  timeout: 2 * 60 * 1000,
  workers: 1,
  retries: 0,
  reporter: [['list']],
  use: {
    headless: true,
    ignoreHTTPSErrors: true, // the backend's certificate is its own
    viewport: { width: 1440, height: 900 },
    locale: 'en-US', // the interface follows the browser's language
    screenshot: 'only-on-failure',
  },
  projects: [
    { name: 'chromium', use: { browserName: 'chromium' } },
    { name: 'webkit', use: { browserName: 'webkit' } },
  ],
});
