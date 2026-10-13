// Writes the golden fixture for muxus-import: what the TypeScript importers,
// helpers and command-line parser return for a set of inputs. Expects the
// TypeScript checkout beside this repository (../muxus); run from its client
// directory:
//   ./node_modules/.bin/tsx --tsconfig <this dir>/tsconfig.json <this dir>/gen.ts
import { writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  mobaXtermConnections,
  parseMobaXtermSessions,
} from '../../../../../../muxus/client/src/mobaxterm-import.ts';
import {
  parseSecureCrtSessions,
  secureCrtConnections,
} from '../../../../../../muxus/client/src/securecrt-import.ts';
import {
  cleanImportName,
  normalizeImportFolder,
  stableImportId,
  uniqueImportAlias,
} from '../../../../../../muxus/client/src/session-import.ts';
import {
  parseCommandLineLaunch,
  parseCommandLineLaunchData,
  parseConnectTarget,
} from '../../../../../../muxus/electron/src/command-line.ts';

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(path.resolve(here, '../../../../../../muxus/tests/package.json'));
const { DOMParser: XmlDomParser } = require('@xmldom/xmldom');

// The same strict parser the TypeScript unit tests install.
class StrictTestDomParser {
  parseFromString(text: string, mimeType: string) {
    let failed = false;
    const parser = new XmlDomParser({
      onError: () => {
        failed = true;
      },
    });
    const document = parser.parseFromString(text, mimeType);
    if (failed) throw new Error('malformed XML');
    return document;
  }
}
(globalThis as { DOMParser?: unknown }).DOMParser = StrictTestDomParser;

type Outcome = { ok: unknown; openssh?: unknown; muxus?: unknown } | { error: string };

function run(
  parse: (text: string) => { sessions: never[] },
  connections: (sessions: never[], storage: 'openssh' | 'muxus') => unknown,
  text: string,
): Outcome {
  try {
    const parsed = parse(text);
    return {
      ok: parsed,
      openssh: connections(parsed.sessions, 'openssh'),
      muxus: connections(parsed.sessions, 'muxus'),
    };
  } catch (error) {
    return { error: (error as Error).message };
  }
}

