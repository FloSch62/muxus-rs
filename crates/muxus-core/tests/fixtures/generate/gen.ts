// Writes the golden JSON fixtures for muxus-core: what the TypeScript's zod
// schemas output (JSON.stringify of the parsed value) for a set of inputs,
// and which inputs they reject. Expects the TypeScript checkout beside this
// repository (../muxus); run from its server directory:
//   ./node_modules/.bin/tsx --tsconfig <this dir>/tsconfig.json <this dir>/gen.ts
import { writeFileSync } from 'node:fs';
import {
  desktopClientMessageSchema,
  managementClientMessageSchema,
  sessionProfileSchema,
  terminalClientMessageSchema,
} from '../../../../../../muxus/shared/src/ws-protocol.ts';
import { metadataPatchSchema } from '../../../../../../muxus/server/src/routes/metadata-schema.ts';
import { workspaceSaveSchema } from './workspaces.ts';
import { tunnelSchema } from './tunnels.ts';
import { savedProfileSchema } from './profiles.ts';

const schemas = {
  sessionProfile: sessionProfileSchema,
  terminalClient: terminalClientMessageSchema,
  desktopClient: desktopClientMessageSchema,
  managementClient: managementClientMessageSchema,
  metadataPatch: metadataPatchSchema,
  workspaceSave: workspaceSaveSchema,
  tunnel: tunnelSchema,
  savedProfile: savedProfileSchema,
} as const;

type SchemaName = keyof typeof schemas;
interface Case {
  name: string;
  schema: SchemaName | 'terminalServer' | 'desktopServer' | 'managementServer';
  input: unknown;
  output?: string;
  rejected?: boolean;
}

const cases: Case[] = [];
const ok = (schema: SchemaName, name: string, input: unknown) => {
  const parsed = schemas[schema].safeParse(input);
  if (!parsed.success) throw new Error(`${schema}/${name} rejected: ${parsed.error.issues[0]?.message}`);
  cases.push({ name, schema, input, output: JSON.stringify(parsed.data) });
};
const reject = (schema: SchemaName, name: string, input: unknown) => {
  if (schemas[schema].safeParse(input).success) throw new Error(`${schema}/${name} accepted`);
  cases.push({ name, schema, input, rejected: true });
};
// Server messages have no schema: the backend builds them as object literals.
const server = (schema: Case['schema'], name: string, message: unknown) => {
  cases.push({ name, schema, input: message, output: JSON.stringify(message) });
};

const fingerprint = 'SHA256:nThbg6kXUpJWGl7E1IGOCspRomTxdCARLviKw6E5SY8';

