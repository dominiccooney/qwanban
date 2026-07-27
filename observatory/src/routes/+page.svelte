<script lang="ts">
import HostChit from '$lib/components/HostChit.svelte';
import HostSession from '$lib/components/HostSession.svelte';
import { LocalStore } from '$lib/localStore.svelte';
import {Host} from "$lib/Host.svelte.js";
import {SvelteMap} from "svelte/reactivity";

function autofocus(element: HTMLElement): void {
    element.focus();
}

const hostNames = new LocalStore<string[]>('hosts', []);

function addHost(host: string): void {
    if (!hostNames.value.includes(host)) {
        hostNames.value.push(host);
    }
}

function removeHost(host: string): void {
    hostNames.value = hostNames.value.filter((value: string) => value !== host);
}

let hosts: Map<string, Host> = new SvelteMap<string, Host>();

// Reconcile the set of named hosts and the host objects representing them.
$effect(() => {
    const namedHosts = new Set<string>(hostNames.value);
    const hostObjectNames = new Set<string>(hosts.keys());
    const deleted = hostObjectNames.difference(namedHosts);
    const added = namedHosts.difference(hostObjectNames);
    console.log(`${deleted.size} deleted`);
    console.log(`${added.size} added`);
    for (const name of deleted) {
        hosts.get(name)!.close();
        hosts.delete(name);
    }
    for (const name of added) {
        hosts.set(name, new Host(name));
    }
    console.log(`${hostNames.value.length} host names; ${hosts.size} hosts`);
});

let hostTextInput: HTMLInputElement | undefined = $state(undefined);
let hostText : string = $state('');

// Which host's session view is open. Keyed by name; the Host object is
// looked up live so a removed host closes its own session view.
let openHostName: string | undefined = $state(undefined);
let openHost: Host | undefined = $derived(openHostName !== undefined ? hosts.get(openHostName) : undefined);
</script>
<style>
#screen-grid {
    display: grid;
    grid-template-columns: 33% 33% 33%;
    gap: 0.5em;
}
</style>
<h1>Observatory</h1>
{#if openHost}
    <HostSession host={openHost} onClose={() => { openHostName = undefined; }} />
{:else}
    <input bind:this={hostTextInput} type="text" bind:value={hostText} use:autofocus>
    <button onclick={() => { addHost(hostText); hostText = ''; hostTextInput?.focus(); }}>Add Host</button>
    <p>{hosts.size} host(s)</p>
    <div id="screen-grid">
    {#each hosts.values() as host (host.name)}
        <HostChit {host} onOpen={() => { openHostName = host.name; }}>
            {#snippet controls()}
                <button onclick={() => removeHost(host.name)}>&cross;</button>
            {/snippet}
        </HostChit>
    {/each}
    </div>
{/if}