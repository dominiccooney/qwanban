import http from 'node:http';
import net from 'node:net';
import crypto from 'node:crypto';
import { mkdtemp, writeFile, rm, cp } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { spawn } from 'node:child_process';

const root = await mkdtemp(path.join(os.tmpdir(), 'tower-browser-'));
await cp(path.resolve('build'), path.join(root, 'ui'), { recursive: true });
const instances = new Map();
const rejected = new Set();
let nextId = 1;
const cloud = http.createServer(async (request, response) => {
	let text = '';
	for await (const chunk of request) text += chunk;
	const url = new URL(request.url, 'http://localhost');
	response.setHeader('content-type', 'application/json');
	if (url.pathname === '/token')
		return response.end(
			JSON.stringify({ access_token: 'fixture', token_type: 'Bearer', expires_in: 3600 })
		);
	if (url.pathname === '/api/containers') {
		response.statusCode = 201;
		return response.end('{}');
	}
	if (url.pathname.startsWith('/api/containers/')) {
		response.statusCode = 204;
		return response.end();
	}
	if (
		!url.pathname.startsWith('/compute/v1/') ||
		request.headers.authorization !== 'Bearer fixture'
	) {
		response.statusCode = 401;
		return response.end(
			JSON.stringify({ error: { code: 401, message: 'expected SDK path and ADC token' } })
		);
	}
	if (url.pathname.includes('/operations/'))
		return response.end(JSON.stringify({ name: url.pathname.split('/').at(-1), status: 'DONE' }));
	if (request.method === 'POST') {
		const body = JSON.parse(text);
		// Reject one real SDK insertion per selected job, then allow an
		// operator retry of the same insertion UUID to succeed.
		if (
			body.metadata?.items?.some(
				(item) => item.key === 'control-tower-prompt' && item.value === 'Test insufficient scopes'
			) &&
			!rejected.has(body.name)
		) {
			rejected.add(body.name);
			response.statusCode = 403;
			return response.end(
				JSON.stringify({
					error: {
						code: 403,
						message: 'Request had insufficient authentication scopes.',
						status: 'PERMISSION_DENIED'
					}
				})
			);
		}
		const instance = {
			id: String(nextId++),
			name: body.name,
			labels: body.labels,
			status: 'RUNNING',
			networkInterfaces: [{ networkIP: '127.0.0.1' }]
		};
		instances.set(instance.id, instance);
		return response.end(JSON.stringify({ name: `insert-${instance.id}`, status: 'DONE' }));
	}
	const identity = url.pathname.split('/').at(-1);
	const instance =
		instances.get(identity) ?? [...instances.values()].find((i) => i.name === identity);
	if (!instance) {
		response.statusCode = 404;
		return response.end('{}');
	}
	if (request.method === 'DELETE') {
		instances.delete(instance.id);
		return response.end(JSON.stringify({ name: `delete-${instance.id}`, status: 'DONE' }));
	}
	response.end(JSON.stringify(instance));
});
await new Promise((resolve) => cloud.listen(0, '127.0.0.1', resolve));
const cloudPort = cloud.address().port;

