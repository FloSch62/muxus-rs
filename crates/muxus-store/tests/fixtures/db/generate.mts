// Generates the database fixtures with the TypeScript MuxusDatabase.
// Run from the TypeScript checkout's server directory, with a scratch HOME:
//   [MUXUS_TS_ROOT=/path/to/muxus] HOME=/tmp/scratch ./node_modules/.bin/tsx generate.mts <outDir>
import { mkdirSync, readFileSync, rmSync, writeFileSync, copyFileSync, mkdtempSync } from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { DatabaseSync } from 'node:sqlite';
import { zstdCompressSync } from 'node:zlib';
const { MuxusDatabase } = await import(`${process.env.MUXUS_TS_ROOT ?? new URL('../../../../../../muxus', import.meta.url).pathname}/server/src/persistence/database.ts`);

const outDir = process.argv[2]!;
mkdirSync(outDir, { recursive: true });
const work = mkdtempSync(path.join(process.env.SCRATCH ?? os.tmpdir(), 'dbfx-'));

function schema(file: string) {
  const db = new DatabaseSync(file);
  try {
    return {
      userVersion: (db.prepare('PRAGMA user_version').get() as { user_version: number }).user_version,
      migrations: db.prepare('SELECT version, name FROM schema_migrations ORDER BY version').all(),
      objects: db.prepare('SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY rowid').all(),
    };
  } finally {
    db.close();
  }
}

function b64(value: unknown): unknown {
  if (Buffer.isBuffer(value)) return value.toString('base64');
  if (value instanceof Map) return Object.fromEntries([...value].map(([k, v]) => [k, b64(v)]));
  if (Array.isArray(value)) return value.map(b64);
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).filter(([, v]) => v !== undefined).map(([k, v]) => [k, b64(v)]));
  }
  return value;
}

// 1. Fresh database right after the constructor ran every migration.
const freshFile = path.join(work, 'fresh.sqlite3');
new MuxusDatabase(freshFile).close();
writeFileSync(path.join(outDir, 'ts-fresh-schema.json'), `${JSON.stringify(schema(freshFile), null, 1)}\n`);

// 2. The same after the app separated session history out of it.
const finalFile = path.join(work, 'final.sqlite3');
copyFileSync(freshFile, finalFile);
{
  const db = new MuxusDatabase(finalFile);
  db.finalizeSessionHistorySeparation(false);
  db.close();
}
writeFileSync(path.join(outDir, 'ts-finalized-schema.json'), `${JSON.stringify(schema(finalFile), null, 1)}\n`);

// 3. A populated database and what every read returns for it. Inputs list
// their keys in the order the route schemas output them, as the app stores them.
const popFile = path.join(work, 'populated.sqlite3');
const db = new MuxusDatabase(popFile);
const loginSequence = {
  steps: [
    { id: 'w1', kind: 'wait' as const, pattern: 'Password:', timeoutSeconds: 10 },
    { id: 's1', kind: 'secret' as const, secretId: 'secret-1', enter: true },
    { id: 't1', kind: 'send' as const, text: 'terminal length 0', enter: true },
  ],
};
db.updateOpenSshMetadata('production', {
  displayName: 'Production',
  group: 'Work/EU',
  color: '#3b82f6',
  icon: 'server',
  terminalScheme: 'nord',
  terminalFontColor: '#f8f8f2',
  terminalBackgroundColor: '#282a36',
  keywordHighlights: {
    inheritGlobal: false,
    profileId: 'builtin-cisco',
    rules: [{ id: 'r1', name: 'Errors', keyword: 'ERROR', foreground: '#ffffff', background: '#b91c1c', caseSensitive: false, wholeWord: true, regex: false }],
  },
  commandButtonGroup: 'command-group-juniper',
  pasteLineDelayMs: 250,
  pasteCharDelayMs: 0,
  disableSftp: true,
  consoleCompatibility: true,
  loginSequence,
});
db.recordOpenSshConnection('production');
db.updateOpenSshMetadata('bastion', { group: 'work/eu' });
db.updateOpenSshMetadata('old-name', { color: '#ef4444' });
db.renameOpenSshAlias('old-name', 'new-name');

