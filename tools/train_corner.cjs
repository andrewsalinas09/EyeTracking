// Offline frozen fit: only earlier compatible sessions, never the test session.
// Node 24+: node tools/train_corner.cjs DB TEST_SESSION OUTPUT
// OUTPUT is create-only. Keep personal models in ignored recordings/.
const fs = require('node:fs');
const { DatabaseSync } = require('node:sqlite');
const [path, testSession, output] = process.argv.slice(2);
if (!path || !testSession || !output) throw Error('Usage: DB TEST_SESSION OUTPUT');
const db = new DatabaseSync(path, { readOnly: true });
db.exec('BEGIN');
const sessions = db.prepare('SELECT id,started_ms FROM capture_sessions ORDER BY started_ms').all();
const test = sessions.find(s => s.id === testSession);
if (!test) throw Error('Unknown test session');
const events = id => db.prepare("SELECT data_json FROM capture_index WHERE session=? AND seq>0 AND kind IN ('context','attempt') ORDER BY seq").all(id).map(r => JSON.parse(r.data_json));
const contextOf = d => ({ display: d.display, mapping: d.base_model ? { coefficients: d.base_model.coefficients, local: d.base_model.local } : null });
const contexts = events(test.id).filter(e => e.kind === 'context');
if (!contexts.length) throw Error('Test session has no calibration context');
const context = contextOf(contexts.at(-1).data);
const key = JSON.stringify(context);
const dist = (a,b) => Math.hypot(a[0]-b[0],a[1]-b[1]);
const finitePair = p => Array.isArray(p) && p.length === 2 && p.every(Number.isFinite);
const rows = [];
for (const s of sessions.filter(s => s.started_ms < test.started_ms)) {
  let current;
  for (const e of events(s.id)) {
    const d = e.data;
    if (e.kind === 'context') current = JSON.stringify(contextOf(d));
    if (e.kind !== 'attempt' || current !== key || !finitePair(d.target) || !finitePair(d.base) || !finitePair(d.landing)) continue;
    const hold = (d.end_tick_ms-d.button_down_tick_ms) >>> 0;
    if (d.button_up_observed && d.evidence?.head && d.evidence?.raw_gaze &&
        d.click_delay_ms >= 80 && d.click_delay_ms <= 3000 && hold <= 500 &&
        d.drag_path_px <= 4 && d.path_px <= 600 && dist(d.target,d.landing) <= 300 &&
        d.target.every((v,i) => v >= d.rect[i] && v < d.rect[i+2])) rows.push(d);
  }
}
db.exec('COMMIT'); db.close();
if (rows.length < 30) throw Error('Fewer than 30 compatible earlier-session labels');
function features(d) {
  const [x,y] = d.base.map((v,i) => (v-d.rect[i])/(d.rect[i+2]-d.rect[i])-.5);
  const f = [1,x,y];
  for(let j=0;j<3;j++) for(let i=0;i<5;i++) f.push(Math.exp(-((x-(i/4-.5))**2+(y-(j/2-.5))**2)/(2*.25**2)));
  return f;
}
function solve(a,b) {
  const n=a.length,m=a.map((r,i)=>[...r,...b[i]]);
  for(let i=0;i<n;i++) {
    let k=i; for(let j=i+1;j<n;j++) if(Math.abs(m[j][i])>Math.abs(m[k][i])) k=j;
    [m[k],m[i]]=[m[i],m[k]];
    const p=m[i][i]; if(Math.abs(p)<1e-10) throw Error('Singular fit');
    for(let j=i;j<n+2;j++) m[i][j]/=p;
    for(let k=0;k<n;k++) if(k!==i) { const p=m[k][i]; for(let j=i;j<n+2;j++) m[k][j]-=p*m[i][j]; }
  }
  return m.map(r=>r.slice(n));
}
const x=rows.map(features),y=rows.map(d=>d.target.map((v,i)=>v-d.base[i]));
let weights=x.map(()=>1),coefficients;
for(let iter=0;iter<4;iter++) {
  const a=Array.from({length:18},(_,i)=>Array.from({length:18},(_,j)=>i===j?(i===0?.01:1):0));
  const b=Array.from({length:18},()=>[0,0]);
  for(let k=0;k<x.length;k++) for(let i=0;i<18;i++) {
    for(let j=0;j<18;j++) a[i][j]+=weights[k]*x[k][i]*x[k][j];
    for(let j=0;j<2;j++) b[i][j]+=weights[k]*x[k][i]*y[k][j];
  }
  coefficients=solve(a,b);
  weights=x.map((f,k)=>Math.min(1,40/Math.max(1,dist([0,1].map(axis=>f.reduce((s,v,i)=>s+v*coefficients[i][axis],0)),y[k]))));
}
if (!coefficients.flat().every(v=>Number.isFinite(v)&&Math.abs(v)<1e6)) throw Error('Invalid fit');
fs.writeFileSync(output, JSON.stringify({version:1,context,coefficients,training_samples:rows.length,trained_before_ms:test.started_ms},null,2), {flag:'wx'});
console.log(JSON.stringify({output,training_samples:rows.length,held_out_session:test.id}));
