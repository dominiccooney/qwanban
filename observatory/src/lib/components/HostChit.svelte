<script lang="ts">
	import type { Snippet } from 'svelte';
	import { type Host, HostState } from '$lib/Host.svelte';
	import { formatAgo, latestAgentStatuses, summarize } from '$lib/journal';
	import { clock } from '$lib/now.svelte';
	import CrossfadeImage from '$lib/components/CrossfadeImage.svelte';

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

	/** CSS modifier for an agent run-status pill. */
	function statusTone(status: string | undefined): string {
		switch (status) {
			case 'running':
				return 'running';
			case 'completed':
				return 'completed';
			case 'failed':
				return 'failed';
			case 'aborted':
				return 'aborted';
			default:
				return 'idle';
		}
	}

	let connectionLabel: string = $derived(labelForState(host.state));
	let connectionTone: string = $derived(
		host.state === HostState.Connected
			? 'connected'
			: host.state === HostState.Connecting
				? 'connecting'
				: 'disconnected'
	);
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
	// Hold the last decoded frame while the next id is in flight so the
	// chit does not unmount the image (and flash empty) between fetches.
	let displayScreenshotUrl: string | undefined = $state(undefined);
	$effect(() => {
		if (latestScreenshotUrl) {
			displayScreenshotUrl = latestScreenshotUrl;
		}
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
	let driverTone = $derived(statusTone(statuses.driver));
	let computerUserTone = $derived(statusTone(statuses.computerUser));
</script>

<div class="chit" class:chit-connected={host.state === HostState.Connected}>
	<div class="chit-header">
		<div class="chit-title">
			{host.name}
			<span class="chit-meta">
				<span class="badge connection {connectionTone}">{connectionLabel}</span>
				<span class="event-count">{host.events.length} event(s)</span>
			</span>
		</div>
		<div class="chit-actions">
			<button onclick={() => host.takeScreenshot()} title="Take screenshot">📸</button>
			<button onclick={onOpen} title="Open session">📜</button>
			{@render controls()}
		</div>
	</div>
	<span class="statuses">
		<span class="badge status {driverTone}">
			driver: {statuses.driver ?? '—'}
		</span>
		<span class="badge status {computerUserTone}">
			computer user: {statuses.computerUser ?? '—'}
		</span>
	</span>
	<span class="activity">{latestActivity}</span>
	{#if displayScreenshotUrl}
		<button class="screenshot-button" onclick={onOpen}>
			<CrossfadeImage src={displayScreenshotUrl} alt="Latest screenshot" />
		</button>
	{/if}
</div>

<style>
	.chit {
		display: flex;
		flex-direction: column;
		gap: 0.45rem;
		border: 1px solid #ddd;
		border-left: 4px solid #c8c8ce;
		border-radius: 8px;
		padding: 0.75rem;
		background: #fff;
		box-shadow: 0 1px 2px rgba(0, 0, 0, 0.04);
	}

	.chit-connected {
		border-left-color: #2f9e44;
	}

	.chit-header {
		display: flex;
		align-items: flex-start;
		justify-content: space-between;
		gap: 0.5rem;
	}

	.chit-title {
		min-width: 0;
		font-weight: 600;
		word-break: break-all;
	}

	.chit-meta {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.35rem;
		margin-top: 0.25rem;
		font-weight: 400;
	}

	.event-count {
		font-size: 0.85em;
		color: #666;
	}

	.chit-actions {
		display: flex;
		flex-shrink: 0;
		gap: 0.3rem;
	}

	.activity {
		font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
		font-size: 0.85em;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
		display: block;
		color: #444;
	}

	.statuses {
		display: flex;
		flex-wrap: wrap;
		gap: 0.35rem;
		font-size: 0.85em;
	}

	.badge {
		display: inline-block;
		border-radius: 999px;
		padding: 0.12em 0.55em;
		font-size: 0.85em;
		line-height: 1.35;
		background: #eee;
		color: #555;
	}

	/* WebSocket connection */
	.badge.connection.connected {
		background: #d2f8d2;
		color: #1a6b1a;
	}
	.badge.connection.connecting {
		background: #fff3bf;
		color: #8a6d00;
	}
	.badge.connection.disconnected {
		background: #ffe3e3;
		color: #c92a2a;
	}

	/* Agent run status from session.status_changed */
	.badge.status.running {
		background: #d2f8d2;
		color: #1a6b1a;
	}
	.badge.status.completed {
		background: #d0ebff;
		color: #1864ab;
	}
	.badge.status.failed {
		background: #ffe3e3;
		color: #c92a2a;
	}
	.badge.status.aborted {
		background: #ffe8cc;
		color: #d9480f;
	}
	.badge.status.idle {
		background: #eee;
		color: #555;
	}

	.screenshot-button {
		display: block;
		width: 100%;
		margin-top: 0.15rem;
		padding: 0;
		border: 1px solid #eee;
		background: none;
		cursor: pointer;
		border-radius: 6px;
		overflow: hidden;
		line-height: 0;
	}
</style>