const mobaxterm: Record<string, string> = {
  fieldsFoldersAuth: `
[Misc]
LastSession=Do not import|#109#0%ignored.example.com%22%root%%rest

[Bookmarks]
SubRep=Production\\Europe
Prod SSH=#109#0%prod.example.com%2222%deploy%%rest

[Bookmarks_1]
SubRep=Lab
Key box=#109#0%key.example.com%22%root%3%rest
`,
  duplicateNames: `
[Bookmarks]
SubRep=
My host!=#109#0%one.example.com%not-a-port%%%rest
My host!=#109#0%two.example.com%22%%%rest
`,
  unsupported: `
[Bookmarks]
Telnet=#98#1%switch.example.com%23%%%2%%22%%%0#MobaFont%10
WSL=#105#14%Ubuntu%
Broken SSH=#109#0%%22%root%%
Broken RDP=#91#4%%3389%admin%0
Valid=#109#0%valid.example.com%22%root%%
`,
  nothingImportable: `
[Bookmarks]
SubRep=
Telnet=#98#1%switch.example.com%23%%%2%%22%%%0
`,
  keysAndGateways: `
[Bookmarks]
SubRep=Lab
Direct key=#109#0%direct.example.com%22%admin%%-1%-1%%%%%0%0%0%_ProfileDir_\\ssh_keys\\id_ecdsa%%-1%0%0%0%%1080%%0%0%1#MobaFont%10%0%0%-1%15%236,236,236
Via bastion=#109#0%10.0.0.5%22%root%%-1%-1%%bastion.example.com%2222%jump%0%0%0%%_CurrentDrive_:\\keys\\bastion.ppk%-1%0%0%0%%1080%%0%0%1#MobaFont%10
Two hops=#109#0%10.0.1.5%22%root%%-1%-1%%bastion.example.com__PIPE__inner.example.com%2222__PIPE__22%jump__PIPE__ops%0%0%0%%_CurrentDrive_:\\keys\\bastion.ppk__PIPE__%-1%0%0%0%%1080%%0%0%1#MobaFont%10
`,
  executeCommand: `
[Bookmarks]
Root=#109#0%a.example.com%22%admin%%-1%-1%sudo su -__PTVIRG__ echo done__PTVIRG__%%%%0%0%0%%%-1%0%0%0%%1080%%0%0%1#MobaFont%10
Nested=#109#0%b.example.com%22%root%%-1%-1%echo pw __PIPE__ sshpass -p pw ssh -oHostKeyAlgorithms__EQUAL__+ssh-rsa admin@10.0.0.1%%%%-1%0%0%%%-1%0%0%0%%1080%%0%0%1#MobaFont%10
Plain=#109#0%c.example.com%22%root%%-1%-1%%%%%-1%0%0%%%-1%0%0%0%%1080%%0%0%1#MobaFont%10
Hash=#109#0%d.example.com%22%root%%-1%-1% echo __DIEZE__1 ; ;  %%%% -1 %0%0%%%-1#MobaFont%10
Only separators=#109#0%e.example.com%22%root%%-1%-1%__PTVIRG__ __PTVIRG__%%%%-1%0#MobaFont%10
`,
  desktops: `
[Bookmarks]
SubRep=Desktops
Windows=#91#4%win.example.com%3390%Administrator%0%-1%-1%-1%-1%0%0%-1%%bastion.example.com%22%root%0%0%_ProfileDir_\\keys\\id_ed25519%-1%%-1%-1%0%-1%0%-1#MobaFont%10%0
Console=#128#5%127.0.0.1%5901%-1%0%bastion.example.com%22%root%%-1#MobaFont%10%0%0%-1%15%236,236,236
Plain VNC=#128#5%vnc.example.com%%-1%0%%22%%%-1#MobaFont%10
`,
  conversion: `
[Bookmarks]
SubRep=Customers\\Acme
Password host=#109#0%pw.example.com%2200%alice%%rest
Key host=#109#0%key.example.com%22%bob%3%rest
`,
  gatewayFile: `
[Bookmarks]
SubRep=Lab
Inner=#109#0%10.0.1.5%22%root%%-1%-1%%bastion.example.com__PIPE__inner.example.com%2222__PIPE__22%jump__PIPE__ops%0%0%0%_ProfileDir_\\keys\\lab%_ProfileDir_\\keys\\bastion__PIPE__%-1%0%0%0%%1080%%0%0%1#MobaFont%10
Windows=#91#4%win.example.com%3389%admin%0%-1%-1%-1%-1%0%0%-1%%bastion.example.com%2222%jump%0%0%_ProfileDir_\\keys\\bastion%-1%%-1%-1%0%-1%0%-1#MobaFont%10
`,
  edgeCases:
    '\uFEFF[bookmarks]\r\n' +
    '; a comment\r\n' +
    'SubRep= Ops \\\\ Core\u0001 // Edge \r\n' +
    'no separator line\r\n' +
    'Not a session=just a value\r\n' +
    '=#109#0%unnamed.example.com%22%root%%\r\n' +
    '=#91#4%%3389%%\r\n' +
    'Leading zeros=#109#0004%rdp.example.com% 3390abc%admin%%\r\n' +
    'Signed port=#109#0%signed.example.com%+2201%  spaced user  %%\r\n' +
    'Zero port=#109#0%zero.example.com%0%%\r\n' +
    'Big port=#109#0%big.example.com%65536%%\r\n' +
    'Huge protocol=#109#99999999999999999999%x.example.com%22\r\n' +
    'Häuser & Wörter #1 *?!=#109#0%unicode.example.com%22%%\r\n' +
    `${'L'.repeat(230)}=#109#0%long.example.com%22%%\r\n` +
    'Dup desktop=#91#4%a.example.com%3389%%\r\n' +
    'Dup desktop=#91#4%b.example.com%3389%%\r\n' +
    'Dup desktop=#128#5%c.example.com%5900%\r\n' +
    'jump-root-bastion.example.com=#109#0%taken.example.com%22%%\r\n' +
    'Gateway A=#109#0%ga.example.com%22%%%-1%-1%%bastion.example.com%22%root%0%0%0%%_MyDocuments_\\key%\r\n' +
    'Gateway B=#109#0%gb.example.com%22%%%-1%-1%%bastion.example.com%22%root%0%0%0%%%\r\n' +
    'Gateway C=#91#4%gc.example.com%3389%%0%-1%-1%-1%-1%0%0%-1%%bastion.example.com%22%root%0%0%%\r\n' +
    'Sparse hops=#109#0%sh.example.com%22%%%-1%-1%%a.example.com__PIPE____PIPE__c.example.com%22__PIPE__x__PIPE__2200%__PIPE__u2__PIPE__u3%0%0%0%%%\r\n' +
    'IPv6 hop=#109#0%v6.example.com%22%%%-1%-1%%fe80::1%2022%adm@in%0%0%0%%%\r\n' +
    '  Spaced name  =  #109#0%  spaced.example.com  %22%%  \r\n' +
    'Key flag=#109#0%flag.example.com%22%%  3  %\r\n' +
    '[Bookmarks_x]\r\n' +
    'Ignored=#109#0%ignored.example.com%22%%\r\n' +
    '[ Bookmarks_7 ]\r\n' +
    'Section seven=#109#0%seven.example.com%22%%\r\n' +
    '[Bookmarks_8]\r\n' +
    'SubRep=\\\\\r\n' +
    'Root again=#109#0%root.example.com%22%%\r\n',
  noBookmarks: '[Misc]\nSession=#109#0%x.example.com%22%%\n',
};

