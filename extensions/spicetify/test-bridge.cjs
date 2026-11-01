const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');

let heartbeat;
let observed;
const sent = [];
const page = { getClientRects: () => [1], querySelectorAll: () => nodes };
let spicyPage = null;
let canvasSource = null;
const line = (text, classes) => ({
    classList: { contains: name => classes.includes(name) },
    querySelector: selector => selector === 'p' ? { textContent: text } : null
});
const nodes = [line('Before', []), line('Current', ['lyrics-lyricsContainer-LyricsLine-active']), line('After', [])];
const context = {
    window: { Spicetify: { Player: { data: { item: { metadata: { title: 'Test track', artist_name: 'Test artist' } } } } } },
    document: {
        documentElement: {},
        querySelector: selector => selector.includes('canvas.spicy-dynamic-bg') ? canvasSource : selector.startsWith('#Spicy') ? spicyPage : page,
        querySelectorAll: () => spicyPage ? [spicyPage] : [],
        createElement: () => ({ getContext: () => ({ drawImage() {}, getImageData: () => ({ data: Uint8Array.from(Array(96).fill([30, 60, 90, 255]).flat()) }) }) })
    },
    MutationObserver: class { constructor(callback) { observed = callback; } observe() {} disconnect() {} },
    setInterval: callback => { heartbeat = callback; },
    setTimeout: callback => callback(),
    fetch: (url, options) => { sent.push({ url, options }); return Promise.resolve(); },
    Date, JSON
};
vm.runInNewContext(fs.readFileSync(path.join(__dirname, 'wdock-lyrics-plus.js'), 'utf8'), context);
assert.equal(typeof observed, 'function');
assert.equal(sent.length, 1);
assert.equal(sent[0].url, 'http://127.0.0.1:43179/spotify-lyrics');
assert.deepEqual(JSON.parse(sent[0].options.body), {
    title: 'Test track', artist: 'Test artist', source: 'Lyrics Plus', active: 1, lines: ['Before', 'Current', 'After'], progress: null, colors: []
});
heartbeat();
assert.equal(sent.length, 1, 'unchanged lyrics are not repeatedly sent before the heartbeat');
context.document.hidden = true;
nodes[1] = line('Stale while hidden', ['lyrics-lyricsContainer-LyricsLine-active']);
heartbeat();
assert.equal(sent.length, 1, 'hidden Spotify must not renew a possibly frozen lyric');
context.document.hidden = false;
nodes[1] = line('Current', ['lyrics-lyricsContainer-LyricsLine-active']);
const word = (text, classes = [], gradient = '') => ({ textContent: text, closest: () => null, classList: { contains: name => classes.includes(name) }, style: { getPropertyValue: name => name === '--gradient-position' ? gradient : '' } });
const spicyNodes = [
    { textContent: 'Before', classList: { contains: () => false }, querySelectorAll: () => [word('Before')] },
    { textContent: 'Thuđãquaanhởlại', classList: { contains: name => name === 'Active' },
        querySelectorAll: () => [word('Thu', ['Sung']), word('đã', ['Sung']), word('qua', ['Active']), word('anh'), word('ở'), word('lại')] },
    { textContent: 'Next', classList: { contains: () => false }, querySelectorAll: () => [word('Next')] }
];
spicyPage = { getClientRects: () => [1], querySelectorAll: () => spicyNodes, querySelector: () => spicyNodes[1] };
canvasSource = {};
observed();
assert.equal(sent.length, 2);
assert.deepEqual(JSON.parse(sent[1].options.body), {
    title: 'Test track', artist: 'Test artist', source: 'Spicy Lyrics', active: 1,
    lines: ['Before', 'Thu đã qua anh ở lại', 'Next'], progress: 8.5, colors: Array(96).fill('#1e3c5a')
});
spicyNodes[1].querySelectorAll = () => [word('some', ['Sung', 'PartOfWord']), word('thing', ['Sung']), word('new', ['Active'])];
observed();
heartbeat();
const last = JSON.parse(sent[sent.length - 1].options.body);
assert.equal(last.lines[1], 'something new');
const grouped = word('wait', ['letterGroup']);
grouped.querySelectorAll = () => [word('w', ['Sung']), word('a', ['Sung']), word('i', ['Active']), word('t', ['NotSung'])];
spicyNodes[1].querySelectorAll = () => [grouped, word('please')];
heartbeat();
const emphasized = JSON.parse(sent[sent.length - 1].options.body);
assert.equal(emphasized.lines[1], 'wait please');
assert.equal(emphasized.progress, 2.5);
// Current Spicy Lyrics builds often change inline gradients, not word classes.
spicyNodes[1].querySelectorAll = () => [word('Thu', [], '100%'), word('đã', [], '40%'), word('qua', [], '-20%')];
heartbeat();
const gradient = JSON.parse(sent[sent.length - 1].options.body);
assert.equal(gradient.lines[1], 'Thu đã qua');
assert.equal(gradient.progress, 5);
spicyNodes[1].querySelectorAll = () => [word('Unknown'), word('timing')];
heartbeat();
const unknown = JSON.parse(sent[sent.length - 1].options.body);
assert.equal(unknown.progress, null, 'unknown timing must retain an obvious active line');
context.window.getComputedStyle = element => ({ getPropertyValue: () => element.computed || '' });
const simple = word('Hello');
simple.computed = '25%';
spicyNodes[1].querySelectorAll = () => [simple];
heartbeat();
const simpleMode = JSON.parse(sent[sent.length - 1].options.body);
assert.equal(simpleMode.progress, 2.5);
const dots = { textContent: '•••', classList: { contains: name => ['Active', 'musical-line'].includes(name) }, querySelectorAll: () => [] };
spicyNodes[1] = dots;
heartbeat();
const intro = JSON.parse(sent[sent.length - 1].options.body);
assert.equal(intro.lines[intro.active], '•••', 'instrumental interludes remain active in the sidebar');