// Session profiles of every kind, with zod's defaults and trimming applied.
ok('sessionProfile', 'local-full', {
  startupCommand: 'echo hi',
  cwd: '/tmp',
  args: ['-l', 'with space'],
  shell: '/bin/zsh',
  kind: 'local',
});
ok('sessionProfile', 'ssh-full', {
  hostKeyFingerprint: fingerprint,
  strictHostKeyChecking: 'accept-new',
  requestTty: 'force',
  remoteCommand: 'tmux attach',
  passwordOnly: true,
  forwards: [
    { targetPort: 80, targetHost: 'web', bindPort: 8080, type: 'local', note: 'dropped' },
    { type: 'dynamic', bindPort: 1080 },
  ],
  proxyCommand: 'nc %h %p',
  proxyJump: ['bastion', 'ops@relay:2200', 'muxus-host:p2'],
  forwardX11: false,
  forwardAgent: true,
  identityAgent: 'SSH_AUTH_SOCK',
  identitiesOnly: true,
  certificateFiles: ['~/.ssh/id-cert.pub'],
  identityFiles: ['~/.ssh/id_ed25519'],
  port: 2222,
  user: 'alice',
  keepaliveIntervalSeconds: 45,
  useConfig: false,
  target: ' web.example.com ',
  profileId: 'p1',
  kind: 'ssh',
  term: 'xterm-kitty',
});
ok('sessionProfile', 'ssh-minimal', { kind: 'ssh', target: 'web' });
ok('sessionProfile', 'telnet-default-port', { kind: 'telnet', host: '  router.local  ' });
ok('sessionProfile', 'serial-defaults', { kind: 'serial', path: ' /dev/ttyUSB0 ' });
ok('sessionProfile', 'serial-full', {
  breakDurationMs: 500,
  flowControl: 'software',
  parity: 'mark',
  stopBits: 1.5,
  dataBits: 7,
  baudRate: 9600,
  path: 'COM3',
  kind: 'serial',
  profileId: 's1',
});
ok('sessionProfile', 'rdp', {
  kind: 'rdp',
  host: ' win.example.com ',
  username: ' CORP\\alice ',
  domain: ' CORP ',
  sshGateway: { profileId: 'p1', target: ' bastion ' },
  shareClipboard: false,
});
ok('sessionProfile', 'vnc', {
  kind: 'vnc',
  host: 'desk',
  port: 5901,
  username: 'bob',
  resizeRemote: true,
  viewOnly: true,
  shareClipboard: false,
});
ok('sessionProfile', 'gnmi', {
  encoding: 'json_ietf',
  sshGateway: { target: 'jump' },
  keyFile: ' /k.pem ',
  certFile: '/c.pem',
  caFile: '/ca.pem',
  tlsServerName: ' srl1 ',
  tls: 'skip-verify',
  username: 'admin',
  host: 'srl1',
  kind: 'gnmi',
});
ok('sessionProfile', 'netconf-default-port', { kind: 'netconf', host: 'sros1', username: ' admin ' });
reject('sessionProfile', 'ssh-without-target', { kind: 'ssh' });
reject('sessionProfile', 'telnet-port-zero', { kind: 'telnet', host: 'x', port: 0 });
reject('sessionProfile', 'serial-nine-data-bits', { kind: 'serial', path: 'COM3', dataBits: 9 });
reject('sessionProfile', 'serial-stop-bits-three', { kind: 'serial', path: 'COM3', stopBits: 3 });
reject('sessionProfile', 'gnmi-unknown-encoding', { kind: 'gnmi', host: 'x', encoding: 'xml' });
reject('sessionProfile', 'rdp-blank-host', { kind: 'rdp', host: '   ' });
reject('sessionProfile', 'local-too-many-args', { kind: 'local', args: Array.from({ length: 65 }, () => 'a') });
reject('sessionProfile', 'ssh-lowercase-fingerprint', { kind: 'ssh', target: 'x', hostKeyFingerprint: fingerprint.toLowerCase() });
reject('sessionProfile', 'unknown-kind', { kind: 'mosh', target: 'x' });

// Terminal session messages.
ok('terminalClient', 'connect', {
  rows: 24,
  cols: 80,
  cwd: '/srv/app',
  title: '  Production  ',
  freshTransport: 'fresh-1',
  profile: { target: 'web', kind: 'ssh' },
  op: 'connect',
});
ok('terminalClient', 'dial', { op: 'dial', profile: { kind: 'ssh', target: 'tunnel-host', useConfig: false } });
ok('terminalClient', 'attach', { rows: 40, cols: 120, terminalId: 't-1', op: 'attach' });
ok('terminalClient', 'resize', { op: 'resize', cols: 100, rows: 30 });
ok('terminalClient', 'auth-response', { op: 'auth-response', skipped: false, rememberPassword: true, answers: ['hunter2', ''] });
ok('terminalClient', 'host-key-response', { op: 'host-key-response', accept: false });
ok('terminalClient', 'set-logging', {
  op: 'set-logging',
  logFilePath: '  /tmp/router.log  ',
  logToFile: true,
  captureInput: false,
  paused: true,
  enabled: true,
});
ok('terminalClient', 'set-title', { op: 'set-title', title: ' Renamed ' });
ok('terminalClient', 'paste', { op: 'paste', charDelayMs: 5, lineDelayMs: 250, bracketed: true, text: 'conf t\nend\n' });
for (const op of ['prepare-transfer', 'cancel-transfer', 'paste-cancel', 'send-break', 'cancel-login-sequence']) {
  ok('terminalClient', op, { op, extra: 'dropped' });
}
reject('terminalClient', 'set-logging-path-only', { op: 'set-logging', logFilePath: '/tmp/x.log' });
reject('terminalClient', 'connect-zero-cols', { op: 'connect', profile: { kind: 'local' }, cols: 0, rows: 24 });
reject('terminalClient', 'connect-desktop-profile', { op: 'connect', profile: { kind: 'rdp', host: 'x' }, cols: 80, rows: 24 });
reject('terminalClient', 'paste-char-delay-too-long', { op: 'paste', text: 'x', bracketed: false, lineDelayMs: 0, charDelayMs: 1001 });
reject('terminalClient', 'dial-telnet', { op: 'dial', profile: { kind: 'telnet', host: 'x', target: 'x' } });
reject('terminalClient', 'unknown-op', { op: 'exec', cmd: 'id' });
reject('terminalClient', 'attach-long-id', { op: 'attach', terminalId: 'x'.repeat(201), cols: 1, rows: 1 });

