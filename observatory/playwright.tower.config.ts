import { defineConfig } from '@playwright/test';

export default defineConfig({
	webServer: {
		command: 'bun run build:tower && cargo build -p control-tower && node tests/tower-server.mjs',
		url: 'http://127.0.0.1:4180/tower',
		timeout: 120000
	},
	use: { baseURL: 'http://127.0.0.1:4180' },
	testMatch: '**/tower.e2e.ts'
});
