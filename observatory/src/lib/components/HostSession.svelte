<script lang="ts">
	import type { Host, JournalEvent } from '$lib/Host.svelte';
	import {
		bodyOf,
		clickCoordinateOf,
		formatAgo,
		groupRepeats,
		laneOf,
		roleOf,
		summarize,
		type TimelineRun,
		type TranscriptLane
	} from '$lib/journal';
	import { clock } from '$lib/now.svelte';
	import CrossfadeImage from '$lib/components/CrossfadeImage.svelte';
	import { SvelteSet } from 'svelte/reactivity';

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
		computer: 'Computer actions'
	};

	// Which lanes each tab shows. The transcript tabs interleave the computer
	// lane so clicks and typing appear between the messages around them; the
	// computer tab is actions only.
	const tabLanes: Record<Tab, TranscriptLane[]> = {
		driver: ['driver'],
		computer_user: ['computer_user', 'computer'],
		computer: ['computer']
	};

	let visibleEvents: JournalEvent[] = $derived(
		host.events.filter((event) => tabLanes[tab].includes(laneOf(event)))
	);

	// Consecutive identical events (a burst of `screenshot` actions, say) fold
	// into one row so a long run of noise costs one line instead of hundreds.
	// Expanding a run is per-run and survives new events arriving.
	let expandedRuns: SvelteSet<number> = new SvelteSet<number>();
	// Grouping runs over journal order, then the list is reversed so the newest
	// row sits at the top (events inside an expanded run are reversed too).
	let timelineRuns: TimelineRun[] = $derived(
		groupRepeats(visibleEvents)
			.map((run) => ({ ...run, events: [...run.events].reverse() }))
			.reverse()
	);

	function toggleRun(run: TimelineRun): void {
		if (expandedRuns.has(run.key)) {
			expandedRuns.delete(run.key);
		} else {
			expandedRuns.add(run.key);
		}
	}

	/** A collapsed run is pinned when any event inside it is pinned. */
	function runIsPinned(run: TimelineRun): boolean {
		return pinnedSeq !== undefined && run.events.some((event) => event.seq === pinnedSeq);
	}

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
	let imageSize: { width: number; height: number } | undefined = $state(undefined);
	function onImageLoad(size: { width: number; height: number }): void {
		imageSize = size;
	}

	/**
	 * ArrowUp/ArrowDown pin the row above/below. Rows are newest-first, so
	 * ArrowUp moves towards newer events. Collapsed runs count as one row, so
	 * stepping skips a burst of repeats instead of walking through each one.
	 */
	function onTimelineKey(keyEvent: KeyboardEvent): void {
		if (keyEvent.key !== 'ArrowUp' && keyEvent.key !== 'ArrowDown') {
			return;
		}
		keyEvent.preventDefault();
		// Expanded runs step event by event; collapsed ones step as a unit.
		// `steps` follows the on-screen order: newest first.
		const steps: JournalEvent[] = timelineRuns.flatMap((run) =>
			run.events.length === 1 || expandedRuns.has(run.key) ? run.events : [run.representative]
		);
		if (steps.length === 0) {
			return;
		}
		const direction = keyEvent.key === 'ArrowUp' ? -1 : 1;
		// Unpinned follows the newest event, which is the first row.
		const currentIndex =
			pinnedSeq === undefined ? 0 : steps.findIndex((event) => event.seq === pinnedSeq);
		const nextIndex = Math.min(Math.max(currentIndex + direction, 0), steps.length - 1);
		pinnedSeq = steps[nextIndex]?.seq;
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
	// Keep the previous frame up while a newly selected screenshot fetches.
	let displayScreenshotUrl: string | undefined = $state(undefined);
	$effect(() => {
		if (shownScreenshotUrl) {
			displayScreenshotUrl = shownScreenshotUrl;
		}
	});

	function pin(event: JournalEvent): void {
		pinnedSeq = pinnedSeq === event.seq ? undefined : event.seq;
	}
</script>

<div class="session-header">
	<button onclick={onClose}>← Back</button>
	<strong>{host.name}</strong>
	<span class="tabs">
		{#each Object.entries(tabLabels) as [key, label] (key)}
			<button class:active={tab === key} onclick={() => (tab = key as Tab)}>{label}</button>
		{/each}
	</span>
	<button onclick={() => host.takeScreenshot()} title="Take screenshot">📸</button>
</div>

<div class="session">
	<div class="screenshot-pane">
		{#if displayScreenshotUrl}
			{#if shownScreenshotEvent}
				<span class="caption">
					{summarize(shownScreenshotEvent)}
					— {formatAgo(shownScreenshotEvent.atMs, clock.value)}
					{#if pinnedSeq !== undefined}(pinned — click the row again to follow live){/if}
				</span>
			{/if}
			<div class="screenshot-frame">
				<CrossfadeImage
					src={displayScreenshotUrl}
					alt="Screen at the selected moment"
					onLoad={onImageLoad}
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
			<p class="empty">No screenshot yet.</p>
		{/if}
	</div>
	<div class="timeline" role="listbox" tabindex="0" onkeydown={onTimelineKey}>
		{#each timelineRuns as run (run.key)}
			{#if run.events.length === 1 || expandedRuns.has(run.key)}
				{#if run.events.length > 1}
					<button class="run-toggle" onclick={() => toggleRun(run)}>
						▾ collapse {run.events.length} × {summarize(run.representative)}
					</button>
				{/if}
				{#each run.events as event (event.seq)}
					<!-- svelte-ignore a11y_click_events_have_key_events -->
					<div
						class="event"
						class:in-run={run.events.length > 1}
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
				{/each}
			{:else}
				<!-- svelte-ignore a11y_click_events_have_key_events -->
				<div
					class="event collapsed"
					class:pinned={runIsPinned(run)}
					onclick={() => pin(run.representative)}
					role="option"
					tabindex="-1"
					aria-selected={runIsPinned(run)}
				>
					<span class="role">
						{formatAgo(run.representative.atMs, clock.value)}
						{roleOf(run.representative)}
					</span>
					{summarize(run.representative)}
					{#if run.representative.screenshotId}📷{/if}
					<button
						class="run-count"
						title="Show all {run.events.length} events"
						onclick={(clickEvent) => {
							clickEvent.stopPropagation();
							toggleRun(run);
						}}
					>
						×{run.events.length} ▸
					</button>
				</div>
			{/if}
		{:else}
			<p class="empty">No events in this lane yet.</p>
		{/each}
	</div>
</div>

<style>
	.session-header {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem 0.75rem;
		margin-bottom: 0.85rem;
	}

	.session-header strong {
		margin-right: 0.25rem;
	}

	.tabs {
		display: inline-flex;
		flex-wrap: wrap;
		gap: 0.35rem;
	}

	.tabs button.active {
		font-weight: 700;
		background: #4b2d70;
		box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.2);
	}

	.session {
		display: grid;
		grid-template-columns: minmax(0, 55%) minmax(0, 45%);
		gap: 0.85rem;
		height: 100%;
	}

	.timeline {
		overflow-y: auto;
		max-height: 80vh;
		outline: none;
		background: #fff;
		border: 1px solid #ddd;
		border-radius: 8px;
		padding: 0.35rem 0;
	}

	.event {
		font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
		font-size: 0.9em;
		padding: 0.4em 0.65em;
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

	.event.in-run {
		background: #fafafa;
	}

	.run-count {
		margin-left: 0.5em;
		padding: 0 0.4em;
		font-family: inherit;
		font-size: 0.9em;
		border-radius: 999px;
		background: #e9e2f5;
		color: #4b2d70;
	}

	.run-count:hover {
		background: #d9cdf0;
	}

	.run-toggle {
		display: block;
		width: calc(100% - 1.3em);
		margin: 0.15em 0.65em;
		text-align: left;
		font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
		font-size: 0.8em;
		background: #f0ebfa;
		color: #4b2d70;
	}

	.run-toggle:hover {
		background: #e2d8f5;
		color: #3a2258;
	}

	.event .body {
		white-space: pre-wrap;
		color: #333;
		display: block;
		margin-left: 1em;
		margin-top: 0.2em;
	}

	.screenshot-pane {
		position: sticky;
		top: 0;
		align-self: start;
	}

	.screenshot-pane .caption {
		font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
		font-size: 0.85em;
		color: #666;
		display: block;
		margin-bottom: 0.4rem;
	}

	.screenshot-frame {
		position: relative;
		background: #fff;
		border: 1px solid #ddd;
		border-radius: 8px;
		overflow: hidden;
		box-shadow: 0 1px 2px rgba(0, 0, 0, 0.04);
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

	.empty {
		margin: 0.75rem;
		color: #666;
	}
</style>