// Remote desktop messages.
ok('desktopClient', 'connect', { op: 'connect', profile: { kind: 'vnc', host: ' desk ' } });
ok('desktopClient', 'retry', { op: 'retry', rejected: true });
ok('desktopClient', 'credentials-request', { op: 'credentials-request', types: ['username', 'password'] });
ok('desktopClient', 'connected', { op: 'connected' });
ok('desktopClient', 'certificate-response', { op: 'certificate-response', accept: true });
ok('desktopClient', 'server-key', {
  op: 'server-key',
  signature: '01-23-45-67-89-ab-cd-ef',
  fingerprint: Array.from({ length: 32 }, (_, i) => i.toString(16).toUpperCase().padStart(2, '0')).join(':'),
  bits: 2048,
});
reject('desktopClient', 'server-key-lowercase', {
  op: 'server-key',
  signature: '01-23-45-67-89-ab-cd-ef',
  fingerprint: Array.from({ length: 32 }, () => 'ab').join(':'),
  bits: 2048,
});
reject('desktopClient', 'credentials-request-empty', { op: 'credentials-request', types: [] });
reject('desktopClient', 'connect-terminal-profile', { op: 'connect', profile: { kind: 'ssh', target: 'x' } });

// Management messages.
ok('managementClient', 'connect', { op: 'connect', profile: { kind: 'gnmi', host: 'srl1' } });
ok('managementClient', 'gnmi-get-defaults', { op: 'gnmi-get', id: 'r1', paths: ['/system/name'] });
ok('managementClient', 'gnmi-get-full', {
  maxBytes: 4096,
  depth: 2,
  encoding: 'proto',
  type: 'state',
  paths: ['/interface[name=e1]', '/system'],
  prefix: '/',
  id: 'r2',
  op: 'gnmi-get',
});
ok('managementClient', 'gnmi-set', {
  op: 'gnmi-set',
  id: 'r3',
  updates: [{ encoding: 'json_ietf', value: '{"a":1}', path: '/system/name' }],
  commit: { rollbackSeconds: 60, id: 'c1', action: 'commit' },
});
ok('managementClient', 'gnmi-set-confirm', { op: 'gnmi-set', id: 'r4', deletes: ['/a'], commit: { action: 'confirm', id: 'c1' } });
ok('managementClient', 'gnmi-subscribe', {
  op: 'gnmi-subscribe',
  id: 'r5',
  mode: 'stream',
  updatesOnly: true,
  subscriptions: [{ path: '/interface' }, { heartbeatIntervalMs: 1000, suppressRedundant: true, sampleIntervalMs: 500, mode: 'sample', path: '/cpu' }],
});
ok('managementClient', 'gnmi-poll', { op: 'gnmi-poll', id: 'r5' });
ok('managementClient', 'netconf-rpc', { op: 'netconf-rpc', timeoutMs: 5000, xml: '<get/>', id: 'n1' });
ok('managementClient', 'grpc-call', { op: 'grpc-call', id: 'g1', method: '/gnoi.system.System/Time', request: { b: 1, a: { z: true, y: [1, 'x'] } } });
ok('managementClient', 'grpc-send', { op: 'grpc-send', id: 'g2', message: { finalize: {} } });
ok('managementClient', 'gnoi-file-put', { op: 'gnoi-file-put', id: 'f1', size: 10, permissions: 644, remoteFile: '/tmp/x' });
ok('managementClient', 'gnoi-file-chunk', { op: 'gnoi-file-chunk', id: 'f1', data: 'aGk=', last: true });
ok('managementClient', 'cancel', { op: 'cancel', id: 'r5' });
reject('managementClient', 'gnmi-get-no-paths', { op: 'gnmi-get', id: 'r1', paths: [] });
reject('managementClient', 'netconf-short-timeout', { op: 'netconf-rpc', id: 'n1', xml: '<get/>', timeoutMs: 10 });
reject('managementClient', 'file-put-bad-permissions', { op: 'gnoi-file-put', id: 'f1', size: 1, remoteFile: '/x', permissions: 8000 });
reject('managementClient', 'long-request-id', { op: 'cancel', id: 'x'.repeat(65) });
reject('managementClient', 'commit-long-rollback', { op: 'gnmi-set', id: 'r', commit: { action: 'commit', id: 'c', rollbackSeconds: 86_401 } });

