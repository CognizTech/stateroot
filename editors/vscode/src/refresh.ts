/** Share one refresh across timer, watcher and user requests. */
export function singleFlight<T>(run: () => Promise<T>): () => Promise<T> {
  let pending: Promise<T> | undefined;
  return () => {
    if (pending) return pending;
    const next = Promise.resolve().then(run);
    pending = next;
    const clear = () => { if (pending === next) pending = undefined; };
    void next.then(clear, clear);
    return next;
  };
}