const secureCrtExport = `<?xml version="1.0" encoding="UTF-8"?>
<VanDyke version="3.0">
  <key name="Security"><string name="Passphrase">encrypted-secret</string></key>
  <key name="Sessions">
    <key name="Customers &amp; labs">
      <key name="Europe">
        <key name="Core router">
          <dword name="Is Session">1</dword>
          <string name="Protocol Name">SSH2</string>
          <string name="Hostname">router.example.test</string>
          <string name="Username">deploy</string>
          <dword name="[SSH2] Port">2222</dword>
          <string name="SSH2 Authentications V2">keyboard-interactive,password,gssapi</string>
          <string name="Password V2">encrypted-password</string>
          <string name="Identity Filename V2">/private/key/that/must/not/import</string>
        </key>
        <key name="Core router">
          <dword name="Is Session">1</dword>
          <string name="Protocol Name">SSH2</string>
          <string name="Hostname">key.example.test</string>
          <string name="Username">root</string>
          <dword name="[SSH2] Port">70000</dword>
          <string name="SSH2 Authentications V2">publickey,password</string>
        </key>
      </key>
      <key name="Bench console">
        <dword name="Is Session">1</dword>
        <string name="Protocol Name">Serial</string>
        <string name="Mac Com Port">/dev/cu.usbserial-1234</string>
        <dword name="Mac Baud Rate">9600</dword>
        <dword name="Mac Data Bits">7</dword>
        <dword name="Mac Stop Bits">2</dword>
        <dword name="Mac Parity">2</dword>
        <dword name="Mac CTS Flow">1</dword>
        <dword name="Mac XON Flow">1</dword>
      </key>
      <key name="Empty folder"/>
    </key>
    <key name="Local terminal">
      <dword name="Is Session">1</dword>
      <string name="Protocol Name">Local Shell</string>
    </key>
    <key name="Broken SSH">
      <dword name="Is Session">1</dword>
      <string name="Protocol Name">SSH2</string>
      <string name="Hostname"/>
    </key>
  </key>
  <key name="Files">
    <key name="File 0"><binary name="Contents">private-key-bytes</binary></key>
  </key>
</VanDyke>`;

