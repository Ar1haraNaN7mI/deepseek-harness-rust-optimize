/* Original cinematic host. The canvas itself is the interaction surface. */
(() => {
'use strict';
const $ = id => document.getElementById(id);
if (!window.DSHBoot || !window.DSHVisuals || !window.DSHLocal || !window.DSHIdentity || !window.DSHText) { $('canvasPane').insertAdjacentHTML('beforeend','<p class="error">请保留同目录的完整预览资源。</p>'); $('play').disabled=true; return; }
const sequence = new DSHBoot.Sequence();
let {total,durations} = DSHBoot;
const scoreDurations=[2.8,2.8,3.4,3.2,2.8,3];
const ui = Object.fromEntries(['sound','theme','play','skip','settings','closeSettings','controlPanel','replay','interactive','speed','seek','currentTime','totalTime','phaseTitle','phaseEnglish','phaseNote','status','identityCard','identityName','identityId','cardName','cardId','identityLine','subtitleZh','subtitleEn','saveIdentity','profileStatus','connectionStatus','connectLocal','inventoryPanel','inventoryMessage','skillsCount','pluginsCount','skillsState','pluginsState','skillsList','pluginsList','inventoryIssues','cardMode','cardNameVisual','cardIdVisual','cardModeVisual','accessForm','accessEntry','accessPassword','accessSubmit','accessState','accessMessage','accessRetry'].map(id => [id,$(id)]));
const identityView=new DSHIdentity.View(['cardName','cardId','cardMode'].map(id=>({value:ui[id],visual:ui[id+'Visual']})));
const textMotion=new DSHText.Stage(document),inventoryRows={skills:new Map(),plugins:new Map()};
const setText=(node,value,options={})=>textMotion.set(node,value,options);
const identityRows=[...document.querySelectorAll('.identity-row')],inventoryColumns=[...document.querySelectorAll('.inventory-column')];
const canvas=$('scene'), pane=$('canvasPane'), stage=$('experience'), ctx=canvas.getContext('2d',{alpha:false});
const transitionCanvas=$('sceneTransition'),transitionCtx=transitionCanvas.getContext('2d');
const chapters=[...document.querySelectorAll('.chapter')];
const motionMedia=matchMedia('(prefers-reduced-motion: reduce)');
const motionOverride=new URLSearchParams(location.search).get('motion')==='reduce';
const reducedMotion={get matches(){return motionOverride||motionMedia.matches;},addEventListener:(...args)=>motionMedia.addEventListener(...args)};
const phases=[
 ['深潜协议','DEEP DIVE / HARNESS','一道信号，开启新的深度。','SYSTEM / STANDBY'],
 ['建立本地连接','LOCAL CONNECTION','连接当前 DSH 工作区。','ACCESS / REQUEST'],
 ['访问身份确认','IDENTITY VERIFICATION','确认身份后，读取本地 skills 与 plugins。','IDENTITY / VERIFY'],
 ['展开能力档案','READING LOCAL CAPABILITIES','直接读取本地 skills 与 plugins。','REQUEST / RECEIVED'],
 ['能力档案就绪','LOCAL LOAD COMPLETE','真实加载结果已返回。','ACCESS / VERIFIED'],
 ['欢迎进入 DSH','WELCOME TO DSH','新的探索，由此开始。','DSH / ONLINE']
];
let light=new URLSearchParams(location.search).get('theme')!=='dark',muted=false,speed=1,width=1,height=1,dpr=1;
let visualStyle=new URLSearchParams(location.search).get('style')==='laterano'?'laterano':'rhine';
// Authentication hydrates the saved profile more than once. Keep an explicit
// selection separate until it has been persisted, so those reads cannot undo it.
let pendingVisualStyle=null,styleRevision=0,styleSaving=null;
let layoutDirty=true,sceneLayout={identity:null,inventory:null};
const paintedStyles=new WeakMap();
function motionStyle(node,key,value){
 let styles=paintedStyles.get(node);if(!styles){styles=new Map();paintedStyles.set(node,styles);}
 if(styles.get(key)===value)return;
 styles.set(key,value);
 if(key.startsWith('--'))node.style.setProperty(key,value);else node.style[key]=value;
}
let soundTouched=false,soundInitialized=false;
let audio=null,master=null,audioSources=[],audioUnavailable=false;
let phaseAge=0,visiblePhase=-1,starting=false;
let scenePhase=-1,paintedPhase=-1,paintedProgress=0,paintedAmbientTime=0,sceneAge=0,bookAge=0,bookSource=null,transitionAge=1;
const smoothStep=value=>{const t=Math.max(0,Math.min(1,value));return t*t*(3-2*t);};
const entryAnimations=new Map();
let raf=0,lastFrame=0,generation=0,ambientTime=0,impulse=0,drag=0,manualPause=false,signature='',pointer={x:.5,y:.5},pointerDown=null,embeddedEnded=false;
const announce=message=>{ui.status.textContent=message;};
let profileDirty=false,draftRevision=0,localRevision=0,inventorySeen=null,profileSeen=null;
const local=new DSHLocal.Client({onChange:()=>{localRevision++;renderLocal();refresh();}});
const accessBlocked=()=>local.locked||(local.connection!=='local'&&!(local.previewOnly&&!window.DSHEmbed?.embedded));
const voice=()=>window.DSHVoice;
let narratedPhase=-1;
const cancelVoice=()=>{narratedPhase=-1;voice()?.cancel?.();};
const narrate=()=>{if(narratedPhase===sequence.phase)return;narratedPhase=sequence.phase;voice()?.phase?.(sequence.phase,{username:ui.identityName.value.trim()||local.profile.username,inventory:local.inventory,connection:local.connection,inventoryMode:local.profile.inventory_mode});};
const loadFinished=()=>['complete','error','unavailable'].includes(local.inventory.status);
function barriers(){for(let i=0;i<6;i++){if((i===2&&accessBlocked())||(i===3&&!loadFinished())||(i===sequence.phase&&voice()?.busy))sequence.hold(i);else sequence.release(i);}}
function beginLocalLoad(){if(!accessBlocked()&&sequence.phase===3&&sequence.playing&&['idle','cancelled'].includes(local.inventory.status))void local.load();}
ui.seek.max=String(total);ui.totalTime.textContent=total.toFixed(2);
  function stopAudio() {
    for (const source of audioSources) { try { source.stop(); } catch (_) { /* Already ended. */ } }
    audioSources = [];
  }
  function keepSource(source, nodes) {
    audioSources.push(source);
    source.onended = () => {
      source.disconnect(); nodes.forEach(node => node.disconnect());
      const index = audioSources.indexOf(source); if (index >= 0) audioSources.splice(index, 1);
    };
  }
  function tone(at, hz, duration, volume, type = 'sine', endHz = hz) {
    const source = audio.createOscillator(), envelope = audio.createGain();
    source.type = type; source.frequency.setValueAtTime(hz, at); source.frequency.exponentialRampToValueAtTime(Math.max(1, endHz), at + duration);
    envelope.gain.setValueAtTime(0, at); envelope.gain.linearRampToValueAtTime(volume, at + Math.min(.025, duration * .2)); envelope.gain.exponentialRampToValueAtTime(.0001, at + duration);
    source.connect(envelope); envelope.connect(master); keepSource(source, [envelope]); source.start(at); source.stop(at + duration + .01);
  }
  function noise(at, duration, volume, hz = 2000) {
    const length = Math.max(1, Math.ceil(audio.sampleRate * duration));
    const buffer = audio.createBuffer(1, length, audio.sampleRate), data = buffer.getChannelData(0); let seed = 7139;
    for (let i = 0; i < length; i++) { seed ^= seed << 13; seed ^= seed >>> 17; seed ^= seed << 5; data[i] = (seed >>> 0) / 2147483648 - 1; }
    const source = audio.createBufferSource(), filter = audio.createBiquadFilter(), envelope = audio.createGain(); source.buffer = buffer;
    filter.type = 'bandpass'; filter.frequency.value = hz; filter.Q.value = .75;
    envelope.gain.setValueAtTime(0, at); envelope.gain.linearRampToValueAtTime(volume, at + Math.min(.015, duration * .2)); envelope.gain.exponentialRampToValueAtTime(.0001, at + duration);
    source.connect(filter); filter.connect(envelope); envelope.connect(master); keepSource(source, [filter, envelope]); source.start(at); source.stop(at + duration);
  }
  async function ensureAudio() {
    if (muted || audioUnavailable) return false;
    try {
      if (!audio || audio.state === 'closed') {
        const Audio = window.AudioContext || window.webkitAudioContext;
        if (!Audio) throw new Error('No Web Audio');
        audio = new Audio(); master = audio.createGain(); master.gain.value = .6; master.connect(audio.destination);
      }
      if (audio.state === 'suspended') {
        let deadline;
        try {
          await Promise.race([audio.resume(), new Promise(resolve => { deadline=setTimeout(resolve,2000); })]);
        } finally { clearTimeout(deadline); }
      }
      return audio.state === 'running';
    } catch (_) {
      audioUnavailable = true; ui.sound.textContent = '音效不可用'; ui.sound.disabled = true; ui.sound.setAttribute('aria-pressed', 'false');
      announce('当前浏览器无法播放合成音效，动画仍可正常体验。'); return false;
    }
  }
function scorePhase(){
 stopAudio();if(!audio||audio.state!=='running'||muted||!sequence.playing)return;
 const start=audio.currentTime+.02,elapsed=sequence.elapsed;
 const ratio=durations[sequence.phase]/scoreDurations[sequence.phase],audioScale=ratio/speed;
 const event=(local,fn)=>{const position=local*ratio;if(position>=elapsed-.025)fn(start+Math.max(0,position-elapsed)/speed);};
 const ping=(local,hz,len=.3,gain=.05,type='sine',end=hz)=>event(local,t=>tone(t,hz,len*audioScale,gain,type,end));
 const air=(local,len,gain,hz)=>event(local,t=>noise(t,len*audioScale,gain,hz));
 if(visualStyle==='laterano'){
  // Original bright, measured chimes over a soft sustained foundation.
  const chords=[[196,294,440],[220,330,495],[261.626,392,587.33],[246.94,369.99,554.37],[293.665,440,659.255],[261.626,392,523.251]];
  const chord=chords[sequence.phase];
  ping(.02,chord[0]/2,1.7,.065,'sine');
  chord.forEach((hz,i)=>{ping(.08+i*.15,hz,1.2,.032);ping(.5+i*.19,hz*2,.65,.022,'triangle');});
  air(.06,.2,.025,2800);ping(1.9,chord[2],.65,.027);air(1.82,.18,.028,1500);
  if(sequence.phase===5)chord.forEach((hz,i)=>ping(1.4+i*.04,hz,1.25,.033));
  return;
 }
 switch(sequence.phase){
 case 0:
  ping(.02,49,1.4,.17,'sine',65);ping(.35,130,1.5,.027,'triangle',780);air(.05,.35,.05,1200);
  ping(2.15,82, .55,.13,'sine',43);air(2.1,.55,.12,2300);ping(2.15,523,.55,.045);break;
 case 1:
  [523,784,1047,659,1175,784,523,1319].forEach((hz,i)=>ping(.04+i*.27,hz,.13,.037,'triangle'));
  air(.04,.5,.05,1700);ping(2.15,196,.6,.075);break;
 case 2:
  [440,554.365,659.255].forEach((hz,i)=>{ping(.03+i*.18,hz,.9,.05);ping(1.15+i*.19,hz*2,.3,.027);});
  ping(2.55,220,.65,.08);ping(2.57,880,.45,.045);air(2.55,.14,.065,1900);break;
 case 3:
  ping(.04,110,2,.055,'sine',660);air(.1,1.35,.06,2400);[523,659,784].forEach((hz,i)=>ping(.3+i*.54,hz,.25,.045,'triangle'));
  ping(2.5,98,.55,.12,'sine',49);air(2.46,.32,.07,1100);break;
 case 4:
  [.06,.65,1.25].forEach((local,i)=>{air(local,.075,.07,1300+i*600);ping(local+.025,[392,523,659][i],.2,.047);});
  ping(2.08,65,.58,.14);ping(2.1,523,.55,.045);ping(2.15,784,.5,.037);break;
 case 5:
  ping(.02,196,.85,.06);ping(.18,392,.75,.04);
   event(1.3,t=>{[261.626,329.628,391.995,587.33].forEach((hz,i)=>tone(t+i*.025*audioScale,hz,1.4*audioScale,.047));tone(t,65.406,1.25*audioScale,.16);noise(t,.22*audioScale,.08,2100);});break;
 }
}
function clickSound(){if(!audio||audio.state!=='running'||muted)return;const t=audio.currentTime+.01;tone(t,660,.065,.023,'sine',330);}
function measureSceneLayout(){
 const origin=pane.getBoundingClientRect();
 const rect=node=>{const r=node.getBoundingClientRect();return{x:r.left-origin.left,y:r.top-origin.top,width:r.width,height:r.height};};
 const heading=rect($('stageHeading'));
 if(visualStyle==='laterano'&&!ui.inventoryPanel.hidden){
  // Content sets the upper edge; CSS retains a readable floor for small windows.
  // Never scale the DOM with a transform: these CSS-pixel bounds also anchor ink.
  const gap=Math.max(12,Math.min(28,Math.min(width,height)*.026));
  const top=Math.max(height*.30,heading.y+heading.height+gap);
  const value=top.toFixed(2)+'px';
  if(ui.inventoryPanel.style.getPropertyValue('--folio-top')!==value)ui.inventoryPanel.style.setProperty('--folio-top',value);
 }
 sceneLayout={
  heading,
  identity:ui.identityCard.hidden?null:{...rect(ui.identityCard),rows:identityRows.map(node=>{const r=rect(node);return{y:r.y,height:r.height};})},
  inventory:ui.inventoryPanel.hidden?null:{...rect(ui.inventoryPanel),columns:inventoryColumns.map(rect)},
  book:null
 };layoutDirty=false;
 if(visualStyle==='laterano'&&!ui.inventoryPanel.hidden){
  const leaf=(page,heading,list)=>{
   const listBox=rect(list),end=listBox.y+listBox.height;
   // Stable row boxes include names and source lines. Empty/loading messages
   // live in their own measured ink span rather than filling the flex column.
   const content=[...list.children].map(rect).filter(box=>box.width>0&&box.height>0);
   const overflowing=list.scrollHeight>list.clientHeight+1;
   const occupiedBottom=overflowing||!content.length?end:Math.max(listBox.y,Math.min(end,Math.max(...content.map(box=>box.y+box.height))));
   return {...rect(page),heading:rect(heading),list:listBox,occupiedBottom,footerY:end};
  };
  const left=leaf($('skillsPage'),$('skillsHeading'),ui.skillsList),right=leaf($('pluginsPage'),$('pluginsHeading'),ui.pluginsList);
  sceneLayout.book={...rect($('inventoryBook')),gutterX:(left.x+left.width+right.x)/2,spread:true,left,right};
 }
}
function clearTransition(){transitionAge=1;motionStyle(transitionCanvas,'opacity','0');}
function resetSceneMotion(){clearTransition();scenePhase=-1;paintedPhase=-1;sceneAge=0;bookAge=0;bookSource=null;}
function beginScene(){
 if(scenePhase===sequence.phase)return;
 const continuingBook=visualStyle==='laterano'&&scenePhase===3&&sequence.phase===4;
 const bindingBook=visualStyle==='laterano'&&scenePhase===2&&sequence.phase===3;
 if(bindingBook&&paintedPhase===2){
  // Retain vector geometry, not a raster screenshot: the measured portal is
  // recomposed into the binding and cover by the incoming folio renderer.
  // Use the last painted clock, not the clock already advanced by this frame.
  bookSource={phase:2,layout:sceneLayout,viewport:{width,height},progress:paintedProgress,ambientTime:paintedAmbientTime};
 }else if(!continuingBook)bookSource=null;
 if(visualStyle==='laterano'&&paintedPhase===scenePhase&&scenePhase>=0&&!continuingBook&&!bindingBook&&!manualPause&&!reducedMotion.matches&&transitionCtx){
  transitionCanvas.width=canvas.width;transitionCanvas.height=canvas.height;
  transitionCtx.setTransform(1,0,0,1,0,0);transitionCtx.clearRect(0,0,canvas.width,canvas.height);transitionCtx.drawImage(canvas,0,0);
  transitionAge=0;motionStyle(transitionCanvas,'opacity','1');
 }else clearTransition();
 sceneAge=manualPause?(visualStyle==='laterano'?sequence.progress*1.65:sequence.elapsed):0;
 if(!continuingBook)bookAge=manualPause?(sequence.phase===4?durations[3]:0)+sequence.elapsed:0;
 scenePhase=sequence.phase;
 if(visualStyle==='laterano'&&!continuingBook&&!manualPause){
  enter($('stageCopy'),[{opacity:0},{opacity:1}],{duration:550});
 }
}
function drawScene(){
 if(!ctx||scenePhase!==sequence.phase)return;
 if(layoutDirty)measureSceneLayout();
 const laterano=visualStyle==='laterano',inBook=sequence.phase===3||sequence.phase===4;
 const visualProgress=reducedMotion.matches?1:Math.max(sequence.progress,Math.min(1,sceneAge/1.65));
 const bookOpen=reducedMotion.matches?1:smoothStep((bookAge-1.35)/.67);
 motionStyle(ui.inventoryPanel,'--book-open',String(laterano?bookOpen:1));
 motionStyle(transitionCanvas,'opacity',String(laterano&&!reducedMotion.matches?1-smoothStep(transitionAge/.6):0));
 ctx.setTransform(dpr,0,0,dpr,0,0);
 const renderer=laterano&&window.DSHLateranoVisuals?window.DSHLateranoVisuals:DSHVisuals;
 renderer.draw(ctx,width,height,{phase:sequence.phase,progress:sequence.progress,visualProgress:laterano?visualProgress:undefined,bookAge:laterano?bookAge:undefined,bookSource:laterano?bookSource:undefined,bookOpen:laterano?bookOpen:undefined,time:sequence.time,textAge:laterano?(inBook?bookAge:sceneAge):manualPause?sequence.elapsed:Math.max(sequence.elapsed,phaseAge),choice:sequence.choices[Math.min(4,sequence.phase)]||0,light,reducedMotion:reducedMotion.matches,pointer,ambientTime,impulse,drag,waiting:sequence.waiting,identity:ui.identityName.value.trim()||local.profile.username,inventory:local.inventory,layout:sceneLayout});
 paintedPhase=sequence.phase;paintedProgress=visualProgress;paintedAmbientTime=ambientTime;
}
function applyVisualStyle(value){
  const next=value==='laterano'?'laterano':'rhine';
  if(next!==visualStyle)resetSceneMotion();
  visualStyle=next;
 sequence.setDurations(visualStyle==='laterano'?DSHBoot.lateranoDurations:DSHBoot.durations);
 durations=sequence.durations;total=sequence.total;
 ui.seek.max=String(total);ui.totalTime.textContent=total.toFixed(2);
 document.documentElement.dataset.visualStyle=visualStyle;
 $('visualStyle').value=visualStyle;
 layoutDirty=true;signature='';
}
function enter(node,keyframes,options){
 const old=entryAnimations.get(node);if(old){old.cancel();entryAnimations.delete(node);}
 if(reducedMotion.matches||typeof node.animate!=='function')return;
 const animation=node.animate(keyframes,{easing:'cubic-bezier(.16,.84,.2,1)',...options});entryAnimations.set(node,animation);
 animation.onfinish=()=>{if(entryAnimations.get(node)===animation)entryAnimations.delete(node);};
}
function revealTitle(showCard){
 if(sequence.phase===0||sequence.phase===5)return;
 if(showCard)enter(ui.identityCard,[{opacity:0,clipPath:'inset(0 0 100%)'},{opacity:1,clipPath:'inset(0)'}],{duration:230,delay:45});
}
function renderLocal(){
 if(accessBlocked()&&sequence.phase>2){generation++;starting=false;sequence.jump(2);phaseAge=0;manualPause=false;stopAudio();cancelVoice();}
 if(!soundInitialized&&local.connection==='local'){
  soundInitialized=true;
  if(!soundTouched){muted=local.profile.sound===false;syncSound();}
 }
 if(profileSeen!==local.profile){
  profileSeen=local.profile;
  if(local.connection==='local')applyVisualStyle(pendingVisualStyle??local.profile.visual_style);
  if(!profileDirty){ui.identityName.value=local.profile.username;ui.identityId.value=local.profile.badge_id;updateIdentity();}
 }
 ui.connectionStatus.textContent=local.connectionMessage;
 setText($('runtimeBadge'),local.profile.runtime==='native'?'NATIVE':'RUST',{kind:'roll',scope:'global'});
 ui.connectLocal.disabled=local.connection==='connecting';
 ui.saveIdentity.disabled=local.saving||local.locked;
 $('visualStyle').disabled=local.saving;
 $('retryStyle').disabled=local.saving||local.locked||local.connection!=='local';
 ui.identityName.disabled=local.locked;ui.identityId.disabled=local.locked;
 ui.accessEntry.hidden=!local.locked;ui.accessState.hidden=local.locked;
 ui.accessPassword.disabled=local.unlocking;ui.accessSubmit.disabled=local.unlocking;
 ui.accessRetry.hidden=local.connection!=='unavailable'||!accessBlocked();
 ui.identityCard.setAttribute('data-access',local.locked?'locked':local.access.enabled?'verified':'open');
 setText(ui.accessState,local.connection==='connecting'?'CHECKING ACCESS':local.connection!=='local'?'LOCAL HOST REQUIRED':local.access.enabled?'ACCESS VERIFIED':local.access.supported?'PASSWORD NOT SET':'PREVIEW ONLY',{kind:'type',delay:.33});
 ui.accessMessage.textContent=local.unlocking?'正在验证访问密码…':local.accessMessage||(local.connection==='connecting'?'正在读取访问设置…':local.connection==='unavailable'&&accessBlocked()?'无法确认访问设置，请重新连接。':local.locked?'输入密码以读取本地能力并进入 DSH。':local.access.enabled?'访问已授权 · 本次开屏已验证':local.connection==='local'&&local.access.supported?'可进入设置 → 个人资料，设置访问密码。':'');
 ui.profileStatus.textContent=local.saving?local.saveMessage:local.saveMessage.startsWith('保存失败')?local.saveMessage:profileDirty?'尚未保存 · 仅预览':local.saveMessage||(local.connection==='local'?'资料已从本地读取':'未连接本地，无法持久保存');
 ui.profileStatus.setAttribute('data-state',local.saveMessage.startsWith('保存失败')?'error':profileDirty?'draft':'saved');
 updateIdentity();
 const inventory=local.inventory;if(inventorySeen===inventory)return;inventorySeen=inventory;
 const rows=(items,kind)=>{
  const cache=inventoryRows[kind],seen=new Set();
  const result=items.map((item,index)=>{
  const sign=item.status==='loaded'?'＋':item.status==='error'?'!':'−';
  const detail=item.source||(Number.isFinite(item.tool_count)?item.tool_count+' tools':'本地插件');
  const key=(item.id||item.name)+'\0'+(item.source||'');seen.add(key);
  let nodes=cache.get(key);
  if(!nodes){nodes={row:document.createElement('div'),name:document.createElement('span'),source:document.createElement('span')};nodes.row.appendChild(nodes.name);nodes.row.appendChild(nodes.source);cache.set(key,nodes);}
  const {row,name,source}=nodes;
  row.className='inventory-row';row.setAttribute('data-status',item.status);
  name.className='inventory-name dsh-lettering';name.title=item.name;
  source.className='inventory-source dsh-lettering';source.title=detail+(item.message?' · '+item.message:'');source.setAttribute('aria-label','来源：'+source.title);
  const delay=Math.min(.3,index*.032);
  /* animate only text that actually changed. Re-running setText on every
     inventory update replayed the whole staggered roll for every row - once when the items
     streamed in and again when the final complete event landed. */
  const nameText=sign+'  '+item.name;
  if(nodes.nameText!==nameText){nodes.nameText=nameText;setText(name,nameText,{kind:'roll',delay});}
  if(nodes.sourceText!==detail){nodes.sourceText=detail;setText(source,detail,{kind:'mask',delay:delay+.09});}
  return row;
  });
  for(const [key,nodes] of cache)if(!seen.has(key)){textMotion.remove(nodes.name);textMotion.remove(nodes.source);cache.delete(key);}
  return result;
 };
 for(const [kind,list,count,status] of [['skills',ui.skillsList,ui.skillsCount,ui.skillsState],['plugins',ui.pluginsList,ui.pluginsCount,ui.pluginsState]]){
  const items=inventory[kind],loaded=items.filter(item=>item.status==='loaded').length;
  const known=inventory.status==='complete'||items.length>0||inventory.stages[kind]==='complete';
  setText(count,known?String(loaded).padStart(2,'0'):inventory.status==='loading'?'…':'—',{kind:'roll'});
  setText(status,inventory.status==='complete'?'已完成':inventory.stages[kind]==='loading'?'正在读取':inventory.stages[kind]==='complete'?'已返回':inventory.status==='loading'?'等待本地结果':'尚未读取',{kind:'roll',delay:.1});
  textMotion.remove(list);
  const itemRows=rows(items,kind);
  if(items.length){list.classList.remove('dsh-lettering');list.replaceChildren(...itemRows);}
  else setText(list,inventory.status==='complete'?'此工作区没有加载项目。':inventory.status==='loading'?'等待本地 DSH 返回真实名称…':'未读取真实数据。',{kind:'mask',delay:.1});
 }
 setText(ui.inventoryMessage,inventory.message+(inventory.status==='complete'&&inventory.elapsed_ms!==null?' · '+inventory.elapsed_ms+' ms':''),{kind:'mask'});
 setText(ui.inventoryIssues,inventory.issues.map(item=>'! '+item.name+' · '+item.message).join('\n'),{kind:'mask',delay:.1});
 ui.inventoryIssues.hidden=inventory.issues.length===0;
 ui.inventoryPanel.setAttribute('data-state',inventory.status);
  layoutDirty=true;
}
function phaseCopy(){
 const data=local.inventory,normal=phases[sequence.phase];
 if(sequence.phase===2&&local.locked)return ['访问身份确认','IDENTITY / AWAITING CLEARANCE','输入访问密码，开启本地能力档案。'];
 if(sequence.phase===1)return local.connection==='local'?['本地连接已建立','LOCAL WORKSPACE CONNECTED','准备读取当前工作区的能力档案。']:['离线演出','LOCAL HOST NOT CONNECTED','未读取真实数据 · 请用 dsh startup web 启动。'];
 if(sequence.phase===3||sequence.phase===4){
  if(data.status==='complete'){
   const tools=data.plugins.reduce((sum,item)=>sum+(item.tool_count||0),0);
   const toolSummary=data.plugins.every(item=>Number.isFinite(item.tool_count))?' · '+tools+' tools':'';
   return [data.issues.length?'加载完成 · 部分项目需检查':'能力档案已载入',data.issues.length?'LOAD COMPLETE / CHECK ISSUES':'LOCAL LOAD COMPLETE',data.skills.length+' skills · '+data.plugins.length+' plugins'+toolSummary];
  }
  if(data.status==='loading')return ['读取本地能力','READING SKILLS / PLUGINS','等待真实加载结果，画面将在完成后继续。'];
  if(['error','unavailable','cancelled'].includes(data.status))return ['真实数据尚未就绪','LOCAL DATA NOT AVAILABLE',data.message];
  return ['尚未读取本地数据','LOCAL DATA NOT READ','播放读取阶段后，显示真实的 skills 与 plugins。'];
 }
 return normal;
}
function renderUI(){
 beginScene();
 // Reading and completion are the same open folio. Preserve existing lettering
 // and reveal only newly returned names or a changed status in this chapter.
 const folio=visualStyle==='laterano'&&(sequence.phase===3||sequence.phase===4);
 const textPhase=folio?3:sequence.phase,textAge=folio?bookAge:visualStyle==='laterano'?sceneAge:manualPause?sequence.elapsed:Math.max(sequence.elapsed,phaseAge);
 textMotion.begin(textPhase,textAge,ambientTime,reducedMotion.matches,manualPause);
 const p=phaseCopy();
 const state=[sequence.phase,sequence.playing,sequence.completed,sequence.waiting,sequence.interactive,manualPause,starting,localRevision,...sequence.choices].join(':');
 if(state!==signature){
  signature=state;
  document.body.classList.toggle('on-black',sequence.phase===0);document.body.classList.toggle('finale',sequence.phase===5);
  document.body.classList.toggle('inventory-phase',sequence.phase===3||sequence.phase===4);
  document.body.classList.toggle('identity-phase',sequence.phase===2);
  ui.identityCard.hidden=sequence.phase!==2;ui.inventoryPanel.hidden=sequence.phase!==3&&sequence.phase!==4;
  const titleChanged=ui.phaseTitle.textContent!==p[0],phaseChanged=visiblePhase!==sequence.phase;
  if(titleChanged){phaseAge=0;textMotion.begin(textPhase,folio?bookAge:visualStyle==='laterano'?sceneAge:manualPause?sequence.elapsed:Math.max(sequence.elapsed,phaseAge),ambientTime,reducedMotion.matches,manualPause);}
  setText(ui.phaseTitle,p[0],{kind:'roll',delay:.04});setText(ui.phaseNote,p[2],{kind:'mask',delay:.16});
  canvas.setAttribute('aria-label',p[0]+'。'+p[2]);
  ui.play.textContent=starting?'正在开始':sequence.playing?'暂停':sequence.completed?'重播':sequence.time>0&&!sequence.waiting?'继续':'开始';ui.play.disabled=starting;
  ui.play.setAttribute('aria-label',sequence.playing?'暂停演出':sequence.completed?'重新体验':'开始或继续演出');
  ui.interactive.textContent=sequence.interactive?'三段交互':'连续播放';ui.interactive.setAttribute('aria-pressed',String(sequence.interactive));
  chapters.forEach((c,i)=>{c.classList.toggle('past',i<sequence.phase);if(i===sequence.phase)c.setAttribute('aria-current','step');else c.removeAttribute('aria-current');});
  if(titleChanged||phaseChanged){layoutDirty=true;revealTitle(phaseChanged&&sequence.phase===2);}visiblePhase=sequence.phase;
 }
 setText(ui.phaseEnglish,p[1],{kind:'type',delay:.05});
 if(sequence.phase===2)renderIdentity();
 setText(ui.subtitleZh,sequence.phase===3||sequence.phase===4?local.inventory.message:sequence.phase===5?(local.inventory.status==='complete'?'本地能力已载入 · 欢迎 '+ui.identityName.value.trim():'演出完成 · 尚未读取完整本地数据'):p[2],{kind:'mask',delay:.12});
 setText(ui.subtitleEn,sequence.phase===3&&local.inventory.status==='loading'?'WAITING FOR THE LOCAL HOST · NO SIMULATED PROGRESS':sequence.phase===5?'READY TO DIVE':p[1],{kind:'type',delay:.22});
 textMotion.render();
 const currentTime=sequence.time.toFixed(2).padStart(5,'0'),seekValue=String(sequence.time);
 const accessibleTime=sequence.time.toFixed(2)+' 秒，'+p[0]+(sequence.held?'，等待真实读取或旁白完成':'');
 if(ui.currentTime.textContent!==currentTime)ui.currentTime.textContent=currentTime;
 if(ui.seek.value!==seekValue)ui.seek.value=seekValue;
 if(ui.seek.getAttribute('aria-valuetext')!==accessibleTime)ui.seek.setAttribute('aria-valuetext',accessibleTime);
}
function frame(now){
 if(embeddedEnded)return;
 raf=0;const delta=lastFrame?Math.max(0,(now-lastFrame)/1000):0;lastFrame=now;
 if(!manualPause){phaseAge+=delta;sceneAge+=delta*speed;transitionAge+=delta;if(sequence.phase===3||sequence.phase===4)bookAge+=delta*speed;if(!reducedMotion.matches)ambientTime+=delta;}
 impulse=Math.max(0,impulse-delta*1.5);
 const previous=sequence.phase,wasPlaying=sequence.playing;
 barriers();sequence.tick(delta*speed);beginLocalLoad();
 if(sequence.phase!==previous){phaseAge=0;stopAudio();narrate();if(sequence.playing)scorePhase();announce(sequence.waiting?phases[sequence.phase][0]+'。点按任意位置确认。':'进入'+phases[sequence.phase][0]);}
 if(wasPlaying&&!sequence.playing){stopAudio();if(sequence.completed)announce('深潜就绪。点按任意位置重新体验。');}
 renderUI();drawScene();
 if(sequence.completed&&finishEmbedded('complete'))return;
 if(!raf&&!document.hidden&&(!reducedMotion.matches||sequence.playing))raf=requestAnimationFrame(frame);
}
function wake(){if(!embeddedEnded&&!raf&&!document.hidden){lastFrame=performance.now();raf=requestAnimationFrame(frame);}}
function refresh(){renderUI();drawScene();wake();}
function resize(){
 const r=pane.getBoundingClientRect(),ratio=Math.min(devicePixelRatio||1,2.5);
 if(width===r.width&&height===r.height&&dpr===ratio){layoutDirty=true;wake();return;}
 width=r.width;height=r.height;dpr=ratio;
 canvas.width=Math.round(width*dpr);canvas.height=Math.round(height*dpr);clearTransition();layoutDirty=true;refresh();
}
async function start(){
 if(starting||embeddedEnded)return;
 if(sequence.completed){restart(true);return;}
 const voiceReady=Promise.resolve(voice()?.unlock?.()).catch(()=>{});
 const current=++generation;starting=true;manualPause=false;stopAudio();refresh();
 const [ready]=await Promise.all([ensureAudio(),voiceReady,soundTouched?Promise.resolve():local.connect()]);if(current!==generation)return;
 starting=false;if(document.hidden){refresh();return;}
 sequence.play();beginLocalLoad();narrate();barriers();lastFrame=performance.now();if(ready)scorePhase();refresh();announce('开始'+phases[sequence.phase][0]+'。播放中点按可触发扫描波。');
}
function pause(message='演出已暂停。点按画面继续。'){generation++;starting=false;sequence.pause();manualPause=true;clearTransition();stopAudio();cancelVoice();local.cancelLoad(false);refresh();announce(message);}
function restart(playNow=false){generation++;starting=false;stopAudio();cancelVoice();sequence.restart();phaseAge=0;manualPause=false;ambientTime=0;impulse=.6;resetSceneMotion();ui.accessPassword.value='';local.resetVerification();local.cancelLoad();refresh();if(playNow||!sequence.interactive)start();else announce('已回到序章。点按任意位置开始。');}
function requireAccess(){if(sequence.phase!==2)jump(2);if(local.locked)ui.accessPassword.focus({preventScroll:true});announce(local.locked?'请先验证访问密码。':local.connection==='connecting'?'正在读取访问设置，请稍候。':'无法确认访问设置，请重新连接。');}
function activate(){if(sequence.phase>=2&&accessBlocked()){requireAccess();return;}impulse=1;if(sequence.playing){clickSound();refresh();return;}if(sequence.completed)restart(true);else start();}
function jump(phase){if(accessBlocked()&&phase>2)phase=2;generation++;starting=false;stopAudio();cancelVoice();sequence.jump(phase);phaseAge=0;manualPause=false;impulse=.65;local.cancelLoad();refresh();announce('已切换到'+phases[phase][0]+'。点按画面播放。');}
function skip(){if(accessBlocked()){requireAccess();return;}generation++;starting=false;stopAudio();cancelVoice();sequence.seek(total);phaseAge=0;manualPause=false;local.cancelLoad(false);refresh();announce('已跳过启动演出。');finishEmbedded('skip');}
function finishEmbedded(reason){
 if(!window.DSHEmbed?.embedded||embeddedEnded)return embeddedEnded;
 if(accessBlocked()){requireAccess();return false;}
 embeddedEnded=true;generation++;starting=false;sequence.pause();stopAudio();cancelVoice();
 local.dispose();cancelAnimationFrame(raf);raf=0;
 entryAnimations.forEach(animation=>animation.cancel());entryAnimations.clear();
 if(audio&&audio.state!=='closed')void audio.close();
 window.DSHEmbed.finish(reason);return true;
}
function syncSound(){voice()?.setMuted?.(muted);ui.sound.textContent=muted?'声音 · 关':'声音 · 开';ui.sound.setAttribute('aria-pressed',String(!muted));ui.sound.setAttribute('aria-label',muted?'声音已关闭，点击开启':'声音已开启，点击关闭');if(muted)stopAudio();}
function syncTheme(){document.documentElement.dataset.theme=light?'light':'dark';ui.theme.textContent=light?'深色':'浅色';ui.theme.setAttribute('aria-label',light?'切换到深色档案':'切换到浅色档案');}
function toggleSound(){if(audioUnavailable&&!voice())return;soundTouched=true;muted=!muted;syncSound();if(!muted){const current=generation;ensureAudio().then(ready=>{if(ready&&current===generation&&sequence.playing)scorePhase();});}}
function settings(open){ui.controlPanel.hidden=!open;ui.settings.setAttribute('aria-expanded',String(open));ui.settings.setAttribute('aria-label',open?'关闭动画设置':'打开动画设置');if(open)enter(ui.controlPanel,[{clipPath:'inset(0 0 100%)'},{clipPath:'inset(0)'}],{duration:300});(open?ui.closeSettings:ui.settings).focus({preventScroll:true});}
function point(event){const r=stage.getBoundingClientRect();pointer={x:Math.max(0,Math.min(1,(event.clientX-r.left)/r.width)),y:Math.max(0,Math.min(1,(event.clientY-r.top)/r.height))};}
stage.addEventListener('pointerdown',e=>{if(e.button!==0||pointerDown||e.target.closest('.inventory-list,.inventory-issues,#accessForm,#accessRetry'))return;point(e);pointerDown={id:e.pointerId,x:e.clientX,y:e.clientY,lastX:e.clientX,moved:false};stage.setPointerCapture(e.pointerId);stage.focus({preventScroll:true});});
stage.addEventListener('pointermove',e=>{point(e);if(pointerDown&&e.pointerId===pointerDown.id){if(Math.hypot(e.clientX-pointerDown.x,e.clientY-pointerDown.y)>6)pointerDown.moved=true;drag+=(e.clientX-pointerDown.lastX)/Math.max(1,width)*Math.PI*2;pointerDown.lastX=e.clientX;stage.classList.toggle('dragging',pointerDown.moved);}if(reducedMotion.matches)drawScene();});
stage.addEventListener('pointerup',e=>{if(!pointerDown||e.pointerId!==pointerDown.id)return;const moved=pointerDown.moved;pointerDown=null;stage.classList.remove('dragging');if(stage.hasPointerCapture(e.pointerId))stage.releasePointerCapture(e.pointerId);if(!moved)activate();else{impulse=.45;refresh();}});
stage.addEventListener('pointercancel',()=>{pointerDown=null;stage.classList.remove('dragging');});
stage.addEventListener('pointerleave',()=>{if(!pointerDown)pointer={x:.5,y:.5};});
ui.play.addEventListener('click',()=>sequence.playing?pause():activate());
ui.skip.addEventListener('click',skip);ui.replay.addEventListener('click',()=>restart());
ui.sound.addEventListener('click',toggleSound);
ui.theme.addEventListener('click',()=>{light=!light;syncTheme();clearTransition();refresh();});
ui.settings.addEventListener('click',()=>settings(ui.controlPanel.hidden));ui.closeSettings.addEventListener('click',()=>settings(false));
ui.interactive.addEventListener('click',()=>{sequence.interactive=!sequence.interactive;refresh();announce(sequence.interactive?'已启用三个确认节点。':'已切换为连续播放。');});
ui.speed.addEventListener('change',()=>{speed=Number(ui.speed.value);lastFrame=performance.now();if(sequence.playing)scorePhase();});
ui.seek.addEventListener('input',()=>{let t=Number(ui.seek.value);if(accessBlocked()&&t>=durations[0]+durations[1]+durations[2])t=durations[0]+durations[1];generation++;starting=false;stopAudio();cancelVoice();sequence.seek(t);manualPause=true;phaseAge=sequence.elapsed;resetSceneMotion();local.cancelLoad();refresh();});
chapters.forEach((c,i)=>c.addEventListener('click',()=>jump(i)));
document.addEventListener('keydown',e=>{
 if(e.isComposing||e.keyCode===229)return;
 if(e.altKey||e.ctrlKey||e.metaKey)return;
 if(e.target.closest('#accessForm'))return;
 if(e.repeat){if(e.key==='Enter'||e.key===' ')e.preventDefault();return;}
 if(e.key==='Escape'){e.preventDefault();if(!ui.controlPanel.hidden)settings(false);else skip();return;}
 if(e.target.closest('input,select,textarea,[contenteditable]'))return;
 if(e.key.toLowerCase()==='c'){e.preventDefault();settings(ui.controlPanel.hidden);return;}
 if(e.key.toLowerCase()==='m'){e.preventDefault();toggleSound();return;}
 if(e.key.toLowerCase()==='r'){e.preventDefault();restart();return;}
 if(e.key.toLowerCase()==='p'){e.preventDefault();manualPause?start():pause();return;}
 if(/^[123]$/.test(e.key)){e.preventDefault();if(sequence.choose(Number(e.key)-1)){impulse=.8;refresh();announce('已切换扫描通道 '+e.key);}return;}
 if((e.key==='ArrowLeft'||e.key==='ArrowRight')&&sequence.phase<5){e.preventDefault();sequence.choose((sequence.choices[sequence.phase]+(e.key==='ArrowRight'?1:2))%3);impulse=.5;refresh();return;}
 if((e.key==='Enter'||e.key===' ')&&!e.target.closest('button')){e.preventDefault();activate();}
});
document.addEventListener('visibilitychange',()=>{if(document.hidden){if(sequence.playing||starting)pause('页面在后台，演出已暂停。');else cancelVoice();cancelAnimationFrame(raf);raf=0;stopAudio();}else wake();});
function motionChange(){document.body.classList.toggle('reduced',reducedMotion.matches);if(reducedMotion.matches){clearTransition();entryAnimations.forEach(animation=>animation.cancel());entryAnimations.clear();}refresh();}reducedMotion.addEventListener('change',motionChange);
function renderIdentity(){identityView.render(sequence.phase===2?(visualStyle==='laterano'?sceneAge:manualPause?sequence.elapsed:Math.max(sequence.elapsed,phaseAge)):2,reducedMotion.matches);}
function updateIdentity(){
 const name=ui.identityName.value.trim()||'OPERATOR',badge=ui.identityId.value.trim()||'DSH-0001';
 const mode=profileDirty?'UNSAVED / PREVIEW':local.connection==='local'?'LOCAL / SAVED':'OFFLINE / PREVIEW';
 identityView.set([name,badge,mode]);renderIdentity();setText(ui.identityLine,'ID: '+badge,{kind:'type',scope:'global',delay:.3});layoutDirty=true;drawScene();
}
function editIdentity(){profileDirty=true;draftRevision++;local.saveMessage='';updateIdentity();renderLocal();}
ui.identityName.addEventListener('input',editIdentity);ui.identityId.addEventListener('input',editIdentity);
async function savePendingStyle(){
 if(pendingVisualStyle===null)return true;
 if(local.connection!=='local'||local.locked){$('styleStatus').textContent='当前选择已保留 · 连接并验证后自动保存风格';return false;}
 if(styleSaving)return styleSaving.promise;
 const selected=pendingVisualStyle,revision=styleRevision,task={promise:null};
 styleSaving=task;
 task.promise=(async()=>{
  const saved=await local.saveStyle(selected);
  if(revision===styleRevision){
   if(saved)pendingVisualStyle=null;
   $('retryStyle').hidden=saved;
   $('styleStatus').textContent=saved?'已保存 · 网页、桌面与终端下次启动使用此风格':local.saveMessage+' · 本次预览仍使用当前选择，可重试保存';
  }
  if(saved)window.DSHEmbed?.profileChanged();
  refresh();if(sequence.playing)scorePhase();
  return saved;
 })();
 try{return await task.promise;}finally{if(styleSaving===task)styleSaving=null;}
}
$('visualStyle').addEventListener('change',async()=>{
 pendingVisualStyle=$('visualStyle').value;styleRevision++;
 $('retryStyle').hidden=true;
 applyVisualStyle(pendingVisualStyle);refresh();
 await savePendingStyle();
});
$('retryStyle').addEventListener('click',()=>{void savePendingStyle();});
ui.accessForm.addEventListener('submit',async event=>{
 event.preventDefault();if(!local.locked||local.unlocking)return;
 const password=ui.accessPassword.value;
 if(!password){ui.accessMessage.textContent='请输入访问密码。';ui.accessPassword.focus({preventScroll:true});return;}
 ui.accessPassword.value='';
 const current=generation,unlocked=await local.unlock(password);
 if(embeddedEnded||current!==generation)return;
 if(unlocked){
   await savePendingStyle();
   if(embeddedEnded||current!==generation)return;
  window.DSHEmbed?.unlocked();
  if(visualStyle==='laterano'&&sequence.phase===2){
   // The identity has already assembled while the operator types. Keep a short
   // confirmation beat, then continue the same geometry into the folio. Voice
   // and access barriers still own whether the sequence may leave this chapter.
   const remaining=Math.max(.45*speed,reducedMotion.matches?0:1.65-sceneAge);
   sequence.elapsed=Math.max(0,durations[2]-remaining);
  }
  impulse=1;barriers();if(current===generation&&sequence.phase===2&&!sequence.playing)void start();refresh();announce('访问验证通过。');
 }
 else{ui.accessPassword.focus({preventScroll:true});announce(local.accessMessage);}
});
ui.saveIdentity.addEventListener('click',async()=>{const revision=draftRevision;const saved=await local.save(ui.identityName.value,ui.identityId.value);if(saved)window.DSHEmbed?.profileChanged();if(saved&&revision===draftRevision){profileDirty=false;ui.identityName.value=local.profile.username;ui.identityId.value=local.profile.badge_id;updateIdentity();}renderLocal();announce(local.saveMessage);});
async function reconnectLocal(){const connected=await local.connect(true);if(connected)await savePendingStyle();if(connected&&sequence.phase===3&&!sequence.completed){local.cancelLoad();beginLocalLoad();}renderLocal();refresh();}
ui.connectLocal.addEventListener('click',reconnectLocal);ui.accessRetry.addEventListener('click',reconnectLocal);
new ResizeObserver(resize).observe(pane);window.addEventListener('resize',resize);
// A batch of changed text/page bounds produces one measurement and draw on the
// next frame, rather than recursively repainting inside ResizeObserver delivery.
const invalidateGeometry=()=>{layoutDirty=true;wake();};
const geometryObserver=new ResizeObserver(invalidateGeometry);geometryObserver.observe($('stageCopy'));geometryObserver.observe($('stageHeading'));geometryObserver.observe(ui.identityCard);geometryObserver.observe(ui.inventoryPanel);geometryObserver.observe($('inventoryBook'));inventoryColumns.forEach(node=>geometryObserver.observe(node));
for(const node of [$('skillsHeading'),$('pluginsHeading'),ui.skillsList,ui.pluginsList])geometryObserver.observe(node);
for(const list of [ui.skillsList,ui.pluginsList])list.addEventListener('scroll',invalidateGeometry,{passive:true});
document.fonts?.ready.then(()=>{layoutDirty=true;refresh();});
 window.addEventListener('pagehide',()=>{generation++;starting=false;sequence.pause();manualPause=true;clearTransition();stopAudio();cancelVoice();local.dispose();cancelAnimationFrame(raf);raf=0;if(audio&&audio.state!=='closed')audio.close();});
window.addEventListener('pageshow',event=>{if(event.persisted){ui.accessPassword.value='';local.resetVerification();lastFrame=performance.now();void local.connect(true).then(connected=>connected&&savePendingStyle());refresh();announce('预览已恢复。点按画面继续演出。');}});
window.DSHEmbed?.onSkipRequest(skip);
syncTheme();applyVisualStyle(visualStyle);renderLocal();motionChange();resize();void local.connect().then(connected=>connected&&savePendingStyle());window.DSHEmbed?.ready();
})();
