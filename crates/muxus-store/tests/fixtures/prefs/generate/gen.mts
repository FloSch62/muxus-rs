// The TypeScript checkout; defaults to ../muxus beside this repository.
const root = process.env.MUXUS_TS_ROOT ?? new URL('../../../../../../../muxus', import.meta.url).pathname;
const saved = new Map<string, string>();
const writes: [string, string][] = [];
const input = process.argv[2];
if (input) saved.set('muxus-prefs', input);
(globalThis as any).window = {
  muxusDesktop: {
    stateStorage: {
      getItem: (name: string) => saved.get(name) ?? null,
      setItem: (name: string, value: string) => { writes.push([name, value]); saved.set(name, value); },
      removeItem: (name: string) => saved.delete(name),
      onChange: () => () => {},
    },
  },
};
const { usePrefsStore } = await import(`${root}/client/src/state/prefs.ts`);
await usePrefsStore.persist.rehydrate();
if (process.argv[3] === 'touch') usePrefsStore.getState().set({});
console.log(JSON.stringify({ writes, state: saved.get('muxus-prefs') ?? null }));
