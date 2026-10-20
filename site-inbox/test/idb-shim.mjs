// A minimal in-memory IndexedDB: enough of open / upgrade / transaction / get / put / delete for
// groups-core.js's idbStorage, with transaction completion delivered asynchronously like a browser.
export function memoryIndexedDB() {
  const dbs = new Map();
  const later = (fn) => setTimeout(fn, 0);
  function request(run) {
    const req = { onsuccess: null, onerror: null, result: undefined, error: null };
    later(() => { try { req.result = run(); req.onsuccess && req.onsuccess(); } catch (e) { req.error = e; req.onerror && req.onerror(); } });
    return req;
  }
  return {
    open(name) {
      const req = { onupgradeneeded: null, onsuccess: null, onerror: null, result: null };
      later(() => {
        let db = dbs.get(name);
        const fresh = !db;
        if (!db) { db = { stores: new Map() }; dbs.set(name, db); }
        const handle = {
          objectStoreNames: { contains: (n) => db.stores.has(n) },
          createObjectStore: (n) => { db.stores.set(n, new Map()); },
          transaction(names) {
            let pending = 0, done = false;
            const tx = { oncomplete: null, onerror: null, onabort: null };
            const finish = () => { if (!done && pending === 0) { done = true; later(() => tx.oncomplete && tx.oncomplete()); } };
            tx.objectStore = (n) => {
              const store = db.stores.get(n);
              const op = (fn) => { pending++; const r = request(fn); const s = r; const orig = () => {}; later(() => later(() => { pending--; finish(); })); void orig; return s; };
              return {
                get: (k) => op(() => store.get(k)),
                put: (v, k) => op(() => { store.set(k, v); return k; }),
                delete: (k) => op(() => { store.delete(k); }),
              };
            };
            later(() => later(() => later(finish)));
            return tx;
          },
        };
        req.result = handle;
        if (fresh && req.onupgradeneeded) req.onupgradeneeded();
        req.onsuccess && req.onsuccess();
      });
      return req;
    },
  };
}
