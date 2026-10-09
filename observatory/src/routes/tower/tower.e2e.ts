import { expect, test } from '@playwright/test';

test('launches through the real controller and relays transcript, capture and screenshots', async ({
	page
}) => {
	const sockets: string[] = [];
	page.on('websocket', (socket) => sockets.push(socket.url()));
	await page.goto('/');
	await expect(page).toHaveURL(/\/tower$/);
	await expect(page.getByRole('heading', { name: 'Control Tower', exact: true })).toBeVisible();
	await page.getByText('Launch a job', { exact: true }).click();
	await page.getByLabel('Title', { exact: true }).fill('Browser QA');
	await page.getByLabel('Prompt', { exact: true }).fill('Inspect the checkout');
	await page.getByRole('button', { name: 'Launch job', exact: true }).click();
	const card = page.locator('article').filter({ hasText: 'Browser QA' });
	await expect(card.getByText('running', { exact: true })).toBeVisible({ timeout: 15000 });
	await expect(card.locator('img.front')).toBeVisible();
	expect(sockets.length).toBeGreaterThan(0);
	expect(sockets.every((url) => url.startsWith('ws://127.0.0.1:4180/api/jobs/'))).toBe(true);
	await card.getByTitle('Open session').click();
	await page.getByRole('button', { name: 'Driver transcript' }).click();
	await expect(page.locator('.event')).toContainText('Hello from the relayed worker');
	await page.getByTitle('Take screenshot').click();
	await page.getByRole('button', { name: 'Computer actions', exact: true }).click();
	await expect(page.getByRole('button', { name: /×2/ })).toBeVisible();
	await page.getByRole('button', { name: '← Back' }).click();
	page.once('dialog', (dialog) => void dialog.accept());
	await card.getByRole('button', { name: 'Cancel', exact: true }).click();
	await expect(page.locator('article')).toHaveCount(0, { timeout: 15000 });
	const history = page.locator('.history');
	await expect(history).toContainText('cancelled');
	await history.locator('summary').click();
	await expect(history).toContainText('Inspect the checkout');
});
