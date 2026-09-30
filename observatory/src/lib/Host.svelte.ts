import { SvelteMap, SvelteSet } from 'svelte/reactivity';

export enum HostState {
	Disconnected,
	Connecting,
	Connected
}

/**
 * One event from a qbt host's journal. `payload` is the raw journal payload:
 * for `computer.action` it holds the request qbt executed; for published
 * events (kind `transcript.*`, `helper.*`, `session.*`) it holds the
 * artifact event the agent published, whose `source.kind` says whether it
 * came from the driver or the computer user.
 */
export interface JournalEvent {
	seq: number;
	atMs: number;
	kind: string;
	payload: Record<string, unknown>;
	screenshotId?: string;
}

const MAX_CACHED_SCREENSHOTS = 20;
const MAX_CLIENT_EVENTS = 500;
const RECONNECT_DELAY_MS = 1000;

/**
 * A connection to one qbt host. Events arrive as text frames in journal
 * order (snapshot on connect, then live); screenshots are fetched by id and
 * arrive as binary frames prefixed with the id line.
 */
export class Host {
	private _state: HostState = $state(HostState.Connecting);
	private _generation = $state(0);
	private _socket: WebSocket | undefined;
	private reconnectTimer: ReturnType<typeof setTimeout> | undefined;
	private closed = false;
	public events: JournalEvent[] = $state([]);
	/** screenshotId -> object URL, insertion-ordered for eviction. */
	public screenshots = new SvelteMap<string, string>();
	private pendingFetches = new SvelteSet<string>();

	constructor(public readonly name: string) {
		void this.connect();
	}

	public get state(): HostState {
		return this._state;
	}

	/** Identifies the current qbt server lifetime. */
	public get generation(): number {
		return this._generation;
	}

	/** The most recent event that captured a screenshot, if any. */
	public get latestScreenshotEvent(): JournalEvent | undefined {
		for (let i = this.events.length - 1; i >= 0; i--) {
			if (this.events[i].screenshotId) {
				return this.events[i];
			}
		}
		return undefined;
	}

	public get latestEvent(): JournalEvent | undefined {
		return this.events.at(-1);
	}

	private connect(): void {
		if (this.closed) {
			return;
		}
		this._state = HostState.Connecting;
		const socket = new WebSocket(`ws://${this.name}`);
		socket.binaryType = 'arraybuffer';
		this._socket = socket;
		socket.onopen = () => {
			if (this._socket !== socket) {
				return;
			}
			// A successful connection is the boundary between server lifetimes.
			// Qbt restarts sequence numbers and screenshot ids, so its new
			// snapshot replaces all data from the previous connection.
			this.clearJournal();
			this._generation += 1;
			this._state = HostState.Connected;
		};
		socket.onclose = () => {
			this.handleDisconnect(socket);
		};
		socket.onerror = () => {
			this.handleDisconnect(socket);
			socket.close();
		};
		socket.onmessage = (event: MessageEvent) => {
			if (this._socket !== socket) {
				return;
			}
			if (event.data instanceof ArrayBuffer) {
				this.receiveScreenshot(event.data);
				return;
			}
			const journalEvent = JSON.parse(String(event.data)) as JournalEvent;
			if (journalEvent.seq === undefined) {
				return; // e.g. a missingScreenshot notice
			}
			this.events.push(journalEvent);
			if (this.events.length > MAX_CLIENT_EVENTS) {
				this.events.shift();
			}
			// Screenshots are NOT fetched here: fetching is display-driven
			// (the views fetch what they show), so a connect-time snapshot
			// of a hundred screenshot events costs one image, not a hundred.
		};
	}

	private handleDisconnect(socket: WebSocket): void {
		if (this._socket !== socket || this.closed) {
			return;
		}
		this._socket = undefined;
		this._state = HostState.Disconnected;
		this.pendingFetches.clear();
		this.reconnectTimer = setTimeout(() => {
			this.reconnectTimer = undefined;
			this.connect();
		}, RECONNECT_DELAY_MS);
	}

	/** Requests the host to capture a fresh screenshot into its journal. */
	public takeScreenshot(): void {
		this.send('takeScreenshot');
	}

	/** Fetches a screenshot by id unless cached or already in flight. */
	public fetchScreenshot(id: string): void {
		if (this.screenshots.has(id) || this.pendingFetches.has(id)) {
			return;
		}
		if (this.send({ fetchScreenshot: id })) {
			this.pendingFetches.add(id);
		}
	}

	private receiveScreenshot(data: ArrayBuffer): void {
		const bytes = new Uint8Array(data);
		const newline = bytes.indexOf(0x0a);
		if (newline < 0) {
			return;
		}
		const id = new TextDecoder().decode(bytes.subarray(0, newline));
		this.pendingFetches.delete(id);
		const url = URL.createObjectURL(new Blob([bytes.subarray(newline + 1)], { type: 'image/png' }));
		this.screenshots.set(id, url);
		while (this.screenshots.size > MAX_CACHED_SCREENSHOTS) {
			const oldest = this.screenshots.keys().next().value!;
			URL.revokeObjectURL(this.screenshots.get(oldest)!);
			this.screenshots.delete(oldest);
		}
	}

	private send(data: object | string): boolean {
		if (this._socket?.readyState !== WebSocket.OPEN) {
			return false;
		}
		this._socket.send(JSON.stringify(data));
		return true;
	}

	private clearJournal(): void {
		for (const url of this.screenshots.values()) {
			URL.revokeObjectURL(url);
		}
		this.screenshots.clear();
		this.pendingFetches.clear();
		this.events = [];
	}

	public close(): void {
		this.closed = true;
		if (this.reconnectTimer !== undefined) {
			clearTimeout(this.reconnectTimer);
			this.reconnectTimer = undefined;
		}
		const socket = this._socket;
		this._socket = undefined;
		socket?.close();
		this._state = HostState.Disconnected;
		this.clearJournal();
	}
}
