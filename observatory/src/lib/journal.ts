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
