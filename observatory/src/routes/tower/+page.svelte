<script lang="ts">
	import { onMount } from 'svelte';
	import { SvelteMap, SvelteURL } from 'svelte/reactivity';
	import { Host } from '$lib/Host.svelte';
	import HostChit from '$lib/components/HostChit.svelte';
	import HostSession from '$lib/components/HostSession.svelte';

	interface Job {
		submission: {
			id: string;
			worker_profile: string;
			title: string;
			source: string;
			prompt: string;
			setup_script: string;
		};
		name: string;
		created_at: number;
		updated_at: number;
		ip: string | null;
		phase: { kind: string; phase?: { kind: string }; failures?: number; next_attempt_at?: number };
		outcome: string | null;
		last_error: string | null;
		failure_error: string | null;
		artifact: { bytes: number } | null;
	}
	let jobs: Job[] = $state([]);
	let workerProfiles: string[] = $state([]);
	let reloadError: string | null = $state(null);
	let error = $state('');
	let pollError = $state('');
	let workerProfile = $state('');
	let title = $state('');
	let prompt = $state('');
	let setupScript = $state('');
	let submitting = $state(false);
	let submissionId: string | undefined;
	let submittedBody: string | undefined;
	let selected: string | undefined = $state(undefined);
	const hosts = new SvelteMap<string, Host>();
	const lifetime = new AbortController();
	let disposed = false;
	let refreshing: Promise<void> | undefined;
	let openHost = $derived(selected ? hosts.get(selected) : undefined);
	let activeJobs = $derived(jobs.filter((job) => job.phase.kind !== 'finished'));
	let finishedJobs = $derived(jobs.filter((job) => job.phase.kind === 'finished'));

	async function request(path: string, body?: unknown): Promise<unknown> {
		const response = await fetch(
			path,
			body === undefined
				? { signal: lifetime.signal }
				: {
						method: 'POST',
						headers: { 'content-type': 'application/json' },
						body: JSON.stringify(body),
						signal: lifetime.signal
					}
		);
		const data = await response.json();
		if (!response.ok) throw new Error(data.error ?? response.statusText);
		return data;
	}

	function refresh(): Promise<void> {
		refreshing ??= refreshOnce().finally(() => (refreshing = undefined));
		return refreshing;
	}
	async function refreshOnce(): Promise<void> {
		const data = (await request('/api/jobs')) as {
			jobs: Job[];
			worker_profiles: string[];
			reload_error: string | null;
		};
		if (disposed) return;
		jobs = data.jobs.sort((a, b) => b.created_at - a.created_at);
		workerProfiles = data.worker_profiles;
		reloadError = data.reload_error;
		if (!workerProfile && workerProfiles.length) workerProfile = workerProfiles[0];
		const running = new Set(
			jobs
				.filter((job) => !job.outcome && (job.phase.phase?.kind ?? job.phase.kind) === 'running')
				.map((job) => job.submission.id)
		);
		for (const [id, host] of hosts) {
			if (!running.has(id)) {
				host.close();
				hosts.delete(id);
			}
		}
		for (const job of jobs) {
			const id = job.submission.id;
			if (running.has(id) && !hosts.has(id)) {
				const url = new SvelteURL(`/api/jobs/${id}/observatory`, location.href);
				url.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
				hosts.set(id, new Host(job.submission.title, url.href));
			}
		}
	}

	onMount(() => {
		let stopped = false;
		let timer: ReturnType<typeof setTimeout>;
		async function poll(): Promise<void> {
			try {
				await refresh();
				if (!stopped) pollError = '';
			} catch (cause) {
				if (!stopped) pollError = String(cause);
			}
			if (!stopped) timer = setTimeout(poll, 2000);
		}
		void poll();
		return () => {
			stopped = true;
			disposed = true;
			lifetime.abort();
			clearTimeout(timer);
			for (const host of hosts.values()) host.close();
			hosts.clear();
		};
	});

	async function submit(): Promise<void> {
		submitting = true;
		try {
			const fields = {
				worker_profile: workerProfile,
				title,
				prompt,
				setup_script: setupScript,
				source: 'operator'
			};
			const body = JSON.stringify(fields);
			if (body !== submittedBody) {
				submissionId = crypto.randomUUID();
				submittedBody = body;
			}
			await request('/api/jobs', { id: submissionId, ...fields });
			title = '';
			prompt = '';
			setupScript = '';
			submissionId = undefined;
			submittedBody = undefined;
			await refresh();
			error = '';
		} catch (cause) {
			error = String(cause);
		} finally {
			submitting = false;
		}
	}
	async function cancel(job: Job): Promise<void> {
		if (!confirm(`Cancel ${job.submission.title} and delete its VM?`)) return;
		try {
			await request(`/api/jobs/${job.submission.id}/cancel`, {});
			await refresh();
			error = '';
		} catch (cause) {
			error = String(cause);
		}
	}
	async function remove(job: Job): Promise<void> {
		try {
			await request(`/api/jobs/${job.submission.id}/delete`, {});
			await refresh();
			error = '';
		} catch (cause) {
			error = String(cause);
		}
	}
	async function retry(job: Job): Promise<void> {
		try {
			await request(`/api/jobs/${job.submission.id}/retry`, {});
			await refresh();
			error = '';
		} catch (cause) {
			error = String(cause);
		}
	}
