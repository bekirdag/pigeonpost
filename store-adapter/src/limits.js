// Per-source request budget for the bridge's unauthenticated routes (sign-in code exchange,
// refresh, handle availability). Each of them makes this service call Keycloak or the postbox, so
// without a budget one address could use the bridge to hammer either. Sliding one-minute window,
// bounded memory; a restart forgets the history, which only ever makes it more permissive.

export function createLimiter({ perMinute = 60, maxSources = 20_000, now = () => Date.now() } = {}) {
  const recent = new Map(); // source -> timestamps within the last minute

  /// Records one request from `source`. Returns 0 when allowed, else the seconds until it would be.
  function take(source) {
    const t = now();
    const since = t - 60_000;
    const list = (recent.get(source) || []).filter((x) => x > since);
    if (list.length >= perMinute) {
      recent.set(source, list);
      return Math.max(1, Math.ceil((list[0] - since) / 1000));
    }
    list.push(t);
    recent.delete(source); // re-insert so the map stays in least-recently-used order
    recent.set(source, list);
    while (recent.size > maxSources) recent.delete(recent.keys().next().value);
    return 0;
  }

  return { take };
}