const ssh = db.saveSavedHostProfile({
  id: 'saved-ssh',
  name: 'Router',
  profile: {
    kind: 'ssh', target: 'router.lab', useConfig: false, user: 'admin', port: 2222,
    identityFiles: ['~/.ssh/id_ed25519'], identitiesOnly: true, proxyJump: ['bastion'],
    forwards: [{ type: 'local', bindPort: 8080, targetHost: 'localhost', targetPort: 80 }, { type: 'dynamic', bindPort: 1080 }],
    passwordOnly: true, requestTty: 'force', strictHostKeyChecking: 'accept-new',
  },
});
const telnet = db.saveSavedHostProfile({ name: 'Switch', profile: { kind: 'telnet', host: 'switch.lab', port: 23 } });
const serial = db.saveSavedHostProfile({
  name: 'Console',
  profile: { kind: 'serial', path: '/dev/ttyUSB0', baudRate: 9600, dataBits: 7, stopBits: 1.5, parity: 'even', flowControl: 'hardware', breakDurationMs: 500 },
});
const rdp = db.saveSavedHostProfile({
  name: 'Build server',
  profile: { kind: 'rdp', host: 'win-build', port: 3389, username: 'ci', domain: 'LAB', sshGateway: { target: 'bastion' }, shareClipboard: false },
});
const vnc = db.saveSavedHostProfile({ name: 'Lab VM', profile: { kind: 'vnc', host: 'lab-vm', port: 5901, sshGateway: { target: 'router.lab', profileId: 'saved-ssh' }, viewOnly: true } });
const gnmi = db.saveSavedHostProfile({ name: 'leaf1', profile: { kind: 'gnmi', host: 'leaf1', port: 57400, username: 'admin', tls: 'skip-verify', caFile: '/lab/ca.pem', encoding: 'json_ietf' } });
const netconf = db.saveSavedHostProfile({ name: 'leaf1 netconf', profile: { kind: 'netconf', host: 'leaf1', port: 830 } });
db.updateSavedHostMetadata(ssh.id, { displayName: 'Core router', group: 'Work/EU', loginSequence: { steps: [] } });
db.updateSavedHostMetadata(telnet.id, { group: 'Lab', pasteCharDelayMs: 5 });
db.recordSavedHostConnection(ssh.id);
db.reorderManagedHosts([
  { kind: 'ssh', alias: 'bastion' },
  { kind: 'profile', id: ssh.id },
  { kind: 'ssh', alias: 'production' },
]);

const layout = {
  version: 1,
  root: {
    id: 'split-1', type: 'split', direction: 'horizontal', ratio: 0.5,
    children: [
      { id: 'pane-1', type: 'pane', tabs: [{ id: 'tab-1', kind: 'terminal', title: 'Router', profile: { kind: 'ssh', target: 'router' }, color: 'red', pinned: true, offerReconnect: true }], activeTabId: 'tab-1' },
      { id: 'pane-2', type: 'pane', tabs: [{ id: 'tab-2', kind: 'terminal', title: 'Local', profile: { kind: 'local', cwd: '/tmp' }, cwdHint: '/tmp', offerReconnect: true }, { id: 'tab-3', kind: 'sftp', title: 'Files', connection: { source: 'openssh', id: 'router' }, path: '/var/log' }] },
    ],
  },
  activePaneId: 'pane-1',
};
const daily = db.saveWorkspace({ id: 'ws-daily', name: 'Daily', layout, multiExecGroups: [{ id: 'g1', name: 'Both', tabIds: ['tab-1', 'tab-2'] }] });
const empty = db.saveWorkspace({ id: 'ws-empty', name: 'Empty', layout: { version: 1, root: null } });
db.setWorkspaceLocked(empty.id, true);
db.setStartupWorkspace(daily.id);
db.openWorkspace(daily.id);
db.saveTerminalSnapshot('tab-1', '\u001b[1mbold\u001b[0m\r\n', 2);

const t1 = db.saveTunnel({ id: 'tunnel-db', name: 'DB', target: 'db.internal', sshOptions: { user: 'deploy', port: 2222, identityFiles: ['~/.ssh/work'], proxyJump: ['bastion'] }, type: 'local', bindPort: 5432, targetHost: 'localhost', targetPort: 5432, autoStart: true });
const t2 = db.saveTunnel({ id: 'tunnel-socks', target: 'bastion', type: 'dynamic', bindPort: 1080, autoReconnect: true });

db.saveSessionLoggingPolicy('*', { enabled: true, captureInput: false, maxPartBytes: 1048576, maxParts: 4, logToFile: true });
db.saveSessionLoggingPolicy('ssh:production', { enabled: false, captureInput: true, maxPartBytes: 2097152, maxParts: 2 });
db.saveSessionHistorySettings({ storageLocation: '/srv/history', maxTotalBytes: 134217728, minFreeBytes: 0, minFreePercent: 2.5, maxAgeDays: 30 });
db.saveSessionLogFileSettings({ directory: '/srv/logs', filenamePattern: '{kind}/{host}_{date}.log', timestamps: true });

const eu = db.upsertFolderSettings('Work/EU', { user: 'ops', port: 2200, identityFiles: ['~/.ssh/eu'], identitiesOnly: true, identityAgent: 'none', forwardAgent: false });
db.setFolderLoginSequence(eu.id, loginSequence);
db.upsertFolderSettings(' Lab ', { user: 'root' });