// Host metadata patches, including login sequences and keyword highlighting.
ok('metadataPatch', 'full', {
  loginSequence: {
    steps: [
      { timeoutSeconds: 10, regex: true, pattern: '[Pp]assword:', kind: 'wait', id: 's1' },
      { enter: true, secretId: 'v1', kind: 'secret', id: 's2' },
      { enter: true, text: '', kind: 'send', id: 's3' },
      { enter: false, text: 'terminal length 0', kind: 'send', id: 's4' },
    ],
  },
  consoleCompatibility: false,
  disableSftp: true,
  pasteCharDelayMs: null,
  pasteLineDelayMs: 200,
  commandButtonGroup: 'g1',
  keywordHighlights: {
    rules: [
      { regex: true, wholeWord: false, caseSensitive: true, background: '#000000', foreground: '#FF0000', keyword: 'err(or)?', name: 'Errors', id: 'r1' },
      { wholeWord: true, caseSensitive: false, foreground: '#00ff00', keyword: 'ok', id: 'r2' },
    ],
    profileId: 'cisco',
    inheritGlobal: false,
  },
  terminalBackgroundColor: null,
  terminalFontColor: '#abcdef',
  terminalScheme: 'dracula',
  icon: null,
  color: '#ff8800',
  group: 'Production/EU',
  displayName: null,
});
ok('metadataPatch', 'clear-sequence', { loginSequence: null, group: null });
ok('metadataPatch', 'empty-sequence', { loginSequence: { steps: [] } });
reject('metadataPatch', 'empty', {});
reject('metadataPatch', 'named-color-font', { terminalFontColor: 'red' });
reject('metadataPatch', 'line-delay-too-long', { pasteLineDelayMs: 10_001 });
reject('metadataPatch', 'invalid-wait-regex', { loginSequence: { steps: [{ id: 'a', kind: 'wait', pattern: '(', regex: true, timeoutSeconds: 5 }] } });
reject('metadataPatch', 'send-without-text-or-enter', { loginSequence: { steps: [{ id: 'a', kind: 'send', text: '', enter: false }] } });
reject('metadataPatch', 'too-many-steps', {
  loginSequence: { steps: Array.from({ length: 33 }, (_, i) => ({ id: `s${i}`, kind: 'send', text: 'x', enter: true })) },
});
reject('metadataPatch', 'wait-timeout-zero', { loginSequence: { steps: [{ id: 'a', kind: 'wait', pattern: 'x', timeoutSeconds: 0 }] } });
reject('metadataPatch', 'group-too-long', { group: 'g'.repeat(301) });
reject('metadataPatch', 'rule-without-keyword', {
  keywordHighlights: { inheritGlobal: true, rules: [{ id: 'r', keyword: '', foreground: '#000000', caseSensitive: true, wholeWord: true }] },
});

