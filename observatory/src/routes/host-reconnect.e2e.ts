import { expect, test, type WebSocketRoute } from '@playwright/test';

test('reconnects after qbt restarts and stops retrying when removed', async ({ page }) => {
	const connections: WebSocketRoute[] = [];
	const screenshotFetches: number[] = [];
	await page.routeWebSocket('ws://restart.test:5678/', (socket) => {
		connections.push(socket);
		const connectionNumber = connections.length;
		const firstConnection = connectionNumber === 1;
		socket.onMessage((message) => {
			if (message === JSON.stringify({ fetchScreenshot: 'shot_1' })) {
				screenshotFetches.push(connectionNumber);
				const png = Buffer.from(
					firstConnection
						? 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII='
						: 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR42mP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC',
					'base64'
				);
				socket.send(Buffer.concat([Buffer.from('shot_1\n'), png]));
			}
		});
		setTimeout(() => {
			socket.send(
				JSON.stringify({
					seq: 1,
					atMs: Date.now(),
					kind: 'computer.action',
					payload: {
						request: { action: firstConnection ? 'before_restart' : 'after_restart' }
					},
					screenshotId: 'shot_1'
				})
			);
		}, 0);
	});
	await page.addInitScript(() => {
		localStorage.setItem('hosts', JSON.stringify(['restart.test:5678']));
	});

	await page.goto('/');
	const chit = page.locator('.chit');
	await expect(chit.getByText('connected', { exact: true })).toBeVisible();
	await expect(chit.getByText(/before_restart/)).toBeVisible();
	await expect(chit.locator('img.front')).toBeVisible();
	await expect.poll(() => screenshotFetches).toEqual([1]);
	const firstScreenshotUrl = await chit.locator('img.front').getAttribute('src');
	await chit.locator('.chit-actions button').nth(1).click();
	await page.locator('.event').click();
	await expect(page.locator('.caption')).toContainText('pinned');

	await connections[0].close();
	await expect.poll(() => connections.length).toBe(2);
	await expect(page.locator('.event')).toContainText('after_restart');
	await expect(page.getByText(/before_restart/)).toHaveCount(0);
	await expect(page.locator('.caption')).not.toContainText('pinned');
	await expect.poll(() => screenshotFetches).toEqual([1, 2]);
	await expect(page.locator('img.front')).not.toHaveAttribute('src', firstScreenshotUrl!);

	await page.getByRole('button', { name: '← Back' }).click();
	await connections[1].close();
	await expect(chit.getByText('disconnected', { exact: true })).toBeVisible();
	await chit.locator('.chit-actions button').last().click();
	await page.waitForTimeout(1200);
	expect(connections).toHaveLength(2);
});