const secureCrt: Record<string, string> = {
  export: secureCrtExport,
  unprefixedSerial: `
      <VanDyke><key name="Sessions"><key name="Console">
        <dword name="Is Session">1</dword>
        <string name="Protocol Name">Serial</string>
        <string name="Com Port">COM4</string>
        <dword name="Baud Rate">115200</dword>
        <dword name="Data Bits">8</dword>
        <dword name="Stop Bits">1</dword>
        <dword name="Parity">4</dword>
        <dword name="XON Flow">1</dword>
      </key></key></VanDyke>
    `,
  doctype: '<!DOCTYPE x><VanDyke/>',
  entity: '<VanDyke><!-- x --><! entity y></VanDyke>',
  otherRoot: '<Other/>',
  noSessions: '<VanDyke/>',
  malformed: '<VanDyke><key></VanDyke>',
  unsupportedOnly: `
        <VanDyke><key name="Sessions"><key name="Shell">
          <dword name="Is Session">1</dword>
          <string name="Protocol Name">Local Shell</string>
        </key></key></VanDyke>
      `,
  parserErrorElement:
    '<VanDyke><key name="Sessions"><parsererror/><key name="A"><dword name="Is Session">1</dword></key></key></VanDyke>',
  prefixedRoot: '<x:VanDyke xmlns:x="urn:x"><key name="Sessions"/></x:VanDyke>',
  edgeCases: `<VanDyke>
  <key name="Ignored first"/>
  <key name="Sessions">
    <key name="A/B\\C">
      <key name="">
        <key name="Nested">
          <dword name="Is Session">1</dword>
          <string name="Protocol Name">ssh2</string>
          <string name="Hostname"><![CDATA[cdata.example.test]]></string>
          <string name="Username">us"er</string>
          <dword name="[SSH2] Port">22abc</dword>
          <string name="SSH2 Authentications V2"> PublicKey </string>
        </key>
      </key>
      <key name="Nested">
        <dword name="Is Session">1</dword>
        <string name="Protocol Name">SSH2</string>
        <string name="Hostname">second.example.test</string>
        <dword name="[SSH2] Port">0</dword>
        <string name="SSH2 Authentications V2">gssapi</string>
      </key>
    </key>
    <key name="Dup">
      <dword name="Is Session">1</dword>
      <string name="Protocol Name">SSH2</string>
      <string name="Hostname">dup1.example.test</string>
      <string name="SSH2 Authentications V2">password</string>
    </key>
    <key name="Dup">
      <dword name="Is Session">1</dword>
      <string name="Protocol Name">SSH2</string>
      <string name="Hostname">dup2.example.test</string>
      <string name="SSH2 Authentications V2">keyboard-interactive</string>
    </key>
    <key name="Dup">
      <dword name="Is Session">1</dword>
      <string name="Protocol Name">Serial</string>
      <string name="Port">/dev/ttyS0</string>
      <string name="Com Port">COM9</string>
      <dword name="Baud Rate">0</dword>
      <dword name="Data Bits">9</dword>
      <dword name="Stop Bits">0</dword>
      <dword name="Parity">3</dword>
      <dword name="DTR Flow Control">2</dword>
    </key>
    <key name="Dup">
      <dword name="Is Session">1</dword>
      <string name="Protocol Name">Serial</string>
      <string name="Com Port">COM10</string>
      <dword name="Baud Rate">12000001</dword>
      <dword name="Parity">-1</dword>
      <dword name="RTS Flow Control">2</dword>
    </key>
    <key name="DSR">
      <dword name="Is Session">1</dword>
      <string name="Protocol Name">Serial</string>
      <string name="Com Port">COM11</string>
      <dword name="DSR Flow">1</dword>
      <dword name="Data Bits">5</dword>
    </key>
    <key name="No port">
      <dword name="Is Session">1</dword>
      <string name="Protocol Name">Serial</string>
    </key>
    <key name="  ">
      <dword name="Is Session">1</dword>
      <string name="Protocol Name">SSH2</string>
      <string name="Hostname">nameless.example.test</string>
    </key>
    <key>
      <dword name="Is Session">1</dword>
      <string name="Protocol Name">SSH2</string>
      <string name="Hostname">"quoted"</string>
    </key>
    <key name="Telnet box">
      <dword name="Is Session">1</dword>
      <string name="Protocol Name">Telnet</string>
    </key>
    <key name="No protocol">
      <dword name="Is Session">1</dword>
    </key>
    <key name="Not a session">
      <dword name="Is Session">0</dword>
      <key name="Child &lt;1&gt;">
        <dword name="Is Session">1</dword>
        <string name="Protocol Name">SSH2</string>
        <string name="Hostname">child.example.test</string>
        <string name="Username">  </string>
      </key>
    </key>
  </key>
</VanDyke>`,
};

