// WDock companion for Spicy Lyrics and Lyrics Plus in Spicetify.
// Sends cached timed lyrics and, when needed, the currently rendered lines.
(() => {
    const endpoint = 'http://127.0.0.1:43179/spotify-lyrics';
    let last = '';
    let lastSent = 0;
    let queued = false;
    let page = null;
    let pageSource = '';
    let palette = [];
    let paletteAt = 0;
    let paletteTrack = '';
    let coverUrl = '';
    let coverImage = null;
    let timedTrack = '';
    let timedHasColors = false;
    let timedBusy = false;
    let timedAttempt = 0;
    let syncCheckedAt = 0;
    let bridgeUnavailableUntil = 0;
    let timedConfirmed = false;

    function setTimedConfirmed(confirmed) {
        if (timedConfirmed === confirmed) return;
        timedConfirmed = confirmed;
        if (confirmed) {
            observer.disconnect();
            discovery.disconnect();
        } else {
            page = null;
            discovery.observe(document.documentElement, { childList: true, subtree: true });
        }
    }

    async function sendTimed() {
        const item = window.Spicetify?.Player?.data?.item;
        const uri = String(item?.uri || '');
        const match = /^spotify:track:([a-zA-Z0-9]+)$/.exec(uri);
        // A local file, podcast or stopped player must restore DOM observers;
        // those items do not have a cacheable spotify:track URI.
        if (timedConfirmed && timedTrack !== uri) setTimedConfirmed(false);
        if (!match || !item?.metadata?.title || !item?.metadata?.artist_name || typeof caches === 'undefined') return;
        if (timedBusy || Date.now() < bridgeUnavailableUntil || Date.now() - timedAttempt < 2000) return;
        if (timedTrack === uri && Date.now() - syncCheckedAt < 5000) return;
        timedBusy = true; timedAttempt = Date.now();
        try {
            if (timedTrack === uri) {
                syncCheckedAt = Date.now();
                const response = await fetch('http://127.0.0.1:43179/spotify-sync', { cache: 'no-store' });
                if (response.ok && (await response.json()).ready && (timedHasColors || !backgroundColors(uri, item.metadata.image_url).length)) {
                    setTimedConfirmed(true);
                    return;
                }
                setTimedConfirmed(false);
                timedTrack = '';
            }
            // Avoid repeatedly decoding the full lyric cache while WDock is
            // closed. A no-cors probe only checks reachability, not contents.
            try {
                await fetch('http://127.0.0.1:43179/now', { mode: 'no-cors', cache: 'no-store' });
                bridgeUnavailableUntil = 0;
            } catch (_) {
                bridgeUnavailableUntil = Date.now() + 10000;
                return;
            }
            if (!(await caches.keys()).includes('SpicyLyrics_LyricsStore_g1')) return;
            const cache = await caches.open('SpicyLyrics_LyricsStore_g1');
            const response = await cache.match('/' + match[1]);
            if (!response) return;
            const entry = await response.json();
            if (entry.CacheVersion !== 6 || entry.ExpiresAt < Date.now()) return;
            const content = entry.Content;
            if (!Array.isArray(content?.Content)) return;
            const lines = content.Content.map(item => {
                const lead = item.Lead || item;
                let words = Array.isArray(lead.Syllables) ? lead.Syllables.map(word => ({
                    start: Math.round(word.StartTime * 1000), end: Math.round(word.EndTime * 1000),
                    text: String(word.Text || ''), join: !!word.IsPartOfWord
                })).filter(word => word.text.trim()) : [];
                const text = words.length ? words.map((word, i) => word.text + (i + 1 < words.length && !word.join ? ' ' : '')).join('').trim() : String(lead.Text || '').trim();
                if (words.some(word => !Number.isFinite(word.start) || !Number.isFinite(word.end) || word.start < 0 || word.end < word.start)) words = [];
                const start = Math.round(lead.StartTime * 1000);
                const rawEnd = Math.round(lead.EndTime * 1000);
                return { start, end: Number.isFinite(rawEnd) && rawEnd >= start ? rawEnd : start, text, words };
            }).filter(line => line.text && Number.isFinite(line.start) && Number.isFinite(line.end) && line.start >= 0 && line.end >= line.start);
            if (!lines.length || window.Spicetify?.Player?.data?.item?.uri !== uri) return;
            lines.sort((a, b) => a.start - b.start);
            const colors = backgroundColors(uri, item.metadata.image_url);
            const body = JSON.stringify({ title: String(item.metadata.title), artist: String(item.metadata.artist_name), lines, colors });
            if (body.length > 200000) return;
            await fetch('http://127.0.0.1:43179/spotify-timed', { method: 'POST', mode: 'no-cors', headers: { 'Content-Type': 'text/plain' }, body });
            timedTrack = uri;
            timedHasColors = colors.length > 0;
            syncCheckedAt = 0;
        } catch (_) {
            // A failed status request must not strand a previously sent track
            // after WDock restarts or the loopback connection drops.
            timedTrack = '';
            setTimedConfirmed(false);
        }
        finally { timedBusy = false; }
    }

    function albumCover(reference) {
        const value = String(reference || '');
        const match = /^(?:spotify:image:|https:\/\/i\.scdn\.co\/image\/)([a-zA-Z0-9]+)$/.exec(value);
        const url = match ? 'https://i.scdn.co/image/' + match[1] : '';
        if (url !== coverUrl) {
            coverUrl = url;
            coverImage = null;
            if (url) {
                coverImage = document.createElement('img');
                coverImage.crossOrigin = 'anonymous';
                coverImage.src = url;
            }
        }
        return coverImage?.complete && coverImage.naturalWidth ? coverImage : null;
    }

    function backgroundColors(track, image) {
        const now = Date.now();
        if (track !== paletteTrack) { palette = []; paletteAt = 0; paletteTrack = track; }
        if (palette.length) return palette;
        if (now - paletteAt < 250) return palette;
        paletteAt = now;
        // Sample the static cover once, rather than pulling animated WebGL
        // frames from Spotify for the lifetime of the song.
        const source = image ? albumCover(image)
            : document.querySelector('#SpicyLyricsPage .ContentBox canvas.spicy-dynamic-bg')
                || document.querySelector('#SpicyLyricsPage .ContentBox canvas');
        if (!source) return palette;
        try {
            const canvas = document.createElement('canvas');
            canvas.width = 12; canvas.height = 8;
            const context = canvas.getContext('2d', { willReadFrequently: true });
            context.drawImage(source, 0, 0, canvas.width, canvas.height);
            const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
            const colors = [];
            let brightness = 0;
            for (let i = 0; i < pixels.length; i += 4) {
                brightness += pixels[i] + pixels[i + 1] + pixels[i + 2];
                colors.push('#' + [pixels[i], pixels[i + 1], pixels[i + 2]].map(n => n.toString(16).padStart(2, '0')).join(''));
            }
            // WebGL canvases can return a cleared buffer; keep the last valid palette.
            if (brightness > colors.length * 18) palette = colors;
        } catch (_) { /* Canvas can be protected or unavailable; WDock uses album art. */ }
        return palette;
    }

    function wordFraction(part) {
        if (part.classList.contains('Sung')) return 1;
        if (part.classList.contains('letterGroup')) {
            const letters = [...part.querySelectorAll('.letter')];
            if (letters.length) {
                const values = letters.map(wordFraction);
                return values.every(value => value !== null) ? values.reduce((sum, value) => sum + value, 0) / values.length : null;
            }
        }
        // Spicy Lyrics updates CSS gradient variables on words and letters;
        // Active/Sung classes are applied to lines, not necessarily words.
        const normal = part.style.getPropertyValue('--gradient-position');
        let simple = part.style.getPropertyValue('--SLM_GradientPosition');
        if (!normal && !simple && typeof window.getComputedStyle === 'function') {
            simple = window.getComputedStyle(part).getPropertyValue('--SLM_GradientPosition');
        }
        const percent = parseFloat(normal || simple);
        if (Number.isFinite(percent)) {
            const start = normal ? -20 : -50;
            return Math.max(0, Math.min(1, (percent - start) / (100 - start)));
        }
        if (part.classList.contains('Active')) return 0.5;
        if (part.classList.contains('NotSung')) return 0;
        return null;
    }

    function spicyLine(node, active) {
        // Spicy Lyrics positions word elements with CSS margins instead of
        // literal spaces. PartOfWord fragments must stay joined.
        const parts = [...node.querySelectorAll('.word, .letterGroup')]
            .filter(part => !part.classList.contains('dot') && !part.closest('.bg-line') && part.textContent.trim());
        if (!parts.length) return { text: node.textContent.trim().replace(/\s+/g, ' '), progress: null };
        let text = '';
        let progress = 0;
        let finished = false;
        let known = true;
        for (let i = 0; i < parts.length; i++) {
            const part = parts[i];
            const word = part.textContent.trim();
            const wordLength = [...word].length;
            text += word;
            if (active && !finished) {
                const amount = wordFraction(part);
                if (amount === null) { known = false; finished = true; }
                else {
                    progress += wordLength * amount;
                    if (amount < 1) finished = true;
                }
            }
            if (i + 1 < parts.length && !part.classList.contains('PartOfWord')) {
                text += ' ';
                if (active && !finished) progress += 1;
            }
        }
        return { text: text.trim(), progress: active && known ? progress : null };
    }

    function snapshot() {
        const metadata = window.Spicetify?.Player?.data?.item?.metadata;
        if (!metadata?.title || !metadata?.artist_name) return null;
        const root = page;
        if (!root || !root.getClientRects().length) return null;
        const spicy = pageSource === 'Spicy Lyrics';
        const nodes = [...root.querySelectorAll(spicy ? '.line' : '.lyrics-lyricsContainer-LyricsLine')].filter(node =>
            (!node.classList.contains('musical-line') || node.classList.contains('Active')) &&
            !node.classList.contains('lyrics-lyricsContainer-LyricsLine-hidden') &&
            !node.classList.contains('lyrics-lyricsContainer-LyricsLine-paddingLine') &&
            (spicy ? node.textContent : node.querySelector('p')?.textContent)?.trim());
        const active = nodes.findIndex(node => node.classList.contains(spicy ? 'Active' : 'lyrics-lyricsContainer-LyricsLine-active'));
        if (active < 0) return null;
        const from = Math.max(0, active - 4);
        const selected = nodes.slice(from, from + 9);
        const rendered = selected.map((node, i) => spicy ? spicyLine(node, i === active - from) : { text: node.querySelector('p').textContent.trim(), progress: null });
        const activeProgress = rendered[active - from]?.progress;
        return {
            title: String(metadata.title).slice(0, 512),
            artist: String(metadata.artist_name).slice(0, 512),
            source: pageSource,
            active: active - from,
            lines: rendered.map(line => line.text.slice(0, 256)),
            progress: activeProgress === null ? null : Math.min(256, Math.round(activeProgress * 10) / 10),
            colors: spicy ? backgroundColors(String(window.Spicetify.Player.data.item.uri || metadata.title), metadata.image_url) : []
        };
    }

    function send() {
        queued = false;
        // Chromium can suspend the lyric renderer while Spotify is hidden.
        // Re-sending its last DOM state would keep stale lyrics alive in WDock.
        if (document.hidden || timedConfirmed || Date.now() < bridgeUnavailableUntil) { last = ''; return; }
        const data = snapshot();
        if (!data) { last = ''; return; }
        const body = JSON.stringify(data);
        const now = Date.now();
        if (body === last && now - lastSent < 2000) return;
        last = body;
        lastSent = now;
        // no-cors keeps this a simple request; WDock validates the Spotify
        // origin, active track, fields, and request size before accepting it.
        fetch(endpoint, { method: 'POST', mode: 'no-cors', headers: { 'Content-Type': 'text/plain' }, body }).catch(() => {});
    }

    const observer = new MutationObserver(() => {
        if (document.hidden || timedConfirmed || Date.now() < bridgeUnavailableUntil) return;
        if (!queued) { queued = true; setTimeout(send, 150); }
    });
    function findPage() {
        if (document.hidden || timedConfirmed || Date.now() < bridgeUnavailableUntil) return;
        // The same renderer can be mounted inside the right-hand Now Playing
        // card, or in the full page. Do not select an invisible stale mount.
        const spicy = [...document.querySelectorAll('#SpicyLyricsPage .LyricsContainer .LyricsContent')]
            .find(node => node.getClientRects().length && node.querySelector('.line.Active'))
            || [...document.querySelectorAll('#SpicyLyricsPage .LyricsContainer .LyricsContent')]
                .find(node => node.getClientRects().length);
        const plus = document.querySelector('.lyrics-lyricsContainer-SyncedLyricsPage');
        const next = spicy?.getClientRects().length ? spicy : plus;
        const source = next === spicy ? 'Spicy Lyrics' : 'Lyrics Plus';
        if (next === page) return;
        observer.disconnect();
        page = next;
        pageSource = source;
        last = '';
        if (page) observer.observe(page, { childList: true, subtree: true, attributes: true, attributeFilter: ['class'] });
        send();
    }
    const discovery = new MutationObserver(findPage);
    discovery.observe(document.documentElement, { childList: true, subtree: true });
    findPage();
    // Follow word gradients while the page is present; the unchanged-payload
    // guard prevents network work when the lyrics and background are idle.
    setInterval(() => { if (!timedConfirmed && !document.hidden && Date.now() >= bridgeUnavailableUntil) { findPage(); send(); } sendTimed(); }, 250);
})();
