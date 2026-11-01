const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
let tick, now = 100000, ready = false; const sent=[];
const clock = { now: () => now };
const context={window:{Spicetify:{Player:{data:{item:{uri:'spotify:track:ABC123',metadata:{title:'Song',artist_name:'Artist',image_url:'unavailable-image'}}}}}},document:{hidden:true,documentElement:{},querySelector:()=>null,querySelectorAll:()=>[]},MutationObserver:class{observe(){} disconnect(){}},setInterval:fn=>tick=fn,setTimeout:()=>{},Date:clock,JSON,fetch:async(url,options)=>{ if(url.endsWith('/spotify-sync')) return {ok:true,json:async()=>({ready})}; sent.push({url,body:JSON.parse(options.body)}); return {}; },caches:{keys:async()=>['SpicyLyrics_LyricsStore_g1'],open:async()=>({match:async()=>({json:async()=>({CacheVersion:6,ExpiresAt:now+10000,Content:{Content:[{Lead:{StartTime:1,EndTime:2,Syllables:[{StartTime:1,EndTime:1.5,Text:'Hello'},{StartTime:1.5,EndTime:2,Text:'world'}]}},{Lead:{StartTime:3,Syllables:[{Text:'Untimed'}]}}]}})})})}};
vm.runInNewContext(fs.readFileSync('extensions/spicetify/wdock-lyrics-plus.js','utf8'),context);
(async()=>{
  const flush = () => new Promise(resolve=>setImmediate(resolve));
  tick(); await flush();
  assert.equal(sent.length,1); assert.equal(sent[0].url,'http://127.0.0.1:43179/spotify-timed');
  assert.equal(sent[0].body.lines[0].text,'Hello world'); assert.equal(sent[0].body.lines[0].words[1].start,1500); assert.equal(sent[0].body.lines[1].text,'Untimed'); assert.equal(sent[0].body.lines[1].words.length,0); assert.equal(sent[0].body.lines[1].end,3000);
  now += 6000; tick(); await flush(); assert.equal(sent.length,2,'retry if WDock rejected or restarted');
  ready = true; now += 6000; tick(); await flush(); assert.equal(sent.length,2,'do not resend when confirmed');
  ready = false; now += 6000; tick(); await flush(); assert.equal(sent.length,3,'resend after WDock loses cached lyrics');
  console.log('timed cache and retry passed');
})().catch(error=>{console.error(error);process.exitCode=1;});