// Workspace saves.
const sshTab = {
  offerReconnect: true,
  pinned: true,
  color: 'red',
  cwdHint: '/srv',
  profile: { target: 'web', kind: 'ssh' },
  title: 'web',
  kind: 'terminal',
  id: 't1',
};
ok('workspaceSave', 'split-layout', {
  multiExecGroups: [{ tabIds: ['t1', 't4'], name: '  Both ', id: 'g1' }],
  layout: {
    activePaneId: 'p1',
    root: {
      children: [
        { activeTabId: 't1', tabs: [sshTab, { offerReconnect: true, profile: { kind: 'local' }, title: 'shell', kind: 'terminal', id: 't4' }], type: 'pane', id: 'p1' },
        {
          tabs: [
            { path: '/var/log', connection: { id: 'web', source: 'openssh' }, title: 'files', kind: 'sftp', id: 't2' },
            { connection: { id: 'p9', source: 'profile' }, title: 'editor', kind: 'editor', id: 't3' },
          ],
          type: 'pane',
          id: 'p2',
        },
      ],
      ratio: 0.5,
      direction: 'vertical',
      type: 'split',
      id: 's1',
    },
    version: 1,
  },
  name: '  Dev  ',
  id: 'w1',
});
ok('workspaceSave', 'empty-default-name', { name: 'x', layout: { version: 1, root: null }, allocateDefaultName: true });
ok('workspaceSave', 'create-if-missing', { id: 'w2', name: 'New', createIfMissing: true, overwriteLocked: true, layout: { version: 1, root: null } });
reject('workspaceSave', 'duplicate-tab-id', {
  name: 'x',
  layout: { version: 1, root: { id: 'p', type: 'pane', tabs: [sshTab, sshTab] } },
});
reject('workspaceSave', 'ratio-out-of-range', {
  name: 'x',
  layout: {
    version: 1,
    root: { id: 's', type: 'split', direction: 'horizontal', ratio: 0.95, children: [{ id: 'a', type: 'pane', tabs: [] }, { id: 'b', type: 'pane', tabs: [] }] },
  },
});
reject('workspaceSave', 'group-unknown-tab', {
  name: 'x',
  layout: { version: 1, root: { id: 'p', type: 'pane', tabs: [sshTab] } },
  multiExecGroups: [{ id: 'g', name: 'g', tabIds: ['t1', 'zz'] }],
});
reject('workspaceSave', 'create-without-id', { name: 'x', createIfMissing: true, layout: { version: 1, root: null } });
reject('workspaceSave', 'active-tab-elsewhere', {
  name: 'x',
  layout: { version: 1, root: { id: 'p', type: 'pane', tabs: [sshTab], activeTabId: 'nope' } },
});
reject('workspaceSave', 'active-pane-missing', { name: 'x', layout: { version: 1, root: { id: 'p', type: 'pane', tabs: [] }, activePaneId: 'q' } });
reject('workspaceSave', 'version-two', { name: 'x', layout: { version: 2, root: null } });
reject('workspaceSave', 'blank-name', { name: '   ', layout: { version: 1, root: null } });

// Saved tunnels.
ok('tunnel', 'local-with-options', {
  autoReconnect: true,
  autoStart: false,
  targetPort: 5432,
  targetHost: 'db.internal',
  bindPort: 15432,
  type: 'local',
  sshOptions: { passwordOnly: false, proxyJump: [' bastion '], forwardAgent: true, identitiesOnly: true, identityFiles: [' ~/.ssh/id '], port: 2222, user: ' ops ' },
  target: ' db-gateway ',
  name: 'Postgres',
  id: 't1',
});
ok('tunnel', 'dynamic-empty-options', { type: 'dynamic', bindPort: 1080, target: 'proxy', sshOptions: {} });
reject('tunnel', 'local-without-target', { type: 'local', bindPort: 8080, target: 'web' });
reject('tunnel', 'unknown-ssh-option', { type: 'dynamic', bindPort: 1080, target: 'x', sshOptions: { password: 'secret' } });
reject('tunnel', 'bind-port-zero', { type: 'dynamic', bindPort: 0, target: 'x' });

// Saved hosts: the keepalive fallback and a link fingerprint never persist.
ok('savedProfile', 'ssh', {
  profile: { hostKeyFingerprint: fingerprint, keepaliveIntervalSeconds: 30, useConfig: false, user: 'root', target: '10.0.0.1', kind: 'ssh', profileId: 'ignored' },
  name: '  Lab router  ',
  id: 'h1',
});
ok('savedProfile', 'telnet', { name: 'Switch', profile: { kind: 'telnet', host: 'switch' } });
ok('savedProfile', 'netconf', { name: 'SR OS', profile: { kind: 'netconf', host: 'sros', username: 'admin' } });
reject('savedProfile', 'local-kind', { name: 'x', profile: { kind: 'local' } });
reject('savedProfile', 'blank-name', { name: '  ', profile: { kind: 'telnet', host: 'x' } });

