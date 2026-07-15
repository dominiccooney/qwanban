import { browser } from '$app/environment';

export class LocalStore<T> {
	value = $state() as T;

	constructor(key: string, initialValue: T) {
		if (browser) {
			const saved = localStorage.getItem(key);
			this.value = saved ? JSON.parse(saved) : initialValue;
		} else {
			this.value = JSON.parse(JSON.stringify(initialValue));
		}

		$effect.root(() => {
			$effect(() => {
				if (browser) {
					localStorage.setItem(key, JSON.stringify(this.value));
				}
			})
		})
	}
}