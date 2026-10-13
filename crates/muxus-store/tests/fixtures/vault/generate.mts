// Creates password-vault fixtures with the TypeScript PasswordVault.
// Run from the TypeScript checkout's server directory, with a scratch HOME:
//   [MUXUS_TS_ROOT=/path/to/muxus] HOME=/tmp/scratch ./node_modules/.bin/tsx generate.mts <outDir>
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { zstdCompressSync } from 'node:zlib';

const root = process.env.MUXUS_TS_ROOT ?? new URL('../../../../../../muxus', import.meta.url).pathname;
const { MuxusDatabase } = await import(`${root}/server/src/persistence/database.ts`);
const { PasswordVault, sshPasswordAccount, folderPasswordAccount } = await import(`${root}/server/src/security/password-vault.ts`);
const { MemoryVaultKeyStore } = await import(`${root}/server/src/security/vault-key-store.ts`);

const outDir = process.argv[2]!;
mkdirSync(outDir, { recursive: true });
const work = mkdtempSync(path.join(process.env.SCRATCH ?? os.tmpdir(), 'vaultfx-'));

async function make(name: string, policy: 'never' | 'startup' | 'credential', kdf?: { cost: number; blockSize: number; parallelism: number }) {
  const file = path.join(work, `${name}.sqlite3`);
  const db = new MuxusDatabase(file);
  const keyStore = new MemoryVaultKeyStore();
  const vault = new PasswordVault(db, { kdf, keyStore });
  await vault.initialize();
  const master = 'master-pass-12';
  await vault.create(master, policy);
  const account = sshPasswordAccount({ user: 'alice', host: 'Router.Example', port: 22 });
  await vault.rememberSshPassword(account, 'alice@router.example:22', 'remote-pässword-🔑', master);
  const folder = folderPasswordAccount('folder-row-1');
  await vault.rememberSshPassword(folder, 'Folder Prod', 'folder-secret', master);
  const secret = await vault.createSecret({ name: 'Core enable', username: 'admin', value: 'enable-secret' }, master);
  const config = db.passwordVaultConfig();
  const osKey = await keyStore.get(config.vaultId);
  const expected = {
    masterPassword: master,
    policy,
    vaultId: config.vaultId,
    kdf: { cost: config.kdfCost, blockSize: config.kdfBlockSize, parallelism: config.kdfParallelism },
    osKey: osKey ? osKey.toString('base64') : null,
    sshAccount: account,
    sshPassword: 'remote-pässword-🔑',
    folderAccount: folder,
    folderPassword: 'folder-secret',
    secretId: secret.id,
    secretName: secret.name,
    secretUsername: secret.username,
    secretValue: 'enable-secret',
    status: vault.status(),
  };
  vault.dispose();
  db.close();
  const raw = new DatabaseSync(file);
  raw.exec('PRAGMA journal_mode = DELETE; VACUUM;');
  raw.close();
  writeFileSync(path.join(outDir, `${name}.sqlite3.zst`), zstdCompressSync(readFileSync(file)));
  writeFileSync(path.join(outDir, `${name}.expected.json`), `${JSON.stringify(expected, null, 1)}\n`);
}

// A fast KDF for the startup-policy vault, the real default for the never-policy one.
await make('ts-vault-startup', 'startup', { cost: 1024, blockSize: 8, parallelism: 1 });
await make('ts-vault-never-default-kdf', 'never');
rmSync(work, { recursive: true, force: true });
console.log('ok', outDir);
