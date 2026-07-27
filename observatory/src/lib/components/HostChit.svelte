<script lang="ts">
import type { Snippet } from 'svelte';
import { type Host, HostState } from '$lib/Host.svelte';
import { formatAgo, latestAgentStatuses, summarize } from '$lib/journal';
import { clock } from '$lib/now.svelte';

type Props = {
	host: Host;
	onOpen: () => void;
	controls: Snippet;
};

let { host, onOpen, controls }: Props = $props();

function labelForState(state: HostState) {
	switch (state) {
		case HostState.Connected:
			return 'connected';
		case HostState.Connecting:
			return 'connecting';
		case HostState.Disconnected:
			return 'disconnected';
		default:
			console.error(`unknown host state: ${state}`);
			return '???';
	}
}

let state: string = $derived(labelForState(host.state));
// Fetching is display-driven: this chit shows only the latest screenshot,
// so only the latest is fetched (fetchScreenshot dedupes in-flight ids).
$effect(() => {
	const id = host.latestScreenshotEvent?.screenshotId;
	if (id) {
		host.fetchScreenshot(id);
	}
});
let latestScreenshotUrl: string | undefined = $derived.by(() => {
	const id = host.latestScreenshotEvent?.screenshotId;
	return id ? host.screenshots.get(id) : undefined;
});
// The glance line: the most recent activity on this host. Relative time
// (via the ticking clock) makes a hung agent stand out.
let latestActivity: string = $derived.by(() => {
	const event = host.latestEvent;
	return event
		? `${summarize(event)} — ${formatAgo(event.atMs, clock.value)}`
		: 'no activity yet';
});
let statuses = $derived(latestAgentStatuses(host.events));
</script>

<style>
.chit {
	border: 1px solid #ccc;
	border-radius: 4px;
	padding: 0.5em;
}
.activity {
	font-family: monospace;
	font-size: 0.85em;
	white-space: nowrap;
	overflow: hidden;
	text-overflow: ellipsis;
	display: block;
}
.statuses {
	display: block;
	font-size: 0.85em;
}
.status {
	display: inline-block;
	border-radius: 3px;
	padding: 0 0.4em;
	margin-right: 0.4em;
	background: #eee;
	color: #555;
}
.status.running {
	background: #d2f8d2;
	color: #1a6b1a;
}
.screenshot-button {
	display: block;
	width: 100%;
	padding: 0;
	border: none;
	background: none;
	cursor: pointer;
}
</style>

<div class="chit">
	<button onclick={() => host.takeScreenshot()}>📸</button>
	<button onclick={onOpen}>📜</button>
	{@render controls()}
	{host.name} ({state}) — {host.events.length} event(s)
	<span class="statuses">
		<span class="status" class:running={statuses.driver === 'running'}>
			driver: {statuses.driver ?? '—'}
		</span>
		<span class="status" class:running={statuses.computerUser === 'running'}>
			computer user: {statuses.computerUser ?? '—'}
		</span>
	</span>
	<span class="activity">{latestActivity}</span>
	{#if latestScreenshotUrl}
		<button class="screenshot-button" onclick={onOpen}>
			<img src={latestScreenshotUrl} alt="Latest screenshot" width="100%" />
		</button>
	{/if}
</div>
