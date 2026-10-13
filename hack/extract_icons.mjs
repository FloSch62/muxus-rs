// Extract the Material icons the Muxus client uses into assets/icons as SVG.
//
//   node hack/extract_icons.mjs ../muxus
//
// @mui/icons-material (MIT) ships every icon as a module that calls
// createSvgIcon(<path …/>). Loading the modules with createSvgIcon and the JSX
// runtime swapped for recorders gives the exact element tree, which is written
// out as a 24×24 SVG. The list is every icon imported anywhere in
// client/src plus the ones MUI components draw internally, so later ports
// find theirs already here. The Muxus logo is copied
// from client/public.
import fs from 'node:fs';
import path from 'node:path';
import Module from 'node:module';

const muxus = path.resolve(process.argv[2] ?? '../muxus');
const out = path.resolve(path.dirname(new URL(import.meta.url).pathname), '..', 'assets', 'icons');
fs.mkdirSync(out, { recursive: true });

const pnpm = path.join(muxus, 'node_modules', '.pnpm');
const pkgDir = fs
  .readdirSync(pnpm)
  .filter((name) => name.startsWith('@mui+icons-material@'))
  .sort()
  .map((name) => path.join(pnpm, name, 'node_modules', '@mui', 'icons-material'))
  .pop();
if (!pkgDir) throw new Error('@mui/icons-material is not installed; run pnpm install in ' + muxus);

/** Every `@mui/icons-material/<Name>` imported by the client. */
function usedIcons(dir) {
  const names = new Set();
  const walk = (current) => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const full = path.join(current, entry.name);
      if (entry.isDirectory()) walk(full);
      else if (/\.(tsx?|mts)$/.test(entry.name)) {
        for (const match of fs.readFileSync(full, 'utf8').matchAll(/@mui\/icons-material\/(\w+)/g)) {
          names.add(match[1]);
        }
      }
    }
  };
  walk(dir);
  return [...names].sort();
}

const element = (type, props) => ({ type, props: props ?? {} });
const originalLoad = Module._load;
Module._load = function load(request, parent, isMain) {
  if (request === 'react/jsx-runtime') return { jsx: element, jsxs: element };
  if (request.endsWith('/utils/createSvgIcon')) return { default: (node, name) => ({ node, name }) };
  if (request === '@babel/runtime/helpers/interopRequireDefault') {
    return { default: (value) => value };
  }
  return originalLoad.call(this, request, parent, isMain);
};
const require = Module.createRequire(path.join(pkgDir, 'index.js'));

const ATTRS = {
  d: 'd',
  cx: 'cx',
  cy: 'cy',
  r: 'r',
  x: 'x',
  y: 'y',
  width: 'width',
  height: 'height',
  rx: 'rx',
  ry: 'ry',
  points: 'points',
  opacity: 'opacity',
  fillOpacity: 'fill-opacity',
  fillRule: 'fill-rule',
  clipRule: 'clip-rule',
  transform: 'transform',
};

function render(node) {
  if (Array.isArray(node)) return node.map(render).join('');
  if (!node || typeof node !== 'object') return '';
  const { children, ...props } = node.props;
  const attrs = Object.entries(props)
    .filter(([key]) => ATTRS[key])
    .map(([key, value]) => ` ${ATTRS[key]}="${String(value)}"`)
    .join('');
  const inner = children === undefined ? '' : render(children);
  return inner ? `<${node.type}${attrs}>${inner}</${node.type}>` : `<${node.type}${attrs}/>`;
}

/** Icons MUI components draw internally (Checkbox, Radio, Alert, Select).
 *  The Window*.svg glyphs and TabPin.svg in assets/icons are drawn by hand. */
const INTERNAL = [
  'ArrowDropDown',
  'CheckBox',
  'CheckBoxOutlineBlank',
  'CheckCircleOutlined',
  'ErrorOutlineOutlined',
  'IndeterminateCheckBox',
  'InfoOutlined',
  'RadioButtonChecked',
  'RadioButtonUnchecked',
  'ReportProblemOutlined',
];
const names = [...new Set([...usedIcons(path.join(muxus, 'client', 'src')), ...INTERNAL])].sort();
for (const name of names) {
  const icon = require(`./${name}.js`).default;
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24" fill="currentColor">${render(icon.node)}</svg>\n`;
  fs.writeFileSync(path.join(out, `${name}.svg`), svg);
}
fs.copyFileSync(path.join(muxus, 'client', 'public', 'muxus.svg'), path.join(out, 'muxus.svg'));
fs.copyFileSync(path.join(pkgDir, 'LICENSE'), path.join(out, 'LICENSE-material-icons.txt'));
console.log(`${names.length} icons written to ${out}`);
