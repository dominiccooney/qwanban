import { SvelteMap } from 'svelte/reactivity';

export enum HostState {
	Disconnected,
	Connecting,
	Connected,
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
const MAX_CLIENT_EVENTS = 2000;

/**
 * A connection to one qbt host. Events arrive as text frames in journal
 * order (snapshot on connect, then live); screenshots are fetched by id and
 * arrive as binary frames prefixed with the id line.
 */
export class Host {
	private _state: HostState = $state(HostState.Connecting);
	private _socket: WebSocket | undefined;
	public events: JournalEvent[] = $state([]);
	/** screenshotId -> object URL, insertion-ordered for eviction. */
	public screenshots = new SvelteMap<string, string>();
	private pendingFetches = new Set<string>();

	constructor(public readonly name: string) {
		void this.connect();
	}

	public get state(): HostState {
		return this._state;
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

	private async connect(): Promise<void> {
		const socket = new WebSocket(`ws://${this.name}`);
		socket.binaryType = 'arraybuffer';
		this._socket = socket;
		socket.onopen = () => {
			this._state = HostState.Connected;
		};
		socket.onclose = () => {
			this._state = HostState.Disconnected;
		};
		socket.onerror = () => {
			this._state = HostState.Disconnected;
		};
		socket.onmessage = (event: MessageEvent) => {
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

	/** Requests the host to capture a fresh screenshot into its journal. */
	public takeScreenshot(): void {
		this.send('takeScreenshot');
	}

	/** Fetches a screenshot by id unless cached or already in flight. */
	public fetchScreenshot(id: string): void {
		if (this.screenshots.has(id) || this.pendingFetches.has(id)) {
			return;
		}
		this.pendingFetches.add(id);
		this.send({ fetchScreenshot: id });
	}

	private receiveScreenshot(data: ArrayBuffer): void {
		const bytes = new Uint8Array(data);
		const newline = bytes.indexOf(0x0a);
		if (newline < 0) {
			return;
		}
		const id = new TextDecoder().decode(bytes.subarray(0, newline));
		this.pendingFetches.delete(id);
		const url = URL.createObjectURL(
			new Blob([bytes.subarray(newline + 1)], { type: 'image/png' })
		);
		this.screenshots.set(id, url);
		while (this.screenshots.size > MAX_CACHED_SCREENSHOTS) {
			const oldest = this.screenshots.keys().next().value!;
			URL.revokeObjectURL(this.screenshots.get(oldest)!);
			this.screenshots.delete(oldest);
		}
	}

	private send(data: object | string): void {
		this._socket?.send(JSON.stringify(data));
	}

	public close(): void {
		this._socket?.close();
		this._state = HostState.Disconnected;
		for (const url of this.screenshots.values()) {
			URL.revokeObjectURL(url);
		}
		this.screenshots.clear();
		this.events = [];
	}
}