db.createPasswordVaultConfig({
  formatVersion: 3, vaultId: 'vault-id-0123456789', unlockPolicy: 'startup', kdfAlgorithm: 'scrypt',
  kdfSalt: Buffer.alloc(16, 1), kdfCost: 1024, kdfBlockSize: 8, kdfParallelism: 1,
  masterKeyNonce: Buffer.alloc(12, 2), masterKeyCiphertext: Buffer.alloc(32, 3), masterKeyTag: Buffer.alloc(16, 4), keyCheck: Buffer.alloc(32, 5),
});
db.upsertEncryptedCredential({ provider: 'muxus-master-vault', service: 'muxus/ssh-password/v1', account: 'acct-1', label: 'admin@router:22', formatVersion: 1, nonce: Buffer.alloc(12, 6), ciphertext: Buffer.from('cipher'), authTag: Buffer.alloc(16, 7) });
db.upsertEncryptedCredential({ provider: 'muxus-master-vault', service: 'muxus/named-secret/v1', account: 'acct-2', label: 'enable', username: 'admin', formatVersion: 1, nonce: Buffer.alloc(12, 8), ciphertext: Buffer.from('enable-cipher'), authTag: Buffer.alloc(16, 9) });
db.queuePasswordVaultKeyCleanup('cleanup-vault-id-12');
db.trustDesktopIdentity({ host: 'Win-Build', port: 3389, gateway: 'ssh:bastion', fingerprint: 'AA:BB', subject: 'CN=win-build' });
db.trustDesktopIdentity({ host: 'lab-vm', port: 5901, fingerprint: 'CC:DD', subject: 'RSA 2048' });
db.saveManagementRequest({ id: 'req-own', protocol: 'gnmi', profileId: gnmi.id, name: 'Counters', request: { operation: 'subscribe', paths: ['/interface[name=*]/statistics'], options: { mode: 'stream', sample: 10 } } });
db.saveManagementRequest({ id: 'req-shared', protocol: 'gnmi', name: 'System', request: { operation: 'get', paths: ['/system'] } });

const expected = {
  appliedMigrations: db.appliedMigrations(),
  openSshMetadata: db.openSshMetadata(['production', 'bastion', 'new-name', 'old-name', 'missing']),
  groupForAlias: db.groupForAlias('production') ?? null,
  groupForSavedHost: db.groupForSavedHost(ssh.id) ?? null,
  sftpDisabledForAlias: db.sftpDisabledForAlias('production'),
  consoleCompatibilityForSavedHost: db.consoleCompatibilityForSavedHost(ssh.id),
  savedHosts: db.listSavedHostProfiles(),
  savedHost: db.savedHostProfile(vnc.id),
  workspaceSummaries: db.listWorkspaceSummaries(),
  workspaces: db.listWorkspaces(),
  latestWorkspace: db.latestWorkspace(),
  startupWorkspace: db.startupWorkspace(),
  snapshot: db.terminalSnapshot('tab-1'),
  tunnels: db.listTunnels(),
  tunnel: db.tunnel(t2.id),
  policyExact: db.sessionLoggingPolicy('ssh:production'),
  policyInherited: db.sessionLoggingPolicy('ssh:other'),
  policyDefaults: db.sessionLoggingPolicy('*'),
  historySettings: db.sessionHistorySettings(),
  logFileSettings: db.sessionLogFileSettings(),
  folders: db.listFolderSettings(),
  folder: db.folderSettingsForPath('work/eu'),
  hostLoginSequenceAlias: db.hostLoginSequence({ alias: 'production' }),
  hostLoginSequenceSaved: db.hostLoginSequence({ profileId: ssh.id }),
  vaultConfig: db.passwordVaultConfig(),
  sshPasswords: db.listEncryptedCredentials('muxus-master-vault', 'muxus/ssh-password/v1'),
  namedSecrets: db.listEncryptedCredentials('muxus-master-vault', 'muxus/named-secret/v1'),
  pendingCleanup: db.pendingPasswordVaultKeyCleanup(),
  desktopIdentity: db.trustedDesktopIdentity('win-build', 3389, 'ssh:bastion'),
  managementRequests: db.listManagementRequests('gnmi', gnmi.id),
  managementShared: db.listManagementRequests('gnmi'),
  ids: { ssh: ssh.id, telnet: telnet.id, serial: serial.id, rdp: rdp.id, vnc: vnc.id, gnmi: gnmi.id, netconf: netconf.id, tunnel: t1.id },
};
db.close();
writeFileSync(path.join(outDir, 'ts-populated.expected.json'), `${JSON.stringify(b64(expected), null, 1)}\n`);
// Compact the file before compressing it.
{
  const raw = new DatabaseSync(popFile);
  raw.exec('PRAGMA journal_mode = DELETE; VACUUM;');
  raw.close();
}
writeFileSync(path.join(outDir, 'ts-populated.sqlite3.zst'), zstdCompressSync(readFileSync(popFile)));
writeFileSync(path.join(outDir, 'ts-populated-schema.json'), `${JSON.stringify(schema(popFile), null, 1)}\n`);
rmSync(work, { recursive: true, force: true });
console.log('ok', outDir);
