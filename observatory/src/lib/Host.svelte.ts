export enum HostState {
	Disconnected,
	Connecting,
	Connected,
}

export class Host {
	private _state: HostState = $state(HostState.Connecting);
	private _socket: WebSocket | undefined;
	public screenshot: string | undefined = $state(undefined);

	constructor(public readonly name: string) {
		void(this.connect());
	}

	public get state(): HostState {
		return this._state;
	}

	private async connect(): Promise<void> {
		this._socket = new WebSocket(`ws://${this.name}`);
		this._socket.onopen = () => {
			this._state = HostState.Connected;
		};
		this._socket.onclose = () => {
			this._state = HostState.Disconnected;
		};
		this._socket.onerror = () => {
			this._state = HostState.Disconnected;
		};
		this._socket.onmessage = (event: MessageEvent) => {
			console.log(String(event.data));
			if (event.data instanceof Blob) {
				if (this.screenshot) {
					URL.revokeObjectURL(this.screenshot);
				}
				this.screenshot = URL.createObjectURL(event.data);
			}
		};
	}

	public send(data: object | string | boolean | number | undefined) {
		this._socket?.send(JSON.stringify(data));
	}

	public close(): void {
		console.log(`Host: close ${this.name}`)
		this._socket?.close();
		this._state = HostState.Disconnected;
		if (this.screenshot) {
			URL.revokeObjectURL(this.screenshot);
			this.screenshot = undefined;
		}
	}
}
