<script lang="ts">
	type Props = {
		src: string | undefined;
		alt: string;
		/** Fires with natural pixel size whenever the front image finishes loading. */
		onLoad?: (size: { width: number; height: number }) => void;
	};

	let { src, alt, onLoad }: Props = $props();

	// Dual-layer crossfade: keep the previous frame painted while the new one
	// loads, then fade the new frame in so the swap never blanks out.
	let backSrc: string | undefined = $state(undefined);
	let frontSrc: string | undefined = $state(undefined);
	let frontVisible = $state(false);
	let frontEl: HTMLImageElement | undefined = $state(undefined);
	let fadeGeneration = 0;

	$effect(() => {
		const next = src;
		if (next === frontSrc) {
			return;
		}
		if (!next) {
			backSrc = undefined;
			frontSrc = undefined;
			frontVisible = false;
			return;
		}
		// Promote the current front to the back so it stays visible under the
		// incoming frame. First paint has nothing to keep.
		if (frontSrc) {
			backSrc = frontSrc;
		}
		frontSrc = next;
		frontVisible = false;
		fadeGeneration += 1;
	});

	// Blob URLs (and cache hits) can finish loading before `onload` is wired;
	// watch the element and promote as soon as it has dimensions.
	$effect(() => {
		const el = frontEl;
		const expected = frontSrc;
		if (!el || !expected || el.getAttribute('src') !== expected) {
			return;
		}
		if (el.complete && el.naturalWidth > 0) {
			revealFront(el);
		}
	});

	function revealFront(el: HTMLImageElement): void {
		onLoad?.({
			width: el.naturalWidth,
			height: el.naturalHeight
		});
		// Double rAF: wait until the decoded frame is painted at opacity 0,
		// then flip visible so the CSS transition actually runs.
		const gen = fadeGeneration;
		requestAnimationFrame(() => {
			requestAnimationFrame(() => {
				if (gen !== fadeGeneration) {
					return;
				}
				frontVisible = true;
			});
		});
	}

	function handleFrontLoad(): void {
		if (frontEl) {
			revealFront(frontEl);
		}
	}

	function handleFrontTransitionEnd(event: TransitionEvent): void {
		if (event.propertyName !== 'opacity' || !frontVisible) {
			return;
		}
		// Drop the underlay once the new frame is fully opaque.
		backSrc = undefined;
	}
</script>

<div class="crossfade">
	{#if backSrc}
		<img class="layer back" src={backSrc} alt="" draggable="false" />
	{/if}
	{#if frontSrc}
		<img
			bind:this={frontEl}
			class="layer front"
			class:visible={frontVisible}
			class:instant={!backSrc}
			src={frontSrc}
			{alt}
			draggable="false"
			onload={handleFrontLoad}
			ontransitionend={handleFrontTransitionEnd}
		/>
	{/if}
</div>

<style>
	.crossfade {
		position: relative;
		line-height: 0;
		background: #111;
		overflow: hidden;
	}

	.layer {
		width: 100%;
		display: block;
	}

	.layer.back {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		object-fit: fill;
	}

	.layer.front {
		position: relative;
		opacity: 0;
		transition: opacity 0.28s ease;
	}

	.layer.front.instant {
		transition: none;
	}

	.layer.front.visible {
		opacity: 1;
	}
</style>