const ids = [
  ['x', ''],
  ['mobaxterm-session', '\0web'],
  ['mobaxterm-jump', '\0bastion\0' + '2222\0jump\0'],
  ['securecrt-serial', 'A\0B'],
  ['p', 'Häuser 🚀 \u{10FFFF}'],
] as const;

const folders = [
  'Production\\Europe',
  ' / a //\u0001b/ ',
  '',
  ' \\ / ',
  `${'a'.repeat(299)}/b`,
  `${'é'.repeat(150)}/${'ü'.repeat(150)}`,
  `x/${'🚀'.repeat(160)}`,
];
const names = [' \u007fa\u0001b ', 'x'.repeat(250), `${'x'.repeat(199)} y`, `${'🚀'.repeat(101)}`, '\u0085c1\u0085'];
const aliases: Array<[string, string, string[], string]> = [
  ['My host!', 'h', [], 'x'],
  ['  #*  ', 'a b', [], 'x'],
  ['', '', [], 'fallback'],
  ['web', '', ['web', 'web-2'], 'x'],
  ['--a--b--', '', [], 'x'],
  ['a\u3000\u00a0b\tc', '', [], 'x'],
  [`${'🚀'.repeat(121)}`, '', [], 'x'],
];

const collation = [
  'b', 'A', 'a', 'B', 'ab', 'Aa', 'e', 'é', 'E', 'f', 'É', 'ß', 'ss', 'st',
  '1', '10', '9', '_', ' ', '-', 'a-b', 'a b', 'a_b', 'a.b', 'a/b', 'a~b', 'a$b',
  'Lab', 'lab', 'Production', 'Production/EU', 'Production/EU/Core', 'Prod',
  'Ungrouped', 'Ungrouped · lab', 'Hosts', 'edge-1', 'Edge-2', 'edge10', 'edge2',
  'z', 'Z', 'zz', '', '!', '#', '@', '[x]', '(x)', '{x}', "'q'", '"q"', 'a,b', 'a;b',
  'a:b', 'a?b', 'a&b', 'a%b', 'a`b', 'a^b', 'a+b', 'a<b', 'a=b', 'a>b', 'a|b', 'a\\b', 'a*b',
];

const argvCases: string[][] = [
  ['muxus', '--host', 'edge-1'],
  ['electron', '.', '--inspect=9229', '--folder=Production/EU'],
  ['muxus', '--workspace', 'Night shift'],
  ['muxus'],
  ['muxus', '--no-sandbox'],
  ['muxus', '--host'],
  ['muxus', '--host='],
  ['muxus', '--host', '--workspace', 'Lab'],
  ['muxus', '--host', 'edge', '--folder', 'Lab'],
  ['muxus', `--host=${'x'.repeat(501)}`],
  ['muxus', `--host=${'x'.repeat(500)}`],
  ['muxus', '--host', '  spaced  '],
  ['muxus', '--connect', 'admin@10.10.10.1'],
  ['muxus', '--connect=admin@10.10.10.1:2222'],
  ['muxus', '--connect', 'edge-1.example.test'],
  ['muxus', '--connect', '10.10.10.1', '--user', 'admin', '--port', '2222'],
  ['muxus', '--port=2222', '--user=admin', '--connect=10.10.10.1'],
  ['muxus', '--connect', 'edge', '--port', '0022'],
  ['muxus', '--connect', 'admin@edge', '--user', 'root'],
  ['muxus', '--connect', 'edge:22', '--port', '2222'],
  ['muxus', '--connect', 'edge', '--port', '22', '--port', '23'],
  ['muxus', '--host', 'edge', '--port', '22'],
  ['muxus', '--user', 'admin'],
  ['muxus', '--connect', 'edge', '--user', 'a|b'],
  ['/opt/Muxus/muxus', 'ssh://admin@10.0.0.1:2222'],
  ['C:\\Muxus\\Muxus.exe', 'telnet://switch:23'],
  ['electron', '/src/muxus/electron', '--no-sandbox', 'ssh://edge'],
  ['muxus', 'ssh://-oProxyCommand=id'],
  ['muxus', 'sftp://host'],
  ['muxus', 'ssh:host'],
  ['muxus', 'SSH:host'],
  ['muxus', `ssh://${'a'.repeat(5000)}`],
  ['ssh://not-a-link'],
  ['muxus', '--host', 'ssh://edge'],
  ['muxus', '--inspect=ssh://x'],
  ['muxus', 'ssh://a', 'ssh://b'],
  ['muxus', '--host', 'edge', 'ssh://a'],
  ['muxus', 'ssh://a', '--user', 'root'],
  ['muxus', '--connect', '[fe80::1]:22'],
  ['muxus', '--connect', 'fe80::1', '--port', '22'],
  ['muxus', '--connect=  admin@edge  '],
  ['muxus', 'plain-argument', '--host', 'x'],
];

