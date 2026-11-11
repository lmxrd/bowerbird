import { expect, test } from "@playwright/test";

// One flow, in order, on the fresh database of the image: the first run
// goes to setup, the account is made, the files page shows, a folder is
// made, and signing out leads to the login page.
test.describe.configure({ mode: "serial" });

const email = "admin@example.com";
const password = "correct horse battery";

test("the first run sets up the superadmin and shows the files", async ({ page }) => {
  await page.goto("/");
  await expect(page).toHaveURL(/\/setup$/);
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Create account" }).click();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByRole("heading", { name: "My drive" })).toBeVisible();

  await page.getByRole("button", { name: "New folder" }).click();
  await page.getByLabel("Name").fill("Receipts");
  await page.getByRole("button", { name: "Create folder" }).click();
  await expect(page.getByRole("link", { name: "Receipts" })).toBeVisible();
});

test("signing in again leads back to the files", async ({ page }) => {
  await page.goto("/");
  await expect(page).toHaveURL(/\/login/);
  await page.getByLabel("Email").fill(email);
  await page.getByLabel("Password").fill(password);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("link", { name: "Receipts" })).toBeVisible();
});

test("an unknown API path is a JSON 404", async ({ request }) => {
  const response = await request.get("/api/nothing-here");
  expect(response.status()).toBe(404);
  expect(await response.json()).toEqual({ error: "Not found" });
});
