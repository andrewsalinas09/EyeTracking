// Exercise the standalone viewer with synthetic evidence, without a browser or
// user gaze data. Rendering calls must receive finite coordinates in every view.
const fs = require('node:fs');
const vm = require('node:vm');
const assert = require('node:assert/strict');
const path = require('node:path');
const html = fs.readFileSync(path.join(__dirname, '../src/learning_view.html'), 'utf8');
const field = Array.from({length:35}, (_,i)=>({offset:[i%3, -(i%4)], trained:i<8}));
const context = epoch=>({kind:'context',schema:1,epoch,display:{rect:[-3840,-2160,0,0]},field});
const attempt = (accepted,updated,target,epoch=0)=>({kind:'attempt',epoch,timestamp_ms:1700000000000,rect:[-3840,-2160,0,0],base:[-2000,-1000],landing:[-1990,-995],last:[-1980,-980],target,path_px:25,accepted,updated,status:accepted?'Learning local correction map':'Skipped: drag',field});
const records=[context(0),attempt(true,false,[-1980,-980]),attempt(true,true,[-1980,-980]),attempt(false,false,null),{kind:'reset',epoch:1,field:[]},context(1)];
const elements = new Map();
const drawing = new Proxy({}, {get:(_,key)=> (...args)=>{for(const v of args)if(typeof v==='number')assert.ok(Number.isFinite(v), `${key}: ${v}`);},set:()=>true});
function element(id='') {
 return {id,value:id==='filter'?'all':id==='scale'?'1':'',checked:true,children:[],textContent:'',width:1400,height:788,
 replaceChildren(...items){this.children=items;if(this.id==='epoch')this.value=items[0]?.value||'';},append(item){this.children.push(item);},getContext(){return drawing;},getBoundingClientRect(){return {left:0,top:0,width:this.width,height:this.height};},click(){this.onclick?.();}};
}
const document={getElementById(id){if(!elements.has(id))elements.set(id,element(id));return elements.get(id);},createElement(){return element();}};
const sandbox=vm.createContext({document,Option:function(text,value){this.text=text;this.value=value;}});
const script=html.match(/<script>([\s\S]*)<\/script>/)[1].replace('/*RECORDS*/[]',JSON.stringify(records));
vm.runInContext(script,sandbox);
const get=id=>elements.get(id);
assert.match(get('stats').textContent,/0 attempts/); // Most recent reset starts empty.
get('epoch').value='0';get('epoch').onchange();
assert.match(get('stats').textContent,/3 attempts · 2 eligible clicks · 1 map updates · 8\/35/);
assert.equal(get('body').children.length,3);
get('body').children[1].onclick();
assert.match(get('detail').textContent,/pointer landing: \(-1990.0, -995.0\)/);
for(const [filter,count] of [['accepted',2],['updated',1],['rejected',1],['all',3]]){
 get('filter').value=filter;get('filter').onchange();assert.equal(get('body').children.length,count);
}
for(const scale of ['1','3','6']){get('scale').value=scale;get('scale').onchange();}
get('field').checked=false;get('field').onchange();
(async()=>{
 const dense=[{...context(2),grid:[65,37],field:Array.from({length:2405},()=>({offset:[250,-100],trained:true}))}, {...attempt(true,false,[-1750,-1100],2),field:null}];
 get('field').checked=true;
 get('file').files=[{name:'dense.jsonl',async text(){return dense.map(r=>JSON.stringify(r)).join('\n');}}];
 await get('file').onchange();assert.match(get('stats').textContent,/2405\/2405 grid points trained/);
 get('file').files=[{name:'saved.jsonl',async text(){return records.map(r=>JSON.stringify(r)).join('\n');}}];
 await get('file').onchange();assert.equal(get('detail').textContent,'Loaded saved.jsonl');
 get('file').files=[{name:'bad.jsonl',async text(){return '{broken';}}];
 await get('file').onchange();assert.match(get('message').textContent,/Could not load file/);
 console.log('Viewer checks passed: empty/reset states, negative origins, filters, point details, scales, file loading.');
})().catch(e=>{console.error(e);process.exitCode=1;});