const launchData: unknown[] = [
  { kind: 'workspace', name: ' Night shift ' },
  { kind: 'host', name: 'edge-1' },
  { kind: 'unknown', name: 'edge-1' },
  { kind: 'folder', name: '' },
  { kind: 'folder', name: '   ' },
  { kind: 'connect', name: 'admin@10.10.10.1:2222' },
  { kind: 'connect', name: '[fe80::1]:2222' },
  { kind: 'connect', name: 'edge;reboot' },
  { kind: 'connect', name: ' edge ' },
  { kind: 'url', name: 'ssh://edge' },
  { kind: 'url', name: '--host=edge' },
  { kind: 'url', name: 42 },
  { kind: 'host', name: 'x'.repeat(501) },
  { kind: 'host' },
  null,
  'host',
  [],
];

const connectTargets: Array<[string, { user?: string; port?: string }]> = [
  ['fe80::1', {}],
  ['admin@fe80::1', { port: '2222' }],
  ['[2001:db8::5]:830', {}],
  ['[2001:db8::5]', {}],
  ['fe80::1%eth0', { port: '22' }],
  ['[edge]:22', {}],
  ['[edge]:', {}],
  ['[]:22', {}],
  ['[a]b', {}],
  ['a@b@edge', {}],
  ...[
    'edge:0', 'edge:65536', 'edge:', 'edge:ssh', '@edge', 'admin@', '-oProxyCommand=sh',
    'admin@-edge', 'edge;reboot', 'edge$(id)', 'ad min@edge', '`id`@edge', '[edge;id]:22',
    'edge:00022', 'edge:123456', '.edge', 'a'.repeat(501),
  ].map((target): [string, { user?: string; port?: string }] => [target, {}]),
  ['edge', { user: '-oProxyCommand=sh' }],
  ['edge', { port: '22;id' }],
  ['edge', { user: 'root', port: '2200' }],
];

const golden = {
  mobaxterm: Object.entries(mobaxterm).map(([name, input]) => ({
    name,
    input,
    ...run(parseMobaXtermSessions as never, mobaXtermConnections as never, input),
  })),
  securecrt: Object.entries(secureCrt).map(([name, input]) => ({
    name,
    input,
    ...run(parseSecureCrtSessions as never, secureCrtConnections as never, input),
  })),
  ids: ids.map(([prefix, value]) => ({ prefix, value, id: stableImportId(prefix, value) })),
  folders: folders.map((input) => ({ input, output: normalizeImportFolder(input) ?? null })),
  names: names.map((input) => ({ input, output: cleanImportName(input) })),
  aliases: aliases.map(([name, host, used, fallback]) => ({
    name,
    host,
    used,
    fallback,
    output: uniqueImportAlias(name, host, new Set(used), fallback),
  })),
  collation: {
    input: collation,
    sorted: [...collation].sort((a, b) => a.localeCompare(b, 'en')),
  },
  argv: argvCases.map((argv) => ({ argv, launch: parseCommandLineLaunch(argv) ?? null })),
  launchData: launchData.map((value) => ({
    value,
    launch: parseCommandLineLaunchData(value) ?? null,
  })),
  connectTargets: connectTargets.map(([target, options]) => ({
    target,
    options,
    output: parseConnectTarget(target, options) ?? null,
  })),
};

const out = path.resolve(here, '../golden.json');
writeFileSync(out, `${JSON.stringify(golden, null, 1)}\n`);
console.log(`wrote ${out}`);
