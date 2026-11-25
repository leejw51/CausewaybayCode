// Causewaybay Code's web client, driven by Playwright in the installed Google Chrome.
// The backend is started by global-setup.js with the real model, so this takes minutes
// and needs about 22 GB of free memory.
const { defineConfig } = require('@playwright/test');

module.exports = defineConfig({
  testDir: '.',
  timeout: 20 * 60 * 1000,
  workers: 1,
  retries: 0,
  reporter: [['list']],
  globalSetup: require.resolve('./global-setup.js'),
  globalTeardown: require.resolve('./global-teardown.js'),
  use: {
    channel: 'chrome',
    headless: true,
    ignoreHTTPSErrors: true, // the backend's certificate is self-signed
    viewport: { width: 1440, height: 900 },
    screenshot: 'only-on-failure',
  },
});
