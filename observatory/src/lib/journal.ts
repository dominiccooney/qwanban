import type { JournalEvent } from '$lib/Host.svelte';

/**
 * View-model helpers over raw journal events. One module derives every
 * label the UI shows so the chit, the timeline, and the transcript can
 * never disagree about what an event means.
 */

export type TranscriptLane = 'driver' | 'computer_user' | 'computer' | 'other';

interface ArtifactEventPayload {
	source?: { kind?: string; sessionId?: string };
	payload?: Record<string, unknown>;
	type?: string;
}

/** Which transcript an event belongs to. */
export function laneOf(event: JournalEvent): TranscriptLane {
	if (event.kind === 'computer.action' || event.kind === 'observer.screenshot') {
		return 'computer';
	}
	const source = (event.payload as ArtifactEventPayload).source;
	if (source?.kind === 'driver') {
		return 'driver';
	}
	if (source?.kind === 'computer_user') {
		return 'computer_user';
	}
	return 'other';
}

/** One-line human-readable summary of an event for timelines and chits. */
export function summarize(event: JournalEvent): string {
	if (event.kind === 'computer.action') {
		const request = event.payload.request as Record<string, unknown> | undefined;
		const action = String(request?.action ?? 'unknown');
		const parts = [action];
		if (Array.isArray(request?.coordinate)) {
			parts.push(`@ ${request.coordinate.join(',')}`);
		}
		if (typeof request?.text === 'string') {
			parts.push(JSON.stringify(request.text));
		}
		if (event.payload.ok === false) {
			parts.push('FAILED');
		}
		return parts.join(' ');
	}
	if (event.kind === 'observer.screenshot') {
		return 'observer screenshot';
	}
	const artifact = event.payload as ArtifactEventPayload;
	const inner = artifact.payload ?? {};
	switch (event.kind) {
		case 'transcript.message_committed': {
			const role = String(inner.role ?? 'message');
			if (role === 'tool_call') {
				return `${inner.toolName}(${inner.input ?? ''})`;
			}
			if (role === 'tool_result') {
				return `${inner.toolName} → ${inner.ok ? 'ok' : `error: ${inner.error}`}`;
			}
			return String(inner.text ?? '');
		}
		case 'helper.note':
			return `[${inner.kind}] ${inner.message}`;
		case 'helper.question':
			return `question: ${inner.question}`;
		case 'helper.status_changed':
			return `status → ${inner.to}`;
		case 'session.status_changed':
			return `${laneOf(event) === 'driver' ? 'driver' : 'computer user'} → ${inner.status}`;
		case 'session.started':
			return `session started (${inner.role})`;
		default:
			return event.kind;
	}
}

/** Long-form body for the transcript view; empty when summary says it all. */
export function bodyOf(event: JournalEvent): string {
	if (event.kind === 'transcript.message_committed') {
		const inner = (event.payload as ArtifactEventPayload).payload ?? {};
		const role = String(inner.role ?? '');
		if (role === 'assistant' || role === 'user' || role === 'reasoning') {
			return String(inner.text ?? '');
		}
	}
	return '';
}

export function roleOf(event: JournalEvent): string {
	if (event.kind === 'computer.action') {
		return 'action';
	}
	const inner = (event.payload as ArtifactEventPayload).payload ?? {};
	return String(inner.role ?? event.kind);
}

export function formatTime(atMs: number): string {
	return new Date(atMs).toLocaleTimeString();
}

/** "5 seconds ago", "3 minutes ago", ... — staleness at a glance. */
export function formatAgo(atMs: number, nowMs: number): string {
	const seconds = Math.max(0, Math.round((nowMs - atMs) / 1000));
	const units: [number, string][] = [
		[86400, 'day'],
		[3600, 'hour'],
		[60, 'minute'],
		[1, 'second']
	];
	for (const [size, name] of units) {
		if (seconds >= size || size === 1) {
			const count = Math.floor(seconds / size);
			return `${count} ${name}${count === 1 ? '' : 's'} ago`;
		}
	}
	return 'now';
}

export interface AgentStatuses {
	driver?: string;
	computerUser?: string;
}

/**
 * The latest run status each agent reported (`session.status_changed`),
 * e.g. running / completed / aborted / failed.
 */
export function latestAgentStatuses(events: JournalEvent[]): AgentStatuses {
	const statuses: AgentStatuses = {};
	// Newest-first: the first status seen per lane wins.
	for (let i = events.length - 1; i >= 0; i--) {
		const event = events[i];
		if (event.kind !== 'session.status_changed') {
			continue;
		}
		const lane = laneOf(event);
		const status = String((event.payload as ArtifactEventPayload).payload?.status ?? '');
		if (lane === 'driver' && statuses.driver === undefined) {
			statuses.driver = status;
		} else if (lane === 'computer_user' && statuses.computerUser === undefined) {
			statuses.computerUser = status;
		}
		if (statuses.driver !== undefined && statuses.computerUser !== undefined) {
			break;
		}
	}
	return statuses;
}

/**
 * A run of consecutive timeline events that say the same thing (e.g. dozens of
 * back-to-back `screenshot` actions). Runs of one are ordinary rows.
 */
export interface TimelineRun {
	/** Stable key for keyed each blocks: the seq of the first event. */
	key: number;
	events: JournalEvent[];
	/** The newest event of the run: its screenshot is the run's screen. */
	representative: JournalEvent;
}

/**
 * Identity used to collapse repeats. Events with a long-form body (assistant,
 * user, reasoning) always stay separate: their text is the point, so folding
 * them would hide content rather than noise.
 */
function repeatKeyOf(event: JournalEvent): string | undefined {
	if (bodyOf(event)) {
		return undefined;
	}
	return `${event.kind}|${roleOf(event)}|${summarize(event)}`;
}

/** Folds consecutive identical events into runs, preserving journal order. */
export function groupRepeats(events: JournalEvent[]): TimelineRun[] {
	const runs: TimelineRun[] = [];
	for (const event of events) {
		const previous = runs.at(-1);
		const key = repeatKeyOf(event);
		if (
			previous !== undefined &&
			key !== undefined &&
			repeatKeyOf(previous.representative) === key
		) {
			previous.events.push(event);
			// The newest event represents the run, so following the timeline
			// live keeps showing the latest screen.
			previous.representative = event;
			continue;
		}
		runs.push({ key: event.seq, events: [event], representative: event });
	}
	return runs;
}

/** The click/drag coordinate of a computer action, for image annotation. */
export function clickCoordinateOf(event: JournalEvent): { x: number; y: number } | undefined {
	if (event.kind !== 'computer.action') {
		return undefined;
	}
	const request = event.payload.request as Record<string, unknown> | undefined;
	const coordinate = request?.coordinate;
	if (Array.isArray(coordinate) && coordinate.length === 2) {
		return { x: Number(coordinate[0]), y: Number(coordinate[1]) };
	}
	return undefined;
}