</script>

<svelte:head><title>Control Tower</title></svelte:head>
<div class="tower">
	<header>
		<div>
			<p class="eyebrow">COMPUTER USE OPERATIONS</p>
			<h1>Control Tower</h1>
		</div>
		<span>{activeJobs.length} active · {finishedJobs.length} finished</span>
	</header>
	{#if error}<p class="error" role="alert">{error}</p>{/if}
	{#if pollError}<p class="error" role="alert">{pollError}</p>{/if}
	{#if reloadError}<p class="error" role="alert">
			Configuration reload rejected: {reloadError}
		</p>{/if}
	{#if openHost}
		<HostSession host={openHost} onClose={() => (selected = undefined)} />
	{:else}
		<details class="submit">
			<summary>Launch a job</summary>
			<form
				onsubmit={(event) => {
					event.preventDefault();
					void submit();
				}}
			>
				<label
					>Worker profile<select bind:value={workerProfile}
						>{#each workerProfiles as name (name)}<option>{name}</option>{/each}</select
					></label
				>
				<label>Title<input bind:value={title} required maxlength="512" /></label>
				<label>Prompt<textarea bind:value={prompt} rows="4"></textarea></label>
				<label
					>Setup script<textarea bind:value={setupScript} rows="4" spellcheck="false"
					></textarea></label
				>
				<button disabled={submitting || !workerProfile || (!prompt && !setupScript)}
					>{submitting ? 'Submitting…' : 'Launch job'}</button
				>
			</form>
		</details>
		<section class="grid" aria-label="Active jobs">
			{#each activeJobs as job (job.submission.id)}
				{@const host = hosts.get(job.submission.id)}
				<article>
					<div class="job-heading">
						<strong>{job.submission.title}</strong>
						<div class="phases">
							<span class="phase">{job.phase.kind.replaceAll('_', ' ')}</span>
							{#if job.phase.phase}
								<span class="phase">{job.phase.phase.kind.replaceAll('_', ' ')}</span>
							{/if}
						</div>
					</div>
					<p class="meta">
						{job.submission.worker_profile} · {job.submission.source} · {new Date(
							job.created_at * 1000
						).toLocaleString()}
					</p>
					<p class="meta">
						{job.ip ?? 'IP pending'}{#if job.outcome}
							· {job.outcome.replaceAll('_', ' ')}{/if}
					</p>
					{#if job.last_error}<p class="error">{job.last_error}</p>{/if}
					{#if job.phase.kind === 'blocked'}<p class="error">
							Automatic attempts stopped. Fix the cause, then retry. Any existing VM or cleanup
							remains pending.
						</p>{/if}
					{#if job.phase.kind === 'retrying' && job.phase.next_attempt_at}<p class="meta">
							Attempt {job.phase.failures} failed; retry after {new Date(
								job.phase.next_attempt_at * 1000
							).toLocaleTimeString()}.
						</p>{/if}
					{#if host}<HostChit {host} onOpen={() => (selected = job.submission.id)}
							>{#snippet controls()}{/snippet}</HostChit
						>{/if}
					<details class="job-details">
						<summary>Prompt &amp; setup</summary>
						<p class="meta">Job ID: <code>{job.submission.id}</code></p>
						{#if job.submission.prompt}
							<p class="detail-label">Prompt</p>
							<pre>{job.submission.prompt}</pre>
						{/if}
						{#if job.submission.setup_script}
							<p class="detail-label">Setup script</p>
							<pre>{job.submission.setup_script}</pre>
						{/if}
					</details>
					<div class="actions">
						{#if job.phase.kind === 'blocked'}<button onclick={() => void retry(job)}>Retry</button
							>{/if}
						{#if !job.outcome}<button onclick={() => void cancel(job)}>Cancel</button
							>{:else if job.phase.kind !== 'blocked'}<button disabled>Deleting VM…</button>{/if}
						{#if job.artifact}
							<a download rel="external" href={`/api/jobs/${job.submission.id}/artifact`}
								>Download packet</a
							>{/if}
					</div>
				</article>
			{:else}<p class="empty">
					No active workers. Launch a job here or submit one from cron.
				</p>{/each}
		</section>
		<h2>History</h2>
		{#each finishedJobs as job (job.submission.id)}
			<details class="history">
				<summary
					><strong>{job.submission.title}</strong><span
						>{job.outcome?.replaceAll('_', ' ')} · {job.submission.worker_profile} · {new Date(
							job.created_at * 1000
						).toLocaleString()}</span
					></summary
				>
				<p>Submitted by {job.submission.source} · <code>{job.submission.id}</code></p>
				{#if job.failure_error ?? job.last_error}<p class="error">
						{job.failure_error ?? job.last_error}
					</p>{/if}
				{#if job.submission.prompt}
					<p class="detail-label">Prompt</p>
					<pre>{job.submission.prompt}</pre>
				{/if}
				{#if job.submission.setup_script}
					<p class="detail-label">Setup script</p>
					<pre>{job.submission.setup_script}</pre>
				{/if}
				<div class="actions">
					{#if job.artifact}
						<a download rel="external" href={`/api/jobs/${job.submission.id}/artifact`}
							>Download packet ({job.artifact.bytes.toLocaleString()} bytes)</a
						>{:else}<span class="meta">No retained packet.</span>{/if}
					<button onclick={() => void remove(job)}>Delete</button>
				</div>
			</details>
		{:else}<p class="empty">No finished jobs.</p>{/each}
	{/if}
</div>

<style>
	.tower {
		max-width: 1500px;
		margin: auto;
	}
	header,
	.job-heading,
	.actions {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
	}
	header {
		border-bottom: 3px solid #163c4c;
		padding-bottom: 1rem;
		margin-bottom: 1.5rem;
	}
	h1 {
		margin: 0;
		color: #163c4c;
		font-size: 2.2rem;
	}
	.eyebrow {
		letter-spacing: 0.15em;
		font-size: 0.7rem;
		color: #647d87;
		margin: 0 0 0.3rem;
	}
	.grid {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(360px, 1fr));
		gap: 1rem;
	}
	article,
	.submit,
	.history {
		border: 1px solid #d5e0e4;
		border-radius: 0.5rem;
		padding: 1rem;
		background: white;
	}
	.submit {
		margin-bottom: 1.5rem;
	}
	.history {
		margin: 0.5rem 0;
	}
	.history summary {
		display: flex;
		flex-wrap: wrap;
		justify-content: space-between;
		gap: 0.5rem;
	}
	form {
		display: grid;
		gap: 0.8rem;
		margin-top: 1rem;
	}
	label {
		display: grid;
		gap: 0.3rem;
	}
	input,
	select,
	textarea {
		width: 100%;
		box-sizing: border-box;
	}
	.phases {
		display: flex;
		gap: 0.35rem;
	}
	.phase {
		background: #e4f0f3;
		color: #163c4c;
		border-radius: 1rem;
		padding: 0.2rem 0.6rem;
		font-size: 0.8rem;
	}
	.job-details {
		margin-top: 0.8rem;
	}
	.job-details summary,
	.submit summary,
	.history summary {
		cursor: pointer;
	}
	.detail-label {
		margin: 0.6rem 0 0.2rem;
		font-size: 0.8rem;
		font-weight: 600;
		color: #163c4c;
	}
	.meta,
	.history span {
		color: #647d87;
		font-size: 0.85rem;
	}
	.error {
		background: #fff0eb;
		color: #8e3420;
		padding: 0.7rem;
		overflow-wrap: anywhere;
	}
	pre {
		white-space: pre-wrap;
		overflow-wrap: anywhere;
		max-height: 24rem;
		overflow: auto;
		background: #f5f8f9;
		padding: 0.8rem;
	}
	.actions {
		justify-content: start;
		flex-wrap: wrap;
		margin-top: 0.8rem;
	}
	.empty {
		color: #647d87;
	}
</style>
