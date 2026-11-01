const title = document.getElementById('title');
const artist = document.getElementById('artist');
const panel = document.getElementById('lyrics');
const status = document.getElementById('status');
let trackKey = '';
let lines = [];
let active = -1;
let generation = 0;
let polling = false;

function message(text) {
  panel.replaceChildren();
  const p = document.createElement('p');
  p.className = 'hint';
  p.textContent = text;
  panel.append(p);
}

async function loadLyrics() {
  lines = []; active = -1;
  message('Open the Lyrics tab in WDock to view Spicy Lyrics.');
  status.textContent = 'Connected to WDock';
}

function updateHighlight(positionMs) {
  if (!lines.length || lines[0].at === null) return;
  let next = -1;
  for (let i = 0; i < lines.length && lines[i].at <= positionMs; i++) next = i;
  if (next === active) return;
  if (active >= 0) panel.children[active]?.classList.remove('active');
  active = next;
  if (active >= 0) {
    const element = panel.children[active];
    element.classList.add('active');
    element.scrollIntoView({ block: 'center', behavior: 'smooth' });
  }
}

async function tick() {
  if (polling) return;
  polling = true;
  try {
    const response = await fetch('http://127.0.0.1:43179/now', { cache: 'no-store', signal: AbortSignal.timeout(2000) });
    if (!response.ok) throw new Error('WDock unavailable');
    const track = await response.json();
    if (!track || !track.title || !track.artist) {
      if (trackKey) { trackKey = ''; generation++; lines = []; }
      title.textContent = 'Nothing playing'; artist.textContent = 'Play a song to see its lyrics';
      message('Waiting for a song with a title and artist.');
      status.textContent = 'Connected to WDock';
      return;
    }
    title.textContent = track.title; artist.textContent = track.artist;
    const key = JSON.stringify([track.title, track.artist, track.album]);
    if (key !== trackKey) { trackKey = key; loadLyrics(track, key); }
    if (track.lyrics) {
      const key = JSON.stringify(track.lyrics);
      if (key !== window.lastLyrics) {
        window.lastLyrics = key;
        panel.replaceChildren(...track.lyrics.lines.map((line, i) => {
          const p = document.createElement('p'); p.textContent = line;
          if (i === track.lyrics.active) p.classList.add('active');
          return p;
        }));
        status.textContent = 'Spicy Lyrics';
      }
    } else if (window.lastLyrics !== '') {
      window.lastLyrics = ''; message('Waiting for Spicy Lyrics.');
    }
  } catch (_) {
    trackKey = ''; generation++; lines = []; active = -1;
    title.textContent = 'WDock not connected'; artist.textContent = 'Start WDock and enable Dynamic Island';
    message('The local WDock connection is unavailable.');
    status.textContent = 'Waiting for WDock';
  } finally { polling = false; }
}

tick();
setInterval(tick, 1000);