// A minimal wire fixture, not a reimplementation of the relay. The Rust
// service performs the browser upgrade and both forwarding directions.
const png = Buffer.from(
	'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=',
	'base64'
);
function frame(opcode, payload) {
	const bytes = Buffer.isBuffer(payload) ? payload : Buffer.from(payload);
	if (bytes.length < 126) return Buffer.concat([Buffer.from([0x80 | opcode, bytes.length]), bytes]);
	const header = Buffer.alloc(4);
	header[0] = 0x80 | opcode;
	header[1] = 126;
	header.writeUInt16BE(bytes.length, 2);
	return Buffer.concat([header, bytes]);
}
const observatory = http.createServer();
observatory.on('upgrade', (request, socket) => {
	const accept = crypto
		.createHash('sha1')
		.update(request.headers['sec-websocket-key'] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11')
		.digest('base64');
	socket.write(
		`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`
	);
	let seq = 1;
	function event(kind, payload, screenshotId) {
		socket.write(
			frame(1, JSON.stringify({ seq: seq++, atMs: Date.now(), kind, payload, screenshotId }))
		);
	}
	event('transcript.message_committed', {
		source: { kind: 'driver' },
		payload: { role: 'assistant', text: 'Hello from the relayed worker' }
	});
	event('observer.screenshot', {}, 'shot_1');
	let buffered = Buffer.alloc(0);
	socket.on('data', (chunk) => {
		buffered = Buffer.concat([buffered, chunk]);
		while (buffered.length >= 6) {
			const opcode = buffered[0] & 0x0f;
			let length = buffered[1] & 0x7f;
			let offset = 2;
			if (length === 126) {
				if (buffered.length < 8) return;
				length = buffered.readUInt16BE(2);
				offset = 4;
			}
			if (length === 127) return socket.destroy();
			const masked = !!(buffered[1] & 0x80);
			const start = offset + (masked ? 4 : 0);
			if (buffered.length < start + length) return;
			const payload = Buffer.from(buffered.subarray(start, start + length));
			if (masked) for (let i = 0; i < payload.length; i++) payload[i] ^= buffered[offset + (i % 4)];
			buffered = buffered.subarray(start + length);
			if (opcode === 8) return socket.end();
			if (opcode !== 1) continue;
			const command = JSON.parse(payload.toString());
			if (command.fetchScreenshot)
				socket.write(frame(2, Buffer.concat([Buffer.from(`${command.fetchScreenshot}\n`), png])));
			if (command === 'takeScreenshot') event('observer.screenshot', {}, `shot_${seq}`);
		}
	});
	socket.on('error', () => {});
});
await new Promise((resolve) => observatory.listen(0, '127.0.0.1', resolve));
const reserve = net.createServer();
await new Promise((resolve) => reserve.listen(0, '127.0.0.1', resolve));
const workerPort = reserve.address().port;
await new Promise((resolve) => reserve.close(resolve));
const configPath = path.join(root, 'config.yaml');
const credentialsPath = path.join(root, 'adc.json');
await writeFile(
	credentialsPath,
	JSON.stringify({
		type: 'authorized_user',
		client_id: 'fixture',
		client_secret: 'fixture',
		refresh_token: 'fixture',
		token_uri: `http://127.0.0.1:${cloudPort}/token`
	})
);
await writeFile(
	configPath,
	JSON.stringify({
		operator_addr: '127.0.0.1:4180',
		worker_addr: `127.0.0.1:${workerPort}`,
		data_dir: path.join(root, 'data'),
		ui_dir: path.join(root, 'ui'),
		cloud: {
			endpoint: `http://127.0.0.1:${cloudPort}`,
			project: 'test-project'
		},
		friendzone_url: `http://127.0.0.1:${cloudPort}`,
		worker_profiles: {
			windows: {
				source_image: 'projects/test-project/global/images/test',
				zone: 'test-zone',
				machine_type: 'n2-standard-4',
				subnetwork: 'projects/test-project/regions/test/subnetworks/test',
				disk_gb: 80,
				observatory_port: observatory.address().port
			}
		},
		limits: {
			max_jobs: 4,
			max_finished_jobs: 10,
			max_artifact_bytes: 1024,
			artifact_budget_bytes: 4096,
			upload_timeout_secs: 5,
			request_timeout_secs: 5,
			job_timeout_secs: 300,
			reconcile_secs: 1,
			max_relays: 8,
			max_relay_message_bytes: 1048576
		}
	})
);
const executable = path.resolve(
	'../target/debug',
	process.platform === 'win32' ? 'control-tower.exe' : 'control-tower'
);
const packagedExecutable = path.join(root, path.basename(executable));
await cp(executable, packagedExecutable);
const tower = spawn(packagedExecutable, ['--config', configPath, 'serve'], {
	stdio: 'inherit',
	cwd: root,
	env: {
		...process.env,
		GOOGLE_APPLICATION_CREDENTIALS: credentialsPath,
		NO_PROXY: '127.0.0.1,localhost',
		no_proxy: '127.0.0.1,localhost'
	}
});
let stopping = false;
async function stop() {
	if (stopping) return;
	stopping = true;
	tower.kill();
	cloud.close();
	observatory.close();
	await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
	process.exit();
}
tower.on('exit', () => void stop());
process.on('SIGINT', () => void stop());
process.on('SIGTERM', () => void stop());
