<script lang="ts">
    import {type Host, HostState} from "$lib/Host.svelte";

type Props = {
    host: Host,
    controls: unknown,
};

let { host, controls }: Props = $props();

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

let state: string = $derived(labelForState(host.state));
</script>

<div>
<button onclick={() => host.send("TakeScreenshot")}>📸</button>
{@render controls()}
{host.name} ({state})
<br>
<img src={host.screenshot} alt="Latest screenshot" width="100%">
</div>