// Server messages, built the way the backend builds them.
server('terminalServer', 'session', { op: 'session', terminalId: 't-1' });
server('terminalServer', 'status', { op: 'status', message: 'Connecting to web …', transient: true });
server('terminalServer', 'connection-health', { op: 'connection-health', state: 'suspect' });
server('terminalServer', 'auth-prompt', {
  op: 'auth-prompt',
  name: 'web',
  host: 'alice@web',
  prompts: [{ prompt: 'Password: ', echo: false }],
  purpose: 'ssh-password',
  rememberPassword: { label: 'alice@web:22', existing: false },
});
server('terminalServer', 'host-key', {
  op: 'host-key',
  host: 'web',
  port: 22,
  keyType: 'ssh-ed25519',
  fingerprint,
  state: 'mismatch',
  previous: 'SHA256:old',
  hop: 'bastion',
});
server('terminalServer', 'ready-dial', { op: 'ready', connId: 'c1', host: 'web', user: 'alice', sftpAvailable: true });
server('terminalServer', 'ready-summary', {
  op: 'ready',
  connId: 'c1',
  host: 'web',
  user: 'alice',
  sftpAvailable: true,
  summary: {
    user: 'alice',
    host: 'web',
    port: 22,
    jumpHosts: ['bastion'],
    proxyCommand: false,
    shared: false,
    serverSoftware: 'OpenSSH_9.6p1',
    authMethods: ['publickey'],
    authKeyAlgorithm: 'ssh-ed25519',
    cipher: 'chacha20-poly1305@openssh.com',
    kex: 'curve25519-sha256',
    compression: 'off',
    sftp: true,
    x11: 'no-server',
    agentForwarding: 'no-agent',
    forwards: [{ type: 'local', bindPort: 8080, targetHost: 'localhost', targetPort: 80 }],
  },
});
server('terminalServer', 'logging-state', { op: 'logging-state', enabled: true, sessionId: 's1', paused: false, captureInput: false, filePath: '/logs/web.log' });
server('terminalServer', 'break-result', { op: 'break-result', ok: false, message: 'not supported' });
server('terminalServer', 'exit', { op: 'exit', code: 0, reason: 'completed' });
server('terminalServer', 'paste-progress', { op: 'paste-progress', state: 'running', line: 2, lines: 10, sent: 30, total: 200 });
server('terminalServer', 'login-sequence', { op: 'login-sequence', state: 'failed', step: 1, steps: 3, detail: 'Waiting for “Password:”', message: 'timed out' });
server('desktopServer', 'certificate', {
  op: 'certificate',
  host: 'win',
  port: 3389,
  fingerprint: 'AA:BB',
  state: 'new',
  kind: 'certificate',
  subject: 'CN=win',
  issuer: 'CN=win',
  validFrom: '2026-01-01T00:00:00.000Z',
  validTo: '2027-01-01T00:00:00.000Z',
  verificationError: 'self-signed certificate',
});
server('desktopServer', 'ready', {
  op: 'ready',
  ticket: 'tk',
  profile: { kind: 'rdp', host: 'win', port: 3389 },
  credentials: { username: 'alice', password: 'x', domain: 'CORP' },
});
server('desktopServer', 'exit', { op: 'exit', message: 'closed', reason: 'disconnected' });
server('managementServer', 'ready-netconf', {
  op: 'ready',
  info: {
    protocol: 'netconf',
    address: 'admin@sros:830',
    connectedAt: '2026-01-01T00:00:00.000Z',
    sessionId: '7',
    base: '1.1',
    capabilities: ['urn:ietf:params:netconf:base:1.1'],
  },
  profile: { kind: 'netconf', host: 'sros', port: 830 },
});
server('managementServer', 'result', {
  op: 'result',
  id: 'r1',
  result: { op: 'gnmi-get', notifications: [{ timestamp: '1700000000000000000', prefix: '/', updates: [{ path: '/system/name', value: { type: 'json_ietf', value: { name: 'srl1' } } }], deletes: [] }] },
  durationMs: 12,
  bytes: 100,
});
server('managementServer', 'stream-end', { op: 'stream-end', id: 'r2', error: { message: 'cancelled', code: 'CANCELLED', cancelled: true } });

writeFileSync(process.argv[2]!, `[\n${cases.map((c) => JSON.stringify(c)).join(',\n')}\n]\n`);
console.log(`${cases.length} cases`);
