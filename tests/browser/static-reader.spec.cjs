const { test, expect } = require("@playwright/test");

test("filters, source choices, article return, and history work without a backend", async ({ page }) => {
  const requests = [];
  page.on("request", (request) => requests.push(request.url()));
  await page.goto("/");
  await expect(page.locator(".publication:visible")).toHaveCount(1);
  const subscription = await page.getByRole("link", { name: "Subscribe to this feed" }).getAttribute("href");
  await page.getByRole("combobox", { name: "Author selection", exact: true }).selectOption("all");
  await expect(page.locator(".publication:visible")).toHaveCount(2);
  await page.getByText("Paper sources", { exact: true }).click();
  await page.getByLabel("Core", { exact: true }).uncheck();
  await expect(page.locator(".publication:visible")).toHaveCount(1);
  await expect(page.getByRole("link", { name: "Subscribe to this feed" })).toHaveAttribute("href", subscription);
  await page.locator(".publication:visible h2 a").click();
  await expect(page.locator(".abstract")).toBeVisible();
  await page.reload();
  await page.getByRole("link", { name: "Back to publications" }).click();
  await expect(page.getByRole("combobox", { name: "Author selection", exact: true })).toHaveValue("all");
  await expect(page.locator(".publication:visible")).toHaveCount(1);
  await page.getByRole("button", { name: "Clear filters" }).click();
  await expect(page.getByRole("combobox", { name: "Author selection", exact: true })).toHaveValue("default");
  await page.goBack();
  await expect(page.getByRole("combobox", { name: "Author selection", exact: true })).toHaveValue("all");
  expect(requests.every((url) => new URL(url).origin === "http://127.0.0.1:8080")).toBe(true);
});

test("explicit empty author selection and malformed URLs are recoverable", async ({ page }) => {
  await page.goto("/?authors=custom");
  await expect(page.locator(".publication:visible")).toHaveCount(0);
  await expect(page.locator("[data-filter-empty]")).toBeVisible();
  await page.goto("/?source=missing");
  await expect(page.getByRole("alert")).toContainText("Unknown source");
  await page.getByRole("button", { name: "Clear filters" }).click();
  await expect(page.getByRole("alert")).toBeHidden();
  await expect(page.locator(".publication:visible")).toHaveCount(1);
});

test("mobile layout and keyboard filter access remain usable", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/");
  await expect(page.locator("[data-static-filters]")).toBeVisible();
  const date = page.getByRole("combobox", { name: "Publication date", exact: true });
  await date.focus();
  await expect(date).toBeFocused();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
});

test("no-JavaScript listing, icons, RSS, and article pages remain usable", async ({ browser }) => {
  const context = await browser.newContext({ javaScriptEnabled: false, baseURL: "http://127.0.0.1:8080" });
  const page = await context.newPage();
  await page.goto("http://127.0.0.1:8080/");
  await expect(page.locator(".publication:visible")).toHaveCount(1);
  await expect(page.locator("[data-static-filters]")).toBeHidden();
  for (const selector of ['link[rel="icon"]', 'link[rel="apple-touch-icon"]', 'link[rel="alternate"]']) {
    const response = await page.request.get(await page.locator(selector).getAttribute("href"));
    expect(response.ok()).toBe(true);
  }
  await page.locator(".publication:visible h2 a").click();
  await expect(page.locator(".abstract")).toBeVisible();
  await context.close();
});
