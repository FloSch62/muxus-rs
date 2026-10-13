// Opens a vault this build exported (see `exports_a_vault_for_the_typescript_build`)
// with the TypeScript PasswordVault and checks every value.
//   [MUXUS_TS_ROOT=/path/to/muxus] HOME=/tmp/scratch ./node_modules/.bin/tsx verify.mts <dir>
import path from 'node:path';
import { readFileSync } from 'node:fs';

const root = process.env.MUXUS_TS_ROOT ?? new URL('../../../../../../muxus', import.meta.url).pathname;
const { MuxusDatabase } = await import(`${root}/server/src/persistence/database.ts`);
const { PasswordVault } = await import(`${root}/server/src/security/password-vault.ts`);
const { MemoryVaultKeyStore } = await import(`${root}/server/src/security/vault-key-store.ts`);

const dir = process.argv[2]!;
const expected = JSON.parse(readFileSync(path.join(dir, 'expected.json'), 'utf8'));
const db = new MuxusDatabase(path.join(dir, 'muxus.sqlite3'));
const vault = new PasswordVault(db, { keyStore: new MemoryVaultKeyStore() });
await vault.initialize();
await vault.unlockForSession(expected.masterPassword);
const results = {
  sshPassword: (await vault.sshPassword(expected.sshAccount)) === expected.sshPassword,
  secretValue: (await vault.secretValue(expected.secretId)) === expected.secretValue,
  revealed: (await vault.revealCredential(expected.secretId, expected.masterPassword)) === expected.secretValue,
  secrets: vault.status().secrets,
};
console.log(JSON.stringify(results));
vault.dispose();
db.close();
if (!results.sshPassword || !results.secretValue || !results.revealed) process.exit(1);
