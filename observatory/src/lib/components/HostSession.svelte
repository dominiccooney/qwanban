<script lang="ts">
import type { Host, JournalEvent } from '$lib/Host.svelte';
import { bodyOf, formatTime, laneOf, roleOf, summarize, type TranscriptLane } from '$lib/journal';

type Props = {
	host: Host;
	onClose: () => void;
};

let { host, onClose }: Props = $props();

type Tab = 'driver' | 'computer_user' | 'computer';
let tab: Tab = $state('computer');

const tabLabels: Record<Tab, string> = {
	driver: 'Driver transcript',
	computer_user: 'Computer user transcript',
	computer: 'Computer actions',
};

// Which lanes each tab shows. The transcript tabs interleave the computer
// lane so clicks and typing appear between the messages around them; the
// computer tab is actions only.
const tabLanes: Record<Tab, TranscriptLane[]> = {
	driver: ['driver'],
	computer_user: ['computer_user', 'computer'],
	computer: ['computer'],
};

let visibleEvents: JournalEvent[] = $derived(
	host.events.filter((event) => tabLanes[tab].includes(laneOf(event)))
);

// The event whose screenshot fills the pane. Defaults to following the
// newest screenshot; clicking a timeline row pins that moment.
let pinnedSeq: number | undefined = $state(undefined);
let screenshotEvents: JournalEvent[] = $derived(
	host.events.filter((event) => event.screenshotId)
);
let shownScreenshotEvent: JournalEvent | undefined = $derived.by(() => {
	if (pinnedSeq !== undefined) {
		// The latest screenshot at or before the pinned event shows the
		// screen as it was when that event happened.
		for (let i = screenshotEvents.length - 1; i >= 0; i--) {
			if (screenshotEvents[i].seq <= pinnedSeq) {
				return screenshotEvents[i];
			}
		}
		return screenshotEvents[0];
	}
	return screenshotEvents.at(-1);
});
// Fetch outside the derived: deriveds must stay pure, and the fetch
// resolves into `host.screenshots`, which the derived below tracks.
$effect(() => {
	const id = shownScreenshotEvent?.screenshotId;
	if (id) {
		host.fetchScreenshot(id);
	}
});
let shownScreenshotUrl: string | undefined = $derived.by(() => {
	const id = shownScreenshotEvent?.screenshotId;
	return id ? host.screenshots.get(id) : undefined;
});

function pin(event: JournalEvent): void {
	pinnedSeq = pinnedSeq === event.seq ? undefined : event.seq;
}
</script>

<style>
.session {
	display: grid;
	grid-template-columns: 1fr 1fr;
	gap: 1em;
	height: 100%;
}
.timeline {
	overflow-y: auto;
	max-height: 80vh;
}
.event {
	font-family: monospace;
	font-size: 0.9em;
	padding: 0.15em 0.3em;
	cursor: pointer;
	border-left: 3px solid transparent;
}
.event:hover {
	background: #f0f0f0;
}
.event.pinned {
	border-left-color: #06c;
	background: #e8f0fe;
}
.event .role {
	color: #888;
	margin-right: 0.5em;
}
.event .body {
	white-space: pre-wrap;
	color: #333;
	display: block;
	margin-left: 1em;
}
.screenshot-pane {
	position: sticky;
	top: 0;
}
.screenshot-pane .caption {
	font-family: monospace;
	font-size: 0.85em;
	color: #666;
}
.tabs button.active {
	font-weight: bold;
}
</style>

<div>
	<button onclick={onClose}>← Back</button>
	<strong>{host.name}</strong>
	<span class="tabs">
		{#each Object.entries(tabLabels) as [key, label] (key)}
			<button class:active={tab === key} onclick={() => (tab = key as Tab)}>{label}</button>
		{/each}
	</span>
	<button onclick={() => host.takeScreenshot()}>📸</button>
</div>

<div class="session">
	<div class="timeline">
		{#each visibleEvents as event (event.seq)}
			<div
				class="event"
				class:pinned={pinnedSeq === event.seq}
				onclick={() => pin(event)}
				onkeydown={(keyEvent) => keyEvent.key === 'Enter' && pin(event)}
				role="button"
				tabindex="0"
			>
				<span class="role">{formatTime(event.atMs)} {roleOf(event)}</span>
				{summarize(event)}
				{#if event.screenshotId}📷{/if}
				{#if bodyOf(event)}
					<span class="body">{bodyOf(event)}</span>
				{/if}
			</div>
		{:else}
			<p>No events in this lane yet.</p>
		{/each}
	</div>
	<div class="screenshot-pane">
		{#if shownScreenshotEvent && shownScreenshotUrl}
			<span class="caption">
				{formatTime(shownScreenshotEvent.atMs)}
				{summarize(shownScreenshotEvent)}
				{#if pinnedSeq !== undefined}(pinned — click the row again to follow live){/if}
			</span>
			<img src={shownScreenshotUrl} alt="Screen at the selected moment" width="100%" />
		{:else}
			<p>No screenshot yet.</p>
		{/if}
	</div>
</div>
