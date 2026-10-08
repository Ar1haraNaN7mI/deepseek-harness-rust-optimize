/* Original cinematic host. The canvas itself is the interaction surface. */
(() => {
'use strict';
const $ = id => document.getElementById(id);
if (!window.DSHBoot || !window.DSHVisuals || !window.DSHLocal || !window.DSHIdentity || !window.DSHText) { $('canvasPane').insertAdjacentHTML('beforeend','<p class="error">请保留同目录的完整预览资源。</p>'); $('play').disabled=true; return; }
const sequence = new DSHBoot.Sequence();
const {total,durations} = DSHBoot;
const scoreDurations=[2.8,2.8,3.4,3.2,2.8,3];
const ui = Object.fromEntries(['sound','theme','play','skip','settings','closeSettings','controlPanel','replay','interactive','speed','seek','currentTime','totalTime','phaseTitle','phaseEnglish','phaseNote','status','identityCard','identityName','identityId','cardName','cardId','identityLine','subtitleZh','subtitleEn','saveIdentity','profileStatus','connectionStatus','connectLocal','inventoryPanel','inventoryMessage','skillsCount','pluginsCount','skillsState','pluginsState','skillsList','pluginsList','inventoryIssues','cardMode','cardNameVisual','cardIdVisual','cardModeVisual','accessForm','accessEntry','accessPassword','accessSubmit','accessState','accessMessage'].map(id => [id,$(id)]));
const identityView=new DSHIdentity.View(['cardName','cardId','cardMode'].map(id=>({value:ui[id],visual:ui[id+'Visual']})));
const textMotion=new DSHText.Stage(document),inventoryRows={skills:new Map(),plugins:new Map()};
const setText=(node,value,options={})=>textMotion.set(node,value,options);
const identityRows=[...document.querySelectorAll('.identity-row')],inventoryColumns=[...document.querySelectorAll('.inventory-column')];
const canvas=$('scene'), pane=$('canvasPane'), stage=$('experience'), ctx=canvas.getContext('2d',{alpha:false});
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
let light=true,muted=false,speed=1,width=1,height=1,dpr=1;
let layoutDirty=true,sceneLayout={identity:null,inventory:null};
let soundTouched=false,soundInitialized=false;
let audio=null,master=null,audioSources=[],audioUnavailable=false;
let phaseAge=0,visiblePhase=-1,starting=false;
const entryAnimations=new Map();
let raf=0,lastFrame=0,generation=0,ambientTime=0,impulse=0,drag=0,manualPause=false,signature='',pointer={x:.5,y:.5},pointerDown=null,embeddedEnded=false;
const announce=message=>{ui.status.textContent=message;};
let profileDirty=false,draftRevision=0,localRevision=0,inventorySeen=null,profileSeen=null;
const local=new DSHLocal.Client({onChange:()=>{localRevision++;renderLocal();refresh();}});
const voice=()=>window.DSHVoice;
let narratedPhase=-1;
const cancelVoice=()=>{narratedPhase=-1;voice()?.cancel?.();};
const narrate=()=>{if(narratedPhase===sequence.phase)return;narratedPhase=sequence.phase;voice()?.phase?.(sequence.phase,{username:ui.identityName.value.trim()||local.profile.username,inventory:local.inventory,connection:local.connection,inventoryMode:local.profile.inventory_mode});};
const loadFinished=()=>['complete','error','unavailable'].includes(local.inventory.status);
function barriers(){for(let i=0;i<6;i++){if((i===2&&local.locked)||(i===3&&!loadFinished())||(i===sequence.phase&&voice()?.busy))sequence.hold(i);else sequence.release(i);}}
function beginLocalLoad(){if(!local.locked&&sequence.phase===3&&sequence.playing&&['idle','cancelled'].includes(local.inventory.status))void local.load();}
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
      if (audio.state === 'suspended') await audio.resume();
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
 sceneLayout={
  identity:ui.identityCard.hidden?null:{...rect(ui.identityCard),rows:identityRows.map(node=>{const r=rect(node);return{y:r.y,height:r.height};})},
  inventory:ui.inventoryPanel.hidden?null:{...rect(ui.inventoryPanel),columns:inventoryColumns.map(rect)}
 };layoutDirty=false;
}
function drawScene(){if(!ctx)return;if(layoutDirty)measureSceneLayout();ctx.setTransform(dpr,0,0,dpr,0,0);DSHVisuals.draw(ctx,width,height,{phase:sequence.phase,progress:sequence.progress,time:sequence.time,textAge:manualPause?sequence.elapsed:Math.max(sequence.elapsed,phaseAge),choice:sequence.choices[Math.min(4,sequence.phase)]||0,light,reducedMotion:reducedMotion.matches,pointer,ambientTime,impulse,drag,waiting:sequence.waiting,identity:ui.identityName.value.trim()||local.profile.username,inventory:local.inventory,layout:sceneLayout});}
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
 if(local.locked&&sequence.phase>2){generation++;starting=false;sequence.jump(2);phaseAge=0;manualPause=false;stopAudio();cancelVoice();}
 if(!soundInitialized&&local.connection==='local'){
  soundInitialized=true;
  if(!soundTouched){muted=local.profile.sound===false;syncSound();}
 }
 if(profileSeen!==local.profile){
  profileSeen=local.profile;
  if(!profileDirty){ui.identityName.value=local.profile.username;ui.identityId.value=local.profile.badge_id;updateIdentity();}
 }
 ui.connectionStatus.textContent=local.connectionMessage;
 setText($('runtimeBadge'),local.profile.runtime==='native'?'NATIVE':'RUST',{kind:'roll',scope:'global'});
 ui.connectLocal.disabled=local.connection==='connecting';
 ui.saveIdentity.disabled=local.saving||local.locked;
 ui.identityName.disabled=local.locked;ui.identityId.disabled=local.locked;
 ui.accessEntry.hidden=!local.locked;ui.accessState.hidden=local.locked;
 ui.accessPassword.disabled=local.unlocking;ui.accessSubmit.disabled=local.unlocking;
 ui.identityCard.setAttribute('data-access',local.locked?'locked':local.access.enabled?'verified':'open');
 setText(ui.accessState,local.connection!=='local'?'LOCAL HOST REQUIRED':local.access.enabled?'ACCESS VERIFIED':local.access.supported?'PASSWORD NOT SET':'PREVIEW ONLY',{kind:'type',delay:.33});
 ui.accessMessage.textContent=local.unlocking?'正在验证访问密码…':local.accessMessage||(local.locked?'输入密码以读取本地能力并进入 DSH。':local.access.enabled?'访问已授权 · 本次访问已解锁':local.connection==='local'&&local.access.supported?'可进入设置 → 个人资料，设置访问密码。':'');
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
  setText(name,sign+'  '+item.name,{kind:'roll',delay});setText(source,detail,{kind:'mask',delay:delay+.09});return row;
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
 textMotion.begin(sequence.phase,manualPause?sequence.elapsed:Math.max(sequence.elapsed,phaseAge),ambientTime,reducedMotion.matches,manualPause);
 const p=phaseCopy();
 const state=[sequence.phase,sequence.playing,sequence.completed,sequence.waiting,sequence.interactive,manualPause,starting,localRevision,...sequence.choices].join(':');
 if(state!==signature){
  signature=state;
  document.body.classList.toggle('on-black',sequence.phase===0);document.body.classList.toggle('finale',sequence.phase===5);
  document.body.classList.toggle('inventory-phase',sequence.phase===3||sequence.phase===4);
  document.body.classList.toggle('identity-phase',sequence.phase===2);
  ui.identityCard.hidden=sequence.phase!==2;ui.inventoryPanel.hidden=sequence.phase!==3&&sequence.phase!==4;
  const titleChanged=ui.phaseTitle.textContent!==p[0],phaseChanged=visiblePhase!==sequence.phase;
  if(titleChanged){phaseAge=0;textMotion.begin(sequence.phase,manualPause?sequence.elapsed:Math.max(sequence.elapsed,phaseAge),ambientTime,reducedMotion.matches,manualPause);}
  setText(ui.phaseTitle,p[0],{kind:'roll',delay:.04});setText(ui.phaseNote,p[2],{kind:'mask',delay:.16});
  canvas.setAttribute('aria-label',p[0]+'。'+p[2]);
  ui.play.textContent=starting?'正在开始':sequence.playing?'暂停':sequence.completed?'重播':sequence.time>0&&!sequence.waiting?'继续':'开始';ui.play.disabled=starting;
  ui.play.setAttribute('aria-label',sequence.playing?'暂停演出':sequence.completed?'重新体验':'开始或继续演出');
  ui.interactive.textContent=sequence.interactive?'三段交互':'连续播放';ui.interactive.setAttribute('aria-pressed',String(sequence.interactive));
  chapters.forEach((c,i)=>{c.classList.toggle('past',i<sequence.phase);if(i===sequence.phase)c.setAttribute('aria-current','step');else c.removeAttribute('aria-current');});
  if(titleChanged||phaseChanged){layoutDirty=true;revealTitle(phaseChanged&&sequence.phase===2);}visiblePhase=sequence.phase;
 }
 setText(ui.phaseEnglish,p[1],{kind:'type',delay:.05});
 renderIdentity();
 setText(ui.subtitleZh,sequence.phase===3||sequence.phase===4?local.inventory.message:sequence.phase===5?(local.inventory.status==='complete'?'本地能力已载入 · 欢迎 '+ui.identityName.value.trim():'演出完成 · 尚未读取完整本地数据'):p[2],{kind:'mask',delay:.12});
 setText(ui.subtitleEn,sequence.phase===3&&local.inventory.status==='loading'?'WAITING FOR THE LOCAL HOST · NO SIMULATED PROGRESS':sequence.phase===5?'READY TO DIVE':p[1],{kind:'type',delay:.22});
 textMotion.render();
 ui.currentTime.textContent=sequence.time.toFixed(2).padStart(5,'0');ui.seek.value=String(sequence.time);ui.seek.setAttribute('aria-valuetext',sequence.time.toFixed(2)+' 秒，'+p[0]+(sequence.held?'，等待真实读取或旁白完成':''));
}
function frame(now){
 if(embeddedEnded)return;
 raf=0;const delta=lastFrame?Math.max(0,(now-lastFrame)/1000):0;lastFrame=now;
 if(!manualPause){phaseAge+=delta;if(!reducedMotion.matches)ambientTime+=delta;}
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
function resize(){const r=pane.getBoundingClientRect();width=r.width;height=r.height;dpr=Math.min(devicePixelRatio||1,2.5);canvas.width=Math.round(width*dpr);canvas.height=Math.round(height*dpr);layoutDirty=true;refresh();}
async function start(){
 if(starting||embeddedEnded)return;
 if(sequence.completed){restart(true);return;}
 const voiceReady=Promise.resolve(voice()?.unlock?.()).catch(()=>{});
 const current=++generation;starting=true;manualPause=false;stopAudio();refresh();
 const [ready]=await Promise.all([ensureAudio(),voiceReady,soundTouched?Promise.resolve():local.connect()]);if(current!==generation)return;
 starting=false;if(document.hidden){refresh();return;}
 sequence.play();beginLocalLoad();narrate();barriers();lastFrame=performance.now();if(ready)scorePhase();refresh();announce('开始'+phases[sequence.phase][0]+'。播放中点按可触发扫描波。');
}
function pause(message='演出已暂停。点按画面继续。'){generation++;starting=false;sequence.pause();manualPause=true;stopAudio();cancelVoice();local.cancelLoad(false);refresh();announce(message);}
function restart(playNow=false){generation++;starting=false;stopAudio();cancelVoice();sequence.restart();phaseAge=0;manualPause=false;ambientTime=0;impulse=.6;local.cancelLoad();refresh();if(playNow||!sequence.interactive)start();else announce('已回到序章。点按任意位置开始。');}
function activate(){impulse=1;if(sequence.playing){clickSound();refresh();return;}if(sequence.completed)restart(true);else start();}
function jump(phase){if(local.locked&&phase>2)phase=2;generation++;starting=false;stopAudio();cancelVoice();sequence.jump(phase);phaseAge=0;manualPause=false;impulse=.65;local.cancelLoad();refresh();announce('已切换到'+phases[phase][0]+'。点按画面播放。');}
function skip(){if(local.locked){jump(2);ui.accessPassword.focus({preventScroll:true});announce('请先验证访问密码。');return;}generation++;starting=false;stopAudio();cancelVoice();sequence.seek(total);phaseAge=0;manualPause=false;local.cancelLoad(false);refresh();announce('已跳过启动演出。');finishEmbedded('skip');}
function finishEmbedded(reason){
 if(!window.DSHEmbed?.embedded||embeddedEnded)return embeddedEnded;
 embeddedEnded=true;generation++;starting=false;sequence.pause();stopAudio();cancelVoice();
 local.dispose();cancelAnimationFrame(raf);raf=0;
 entryAnimations.forEach(animation=>animation.cancel());entryAnimations.clear();
 if(audio&&audio.state!=='closed')void audio.close();
 window.DSHEmbed.finish(reason);return true;
}
function syncSound(){voice()?.setMuted?.(muted);ui.sound.textContent=muted?'声音 · 关':'声音 · 开';ui.sound.setAttribute('aria-pressed',String(!muted));ui.sound.setAttribute('aria-label',muted?'声音已关闭，点击开启':'声音已开启，点击关闭');if(muted)stopAudio();}
function toggleSound(){if(audioUnavailable&&!voice())return;soundTouched=true;muted=!muted;syncSound();if(!muted){const current=generation;ensureAudio().then(ready=>{if(ready&&current===generation&&sequence.playing)scorePhase();});}}
function settings(open){ui.controlPanel.hidden=!open;ui.settings.setAttribute('aria-expanded',String(open));ui.settings.setAttribute('aria-label',open?'关闭动画设置':'打开动画设置');if(open)enter(ui.controlPanel,[{clipPath:'inset(0 0 100%)'},{clipPath:'inset(0)'}],{duration:300});(open?ui.closeSettings:ui.settings).focus({preventScroll:true});}
function point(event){const r=stage.getBoundingClientRect();pointer={x:Math.max(0,Math.min(1,(event.clientX-r.left)/r.width)),y:Math.max(0,Math.min(1,(event.clientY-r.top)/r.height))};}
stage.addEventListener('pointerdown',e=>{if(e.button!==0||pointerDown||e.target.closest('.inventory-list,.inventory-issues,#accessForm'))return;point(e);pointerDown={id:e.pointerId,x:e.clientX,y:e.clientY,lastX:e.clientX,moved:false};stage.setPointerCapture(e.pointerId);stage.focus({preventScroll:true});});
stage.addEventListener('pointermove',e=>{point(e);if(pointerDown&&e.pointerId===pointerDown.id){if(Math.hypot(e.clientX-pointerDown.x,e.clientY-pointerDown.y)>6)pointerDown.moved=true;drag+=(e.clientX-pointerDown.lastX)/Math.max(1,width)*Math.PI*2;pointerDown.lastX=e.clientX;stage.classList.toggle('dragging',pointerDown.moved);}if(reducedMotion.matches)drawScene();});
stage.addEventListener('pointerup',e=>{if(!pointerDown||e.pointerId!==pointerDown.id)return;const moved=pointerDown.moved;pointerDown=null;stage.classList.remove('dragging');if(stage.hasPointerCapture(e.pointerId))stage.releasePointerCapture(e.pointerId);if(!moved)activate();else{impulse=.45;refresh();}});
stage.addEventListener('pointercancel',()=>{pointerDown=null;stage.classList.remove('dragging');});
stage.addEventListener('pointerleave',()=>{if(!pointerDown)pointer={x:.5,y:.5};});
ui.play.addEventListener('click',()=>sequence.playing?pause():activate());
ui.skip.addEventListener('click',skip);ui.replay.addEventListener('click',()=>restart());
ui.sound.addEventListener('click',toggleSound);
ui.theme.addEventListener('click',()=>{light=!light;document.documentElement.dataset.theme=light?'light':'dark';ui.theme.textContent=light?'深色':'浅色';ui.theme.setAttribute('aria-label',light?'切换到深色档案':'切换到浅色档案');refresh();});
ui.settings.addEventListener('click',()=>settings(ui.controlPanel.hidden));ui.closeSettings.addEventListener('click',()=>settings(false));
ui.interactive.addEventListener('click',()=>{sequence.interactive=!sequence.interactive;refresh();announce(sequence.interactive?'已启用三个确认节点。':'已切换为连续播放。');});
ui.speed.addEventListener('change',()=>{speed=Number(ui.speed.value);lastFrame=performance.now();if(sequence.playing)scorePhase();});
ui.seek.addEventListener('input',()=>{let t=Number(ui.seek.value);if(local.locked&&t>=durations[0]+durations[1]+durations[2])t=durations[0]+durations[1];generation++;starting=false;stopAudio();cancelVoice();sequence.seek(t);manualPause=true;phaseAge=sequence.elapsed;local.cancelLoad();refresh();});
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
 if(e.key.toLowerCase()==='p'){e.preventDefault();sequence.playing?pause():start();return;}
 if(/^[123]$/.test(e.key)){e.preventDefault();if(sequence.choose(Number(e.key)-1)){impulse=.8;refresh();announce('已切换扫描通道 '+e.key);}return;}
 if((e.key==='ArrowLeft'||e.key==='ArrowRight')&&sequence.phase<5){e.preventDefault();sequence.choose((sequence.choices[sequence.phase]+(e.key==='ArrowRight'?1:2))%3);impulse=.5;refresh();return;}
 if((e.key==='Enter'||e.key===' ')&&!e.target.closest('button')){e.preventDefault();activate();}
});
document.addEventListener('visibilitychange',()=>{if(document.hidden){if(sequence.playing||starting)pause('页面在后台，演出已暂停。');else cancelVoice();cancelAnimationFrame(raf);raf=0;stopAudio();}else wake();});
function motionChange(){document.body.classList.toggle('reduced',reducedMotion.matches);if(reducedMotion.matches){entryAnimations.forEach(animation=>animation.cancel());entryAnimations.clear();}refresh();}reducedMotion.addEventListener('change',motionChange);
function renderIdentity(){identityView.render(sequence.phase===2?(manualPause?sequence.elapsed:Math.max(sequence.elapsed,phaseAge)):2,reducedMotion.matches);}
function updateIdentity(){
 const name=ui.identityName.value.trim()||'OPERATOR',badge=ui.identityId.value.trim()||'DSH-0001';
 const mode=profileDirty?'UNSAVED / PREVIEW':local.connection==='local'?'LOCAL / SAVED':'OFFLINE / PREVIEW';
 identityView.set([name,badge,mode]);renderIdentity();setText(ui.identityLine,'ID: '+badge,{kind:'type',scope:'global',delay:.3});layoutDirty=true;drawScene();
}
function editIdentity(){profileDirty=true;draftRevision++;local.saveMessage='';updateIdentity();renderLocal();}
ui.identityName.addEventListener('input',editIdentity);ui.identityId.addEventListener('input',editIdentity);
ui.accessForm.addEventListener('submit',async event=>{
 event.preventDefault();if(!local.locked||local.unlocking)return;
 const password=ui.accessPassword.value;
 if(!password){ui.accessMessage.textContent='请输入访问密码。';ui.accessPassword.focus({preventScroll:true});return;}
 ui.accessPassword.value='';
 const current=generation,unlocked=await local.unlock(password);
 if(embeddedEnded)return;
 if(unlocked){window.DSHEmbed?.unlocked();impulse=1;barriers();if(current===generation&&sequence.phase===2&&!sequence.playing)void start();refresh();announce('访问验证通过。');}
 else{ui.accessPassword.focus({preventScroll:true});announce(local.accessMessage);}
});
ui.saveIdentity.addEventListener('click',async()=>{const revision=draftRevision;const saved=await local.save(ui.identityName.value,ui.identityId.value);if(saved&&revision===draftRevision){profileDirty=false;ui.identityName.value=local.profile.username;ui.identityId.value=local.profile.badge_id;updateIdentity();}renderLocal();announce(local.saveMessage);});
ui.connectLocal.addEventListener('click',async()=>{const connected=await local.connect(true);if(connected&&sequence.phase===3&&!sequence.completed){local.cancelLoad();beginLocalLoad();}renderLocal();refresh();});
new ResizeObserver(resize).observe(pane);window.addEventListener('resize',resize);
const geometryObserver=new ResizeObserver(()=>{layoutDirty=true;refresh();});geometryObserver.observe($('stageCopy'));geometryObserver.observe(ui.identityCard);geometryObserver.observe(ui.inventoryPanel);
document.fonts?.ready.then(()=>{layoutDirty=true;refresh();});
window.addEventListener('pagehide',()=>{generation++;starting=false;sequence.pause();manualPause=true;stopAudio();cancelVoice();local.dispose();cancelAnimationFrame(raf);raf=0;if(audio&&audio.state!=='closed')audio.close();});
window.addEventListener('pageshow',event=>{if(event.persisted){lastFrame=performance.now();void local.connect(true);refresh();announce('预览已恢复。点按画面继续演出。');}});
renderLocal();motionChange();resize();void local.connect();window.DSHEmbed?.ready();
})();
