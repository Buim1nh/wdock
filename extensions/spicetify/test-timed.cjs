const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
let tick, now = 100000, ready = false, available = true, cacheReads = 0, disconnected = 0, observed = 0; const sent=[];
const clock = { now: () => now };
const context={window:{Spicetify:{Player:{data:{item:{uri:'spotify:track:ABC123',metadata:{title:'Song',artist_name:'Artist',image_url:'unavailable-image'}}}}}},document:{hidden:true,documentElement:{},querySelector:()=>null,querySelectorAll:()=>[]},MutationObserver:class{observe(){observed++} disconnect(){disconnected++}},setInterval:fn=>tick=fn,setTimeout:()=>{},Date:clock,JSON,fetch:async(url,options)=>{ if(!available) throw new Error('offline'); if(url.endsWith('/now')) return {}; if(url.endsWith('/spotify-sync')) return {ok:true,json:async()=>({ready})}; sent.push({url,body:JSON.parse(options.body)}); return {}; },caches:{keys:async()=>{cacheReads++;return ['SpicyLyrics_LyricsStore_g1']},open:async()=>({match:async()=>({json:async()=>({CacheVersion:6,ExpiresAt:now+10000,Content:{Content:[{Lead:{StartTime:1,EndTime:2,Syllables:[{StartTime:1,EndTime:1.5,Text:'Hello'},{StartTime:1.5,EndTime:2,Text:'world'}]}},{Lead:{StartTime:3,Syllables:[{Text:'Untimed'}]}}]}})})})}};
vm.runInNewContext(fs.readFileSync('extensions/spicetify/wdock-lyrics-plus.js','utf8'),context);
(async()=>{
  const flush = () => new Promise(resolve=>setImmediate(resolve));
  tick(); await flush();
  assert.equal(sent.length,1); assert.equal(sent[0].url,'http://127.0.0.1:43179/spotify-timed');
  assert.equal(sent[0].body.lines[0].text,'Hello world'); assert.equal(sent[0].body.lines[0].words[1].start,1500); assert.equal(sent[0].body.lines[1].text,'Untimed'); assert.equal(sent[0].body.lines[1].words.length,0); assert.equal(sent[0].body.lines[1].end,3000);
  now += 6000; tick(); await flush(); assert.equal(sent.length,2,'retry if WDock rejected or restarted');
  ready = true; now += 6000; tick(); await flush(); assert.equal(sent.length,2,'do not resend when confirmed'); assert.ok(disconnected >= 2, 'stop lyric and document observers after confirmation');
  const beforeObserve = observed; context.window.Spicetify.Player.data.item.uri = 'spotify:local:artist:album:track'; now += 250; tick(); await flush(); assert.ok(observed > beforeObserve, 'restore observers when switching to a local track');
  context.window.Spicetify.Player.data.item.uri = 'spotify:track:ABC123';
  ready = false; now += 6000; tick(); await flush(); assert.equal(sent.length,3,'resend after WDock loses cached lyrics');
  available = false; now += 6000; tick(); await flush();
  now += 2000; tick(); await flush(); const before = cacheReads;
  for (let i=0; i<10; i++) { now += 500; tick(); await flush(); }
  assert.equal(cacheReads,before,'do not read cached lyrics repeatedly when WDock is unavailable');
  available = true; now += 10000; tick(); await flush();
  assert.equal(sent.length,4,'automatically reconnect when WDock returns');
  console.log('timed cache and retry passed');
})().catch(error=>{console.error(error);process.exitCode=1;});
