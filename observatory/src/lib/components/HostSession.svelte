<script lang="ts">
import type { Host, JournalEvent } from '$lib/Host.svelte';
import {
	bodyOf,
	clickCoordinateOf,
	formatAgo,
	laneOf,
	roleOf,
	summarize,
	type TranscriptLane
} from '$lib/journal';
import { clock } from '$lib/now.svelte';

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
// newest screenshot; clicking a timeline row (or ArrowUp/ArrowDown) pins
// that moment.
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

// The click made right after the shown screenshot, drawn as a marker on it:
// "this is where the next action landed".
let annotatedClick: { x: number; y: number; label: string } | undefined = $derived.by(() => {
	const shown = shownScreenshotEvent;
	if (!shown) {
		return undefined;
	}
	const limitSeq = pinnedSeq ?? Number.MAX_SAFE_INTEGER;
	for (const event of host.events) {
		if (event.seq <= shown.seq || event.seq > limitSeq) {
			continue;
		}
		const coordinate = clickCoordinateOf(event);
		if (coordinate) {
			return { ...coordinate, label: summarize(event) };
		}
		if (event.screenshotId) {
			break; // The screen changed; later clicks belong to it.
		}
	}
	return undefined;
});

// Display size for scaling click coordinates onto the rendered image.
let imageElement: HTMLImageElement | undefined = $state(undefined);
let imageSize: { width: number; height: number } | undefined = $state(undefined);
function onImageLoad(): void {
	if (imageElement) {
		imageSize = {
			width: imageElement.naturalWidth,
			height: imageElement.naturalHeight
		};
	}
}

/** ArrowUp/ArrowDown pin the previous/next visible event. */
function onTimelineKey(keyEvent: KeyboardEvent): void {
	if (keyEvent.key !== 'ArrowUp' && keyEvent.key !== 'ArrowDown') {
		return;
	}
	keyEvent.preventDefault();
	const events = visibleEvents;
	if (events.length === 0) {
		return;
	}
	const direction = keyEvent.key === 'ArrowUp' ? -1 : 1;
	const currentIndex =
		pinnedSeq === undefined
			? events.length - 1
			: events.findIndex((event) => event.seq === pinnedSeq);
	const nextIndex = Math.min(
		Math.max(currentIndex + direction, 0),
		events.length - 1
	);
	pinnedSeq = events[nextIndex]?.seq;
}
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
	grid-template-columns: minmax(0, 55%) minmax(0, 45%);
	gap: 0.5em;
	height: 100%;
}
.timeline {
	overflow-y: auto;
	max-height: 80vh;
	outline: none;
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
	align-self: start;
}
.screenshot-pane .caption {
	font-family: monospace;
	font-size: 0.85em;
	color: #666;
	display: block;
}
.screenshot-frame {
	position: relative;
}
.screenshot-frame img {
	width: 100%;
	display: block;
}
.click-marker {
	position: absolute;
	width: 24px;
	height: 24px;
	margin: -12px 0 0 -12px;
	border: 3px solid #ff3b30;
	border-radius: 50%;
	box-shadow: 0 0 0 2px rgba(255, 255, 255, 0.8);
	pointer-events: none;
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
	<div class="screenshot-pane">
		{#if shownScreenshotEvent && shownScreenshotUrl}
			<span class="caption">
				{summarize(shownScreenshotEvent)}
				— {formatAgo(shownScreenshotEvent.atMs, clock.value)}
				{#if pinnedSeq !== undefined}(pinned — click the row again to follow live){/if}
			</span>
			<div class="screenshot-frame">
				<img
					bind:this={imageElement}
					onload={onImageLoad}
					src={shownScreenshotUrl}
					alt="Screen at the selected moment"
				/>
				{#if annotatedClick && imageSize}
					<div
						class="click-marker"
						title={annotatedClick.label}
						style:left={`${(annotatedClick.x / imageSize.width) * 100}%`}
						style:top={`${(annotatedClick.y / imageSize.height) * 100}%`}
					></div>
				{/if}
			</div>
		{:else}
			<p>No screenshot yet.</p>
		{/if}
	</div>
	<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
	<div class="timeline" role="listbox" tabindex="0" onkeydown={onTimelineKey}>
		{#each visibleEvents as event (event.seq)}
			<!-- svelte-ignore a11y_click_events_have_key_events -->
			<div
				class="event"
				class:pinned={pinnedSeq === event.seq}
				onclick={() => pin(event)}
				role="option"
				tabindex="-1"
				aria-selected={pinnedSeq === event.seq}
			>
				<span class="role">{formatAgo(event.atMs, clock.value)} {roleOf(event)}</span>
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
</div>
