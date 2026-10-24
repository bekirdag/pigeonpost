// Loader for the vendored group end-to-end encryption module (pigeonpost-groupcrypto).
//
// Works under the inbox CSP `script-src 'self' 'wasm-unsafe-eval'`: everything is same-origin,
// there is no eval/new Function, and WebAssembly compilation is what 'wasm-unsafe-eval' allows.
// The wasm bytes are checked against the SHA-256 pinned at build time before instantiation, so
// a tampered or stale cached copy is refused instead of run.
//
//   const { loadGroupCrypto } = await import('./vendor/groupcrypto/loader.js');
//   const gc = await loadGroupCrypto();
//   const device = gc.GroupDevice.newWithIdentitySeed(seed, 'web-1');

import init, * as bindings from './pigeonpost_groupcrypto.js?v=f185d0bd6f783430db1edd81aba0a90570a0b93c9bac486a81f9bc90c816de16';
import { WASM_SHA256 } from './integrity.js?v=f185d0bd6f783430db1edd81aba0a90570a0b93c9bac486a81f9bc90c816de16';

let ready = null;

function hex(buffer) {
  return Array.from(new Uint8Array(buffer), (b) => b.toString(16).padStart(2, '0')).join('');
}

async function fetchBytes() {
  const url = new URL('./pigeonpost_groupcrypto_bg.wasm', import.meta.url);
  url.searchParams.set('v', WASM_SHA256);
  const response = await fetch(url, { credentials: 'same-origin', cache: 'no-cache' });
  if (!response.ok) throw new Error(`groupcrypto: wasm fetch failed (${response.status})`);
  return new Uint8Array(await response.arrayBuffer());
}

/**
 * Load, verify and initialise the module once. `options.wasmBytes` lets non-browser hosts
 * (the Node test harness) supply the bytes; they are verified the same way.
 */
export function loadGroupCrypto(options = {}) {
  if (!ready) {
    ready = (async () => {
      const bytes = options.wasmBytes ? new Uint8Array(options.wasmBytes) : await fetchBytes();
      const digest = hex(await globalThis.crypto.subtle.digest('SHA-256', bytes));
      if (digest !== WASM_SHA256) {
        throw new Error('groupcrypto: wasm integrity check failed');
      }
      await init({ module_or_path: bytes });
      return bindings;
    })();
    ready.catch(() => {
      ready = null;
    });
  }
  return ready;
}
