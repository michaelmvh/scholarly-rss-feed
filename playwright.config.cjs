const { defineConfig } = require("@playwright/test");
module.exports = defineConfig({
  testDir: "./tests/browser",
  use: { baseURL: "http://127.0.0.1:8080" },
  webServer: {
    command: "node tests/serve-static.mjs",
    url: "http://127.0.0.1:8080",
    reuseExistingServer: false,
  },
});
