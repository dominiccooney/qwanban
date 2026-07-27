import { browser } from '$app/environment';

/**
 * A reactive clock, ticking once a second. Deriveds that use it (e.g. "43
 * seconds ago" labels) re-evaluate on each tick, so staleness is visible
 * without any events arriving.
 */
class Clock {
	value: number = $state(Date.now());

	constructor() {
		if (browser) {
			setInterval(() => {
				this.value = Date.now();
			}, 1000);
		}
	}
}

export const clock = new Clock();
