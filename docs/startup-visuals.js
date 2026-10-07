/* DSH / DEEP DIVE: original, flat-vector institutional opening sequence.
 * All paths and pixels are drawn here. No copied marks, media, 3D objects,
 * dependencies, or network calls. The host owns copy, identity cards and audio.
 */
(() => {
  'use strict';
  const TAU = Math.PI * 2;
  const clamp = (n, a = 0, b = 1) => Math.max(a, Math.min(b, n));
  const mix = (a, b, t) => a + (b - a) * t;
  const fract = n => n - Math.floor(n);
  const ease = n => 1 - Math.pow(1 - clamp(n), 3);
  const smooth = n => { n = clamp(n); return n * n * (3 - 2 * n); };
  const part = (p, a, b) => clamp((p - a) / (b - a));
  const hash = n => fract(Math.sin(n * 127.1 + 311.7) * 43758.5453);

  const emblemParts = window.DSHEmblem.parts;
  const overshoot = n => {const t=clamp(n)-1;return 1+2.5*t*t*t+1.5*t*t;};

  function draw(ctx, width, height, state = {}) {
    if (!ctx || width <= 0 || height <= 0) return;
    const phase = clamp(Math.floor(Number(state.phase)||0),0,5), reduced = !!state.reducedMotion;
    const p = reduced ? [.65,.72,.7,.72,.85,1][phase] : clamp(Number(state.progress)||0);
    const waiting = !!state.waiting;
    const channel = clamp(Math.floor(Number(state.choice)||0),0,2);
    const time = reduced ? 0 : Number(state.time)||0;
    const ambient = reduced ? 0 : Number(state.ambientTime ?? state.time)||0;
    const drag = reduced ? 0 : Number(typeof state.drag === 'object' ? state.drag.x : state.drag)||0;
    const impulse = reduced ? 0 : clamp(Number(state.impulse)||0);
    const pointer = state.pointer || {}, xValue = Array.isArray(pointer)?pointer[0]:pointer.x, yValue = Array.isArray(pointer)?pointer[1]:pointer.y;
    const nx = clamp(Number.isFinite(xValue)?xValue:.5), ny = clamp(Number.isFinite(yValue)?yValue:.5);
    const unit = Math.min(width,height)/1000, W=width/unit, H=height/unit;
    const wx = x => (x-.5)*W, wy = y => (y-.5)*H;
    const px = reduced?0:(nx-.5)*26, py = reduced?0:(ny-.5)*20;
    const spin = (reduced?0:ambient*.46+time*1.35+drag*2.6)+channel*TAU/3;
    const pulse = reduced?1:1+Math.sin(ambient*1.65)*.025;
    const light = phase > 0 && state.light !== false;
    const C = phase===0 ? {bg:'#030506',ink:'#f2f4f2',soft:'#859397',line:'#28363d',accent:'#b5ced8',cool:'#82b3c5',ghostA:'#bd7174',ghostB:'#3a94b1'} : light ?
      {bg:'#eeeae2',ink:'#162126',soft:'#68767b',line:'#b9c3c3',accent:'#c5793f',cool:'#41889f',ghostA:'#b9696e',ghostB:'#2c819a'} :
      {bg:'#0b1116',ink:'#e6edf0',soft:'#8497a1',line:'#2b424e',accent:'#82bfd2',cool:'#5596b2',ghostA:'#bd829d',ghostB:'#61b4d3'};
    C.accent=(light?['#c5793f','#29848e','#8a668e']:['#82bfd2','#81c1a7','#b59ad9'])[channel];
    const identity = String(state.identity || 'OPERATOR').trim().slice(0,28) || 'OPERATOR';
    const inventory=state.inventory||{status:'idle',skills:[],plugins:[],issues:[]};

    function line(x1,y1,x2,y2,color=C.line,alpha=1,lw=1) {
      ctx.globalAlpha=clamp(alpha);ctx.strokeStyle=color;ctx.lineWidth=lw;ctx.beginPath();ctx.moveTo(x1,y1);ctx.lineTo(x2,y2);ctx.stroke();ctx.globalAlpha=1;
    }
    function rect(x,y,w,h,color=C.ink,alpha=1,filled=true,lw=1) {
      ctx.globalAlpha=clamp(alpha);if(filled){ctx.fillStyle=color;ctx.fillRect(x,y,w,h);}else{ctx.strokeStyle=color;ctx.lineWidth=lw;ctx.strokeRect(x,y,w,h);}ctx.globalAlpha=1;
    }
    function poly(points,color,alpha=1,stroke=null,lw=1) {
      ctx.globalAlpha=clamp(alpha);ctx.beginPath();points.forEach(([x,y],i)=>i?ctx.lineTo(x,y):ctx.moveTo(x,y));ctx.closePath();if(color){ctx.fillStyle=color;ctx.fill();}if(stroke){ctx.strokeStyle=stroke;ctx.lineWidth=lw;ctx.stroke();}ctx.globalAlpha=1;
    }
    function dot(x,y,r,color=C.ink,alpha=1) {ctx.globalAlpha=clamp(alpha);ctx.fillStyle=color;ctx.beginPath();ctx.arc(x,y,r,0,TAU);ctx.fill();ctx.globalAlpha=1;}
    function arc(x,y,r,a,b,color=C.ink,alpha=1,lw=1) {ctx.globalAlpha=clamp(alpha);ctx.strokeStyle=color;ctx.lineWidth=lw;ctx.beginPath();ctx.arc(x,y,r,a,b);ctx.stroke();ctx.globalAlpha=1;}
    function text(str,x,y,size=13,color=C.soft,alpha=1,spacing=0,align='center',weight=400,mono=false) {
      ctx.save();ctx.globalAlpha=clamp(alpha);ctx.fillStyle=color;ctx.textBaseline='middle';ctx.font=`${weight} ${size}px ${mono?'"Cascadia Code", "Consolas"':'"Bahnschrift", "Arial", "Microsoft YaHei"'},sans-serif`;
      if(spacing){const chars=[...str],full=chars.reduce((s,c)=>s+ctx.measureText(c).width,0)+(chars.length-1)*spacing;let at=align==='center'?x-full/2:align==='right'?x-full:x;chars.forEach(c=>{ctx.fillText(c,at,y);at+=ctx.measureText(c).width+spacing;});}
      else{ctx.textAlign=align;ctx.fillText(str,x,y);}ctx.restore();
    }
    function emblem(x,y,scale=1,assembly=1,fill=1,color=C.ink,alpha=1) {
      ctx.save();ctx.translate(x,y);ctx.scale(scale,scale);
      emblemParts.forEach((contours,i)=>{
        const t=reduced?1:overshoot(part(assembly,i*.045,.78+i*.045));
        const side=i===0?-1:i===1?1:i%2?-1:1;
        ctx.save();ctx.translate(side*(1-t)*(i<2?250:340),(1-t)*(i<2?side*90:(i-3)*110));
        ctx.rotate((1-t)*side*.36);
        ctx.globalAlpha=clamp(alpha*(.22+clamp(assembly)*.78)*(fill>0?fill:1));ctx.beginPath();
        contours.forEach(points=>{points.forEach(([xx,yy],j)=>j?ctx.lineTo(xx,yy):ctx.moveTo(xx,yy));ctx.closePath();});
        if(fill>0){ctx.fillStyle=color;ctx.fill('evenodd');}
        if(fill<.9){ctx.strokeStyle=color;ctx.lineWidth=1.1;ctx.stroke();}ctx.globalAlpha=1;
        ctx.restore();
      });
      ctx.restore();
    }
    function wordmark(x,y,h=50,tracking=1,color=C.ink,opacity=1) {
      text('DSH',x,y+h*.53,h*1.28,color,opacity,h*.10*tracking,'center',700);
    }
    function glare(x,y,r,alpha,tint='255,254,245') {
      if(alpha<=0)return;const g=ctx.createRadialGradient(x,y,0,x,y,Math.max(1,r));g.addColorStop(0,`rgba(${tint},${clamp(alpha)})`);g.addColorStop(.25,`rgba(${tint},${clamp(alpha*.76)})`);g.addColorStop(.58,`rgba(${tint},${clamp(alpha*.24)})`);g.addColorStop(1,`rgba(${tint},0)`);ctx.fillStyle=g;ctx.fillRect(x-r,y-r,r*2,r*2);
    }
    function streak(x,y,w,alpha) {
      if(w<=0||alpha<=0)return;const g=ctx.createLinearGradient(x-w/2,y,x+w/2,y);g.addColorStop(0,'rgba(213,238,246,0)');g.addColorStop(.45,`rgba(235,249,255,${alpha*.52})`);g.addColorStop(.5,`rgba(255,255,255,${alpha})`);g.addColorStop(.55,`rgba(235,249,255,${alpha*.52})`);g.addColorStop(1,'rgba(213,238,246,0)');ctx.fillStyle=g;ctx.fillRect(x-w/2,y-1.5,w,3);ctx.globalAlpha=.2;ctx.fillRect(x-w/2,y-6,w,12);ctx.globalAlpha=1;
    }
    function paper() {
      // Fixed deterministic grain, with a gentle corner falloff rather than a gradient hero.
      for(let i=0;i<300;i++){const x=(hash(i+4)-.5)*W,y=(hash(i+807)-.5)*H;rect(x,y,.8+hash(i+390)*1.1,.65,C.ink,.022+hash(i+69)*.03);}
      const shade=ctx.createRadialGradient(0,wy(.43),Math.min(W,H)*.17,0,wy(.43),Math.max(W,H)*.8);shade.addColorStop(0,'rgba(0,0,0,0)');shade.addColorStop(1,light?'rgba(82,80,70,.08)':'rgba(0,0,0,.3)');ctx.fillStyle=shade;ctx.fillRect(-W/2,-H/2,W,H);
      emblem(wx(.80),wy(.38),3.4,1,0,C.ink,.027);
      const inset=38;[[-W/2+inset,-H/2+inset,1,1],[W/2-inset,-H/2+inset,-1,1],[-W/2+inset,H/2-inset,1,-1],[W/2-inset,H/2-inset,-1,-1]].forEach(([x,y,sx,sy])=>{line(x,y,x+sx*21,y,C.ink,.3,.75);line(x,y,x,y+sy*21,C.ink,.3,.75);});
      const y=wy(.76);line(wx(.055),y,wx(.095),y,C.soft,.45,.8);line(wx(.905),y,wx(.945),y,C.soft,.45,.8);
      text('DEPTH / ACCESS',wx(.08),wy(.72),10,C.soft,.65,1,'left',400,true);
      if(W>1450)text('ENGINEERING DIVISION',wx(.94),wy(.26),11,C.soft,.6,1.4,'right',400,true);
    }
    function dataFall(strength=.6) {
      const columnX=wx(.86),start=wy(.27),rows=10,base=Math.floor((time+ambient*.11)*26)+channel*317;
      text('BUFFER',columnX,start-26,10,C.soft,strength,1.5,'left',400,true);
      for(let row=0;row<rows;row++){
        const value=Math.floor(hash(base+row*109+7)*65535).toString(16).toUpperCase().padStart(4,'0');
        const a=strength*(.24+hash(row+base+409)*.65);text(value,columnX,start+row*17,11,row===base%rows?C.accent:C.soft,a,1.2,'left',400,true);
        if(row%2===0)rect(columnX-10,start+row*17-2,3,3,C.accent,a);
      }
    }
    function scan(amount=.4) {
      if(reduced)return;
      const t=fract(time*.72+ambient*.035),y=mix(wy(.15),wy(.80),t),x1=wx(.06),x2=wx(.94);
      line(x1,y,x2,y,C.cool,amount*.52,1.5);
      const g=ctx.createLinearGradient(0,y-11,0,y+1);g.addColorStop(0,'rgba(79,136,158,0)');g.addColorStop(1,`rgba(79,136,158,${amount*.045})`);ctx.fillStyle=g;ctx.fillRect(x1,y-11,x2-x1,12);
    }
    const inGap = a => {a=((a%TAU)+TAU)%TAU;return a>Math.PI*.24&&a<Math.PI*.76;};
    function brokenArc(radius,rotation,length,color,alpha,lw) {
      const steps=Math.max(24,Math.ceil(length*35));
      ctx.globalAlpha=clamp(alpha);ctx.strokeStyle=color;ctx.lineWidth=lw;ctx.beginPath();let down=false;
      for(let i=0;i<=steps;i++){const a=rotation+length*i/steps,x=Math.cos(a)*radius,y=Math.sin(a)*radius;if(inGap(a)){down=false;continue;}if(down)ctx.lineTo(x,y);else{ctx.moveTo(x,y);down=true;}}ctx.stroke();ctx.globalAlpha=1;
    }
    function reticle(strength=1,contract=0) {
      const kick=reduced||waiting?0:Math.sin(part(p,0,.25)*Math.PI)*(1-part(p,0,.25));
      const cy=wy(.47),radius=Math.min(W*.425,H*.425)*(1-contract*.17+kick*.24);
      ctx.save();ctx.translate(px,cy+py);
      // Lower opening remains fixed, while independently rotating bands and markers
      // pass behind it. Identity text and subtitles always retain clear space.
      brokenArc(radius,Math.PI*.76,Math.PI*1.48,C.ink,strength*.76,4.7);
      brokenArc(radius-14,0,TAU,C.soft,strength*.35,.8);
      brokenArc(radius-29,spin*1.35+.4,Math.PI*1.4,C.ink,strength*.8,2.4);
      brokenArc(radius-49,-spin*1.85-1.2,Math.PI*1.12,C.accent,strength*.9,5.5);
      brokenArc(radius-62,-spin*1.85-.9,Math.PI*.48,C.accent,strength*.22,12);
      brokenArc(radius+17,-spin*.23,Math.PI*1.25,C.soft,strength*.31,.7);
      for(let i=0;i<84;i++){
        const a=i*TAU/84+spin*.12;if(inGap(a))continue;const major=i%7===0,l=major?13:4;
        line(Math.cos(a)*(radius+25),Math.sin(a)*(radius+25),Math.cos(a)*(radius+25+l),Math.sin(a)*(radius+25+l),major?C.ink:C.soft,strength*(major?.62:.38),major?.9:.65);
      }
      for(let i=0;i<12;i++){
        const a=i*TAU/12-spin*.25;if(inGap(a))continue;const r=radius-30;
        dot(Math.cos(a)*r,Math.sin(a)*r,i%3===0?4.1:1.8,i%3===0?C.accent:C.ink,strength*(i%3===0?.88:.48));
      }
      [-1,1].forEach(side=>{const x=side*(radius-83);line(x,-20,x,-7,C.soft,strength*.35,.7);line(x,7,x,20,C.soft,strength*.35,.7);line(x,side<0?-20:20,x-side*9,side<0?-20:20,C.soft,strength*.35,.7);});
      ctx.restore();
    }
    function blackIntro() {
      const q=waiting||reduced?1:p,assembly=part(q,0,.35),stamp=ease(part(q,.18,.44));
      const cy=wy(.40),s=1.70*pulse*(waiting||reduced?1:1.22-.22*ease(part(p,0,.48)));
      // The seal is the hero. The un-stretched signature has a clear subordinate role.
      emblem(px,cy+py,s,assembly,1,C.ink,1);
      wordmark(0,cy+185,46,1.1,C.ink,stamp);
      line(-96,cy+255,96,cy+255,C.soft,.44*stamp,.8);
      text('HARNESS / INTELLIGENCE DIV.',0,cy+284,11,C.soft,stamp,2.8);
      text('深 潜 协 议',0,cy+320,17,C.ink,stamp,3.2);
      for(const side of [-1,1]){
        const x=side*Math.min(W*.35,345);
        line(x,cy-16,x,cy+16,C.soft,.55,1);line(x-7,cy,x+7,cy,C.soft,.55,1);
        text(side<0?'MODEL':'HARNESS',x,cy+43,9,C.soft,.68,1.5);
      }
      if(!reduced&&!waiting){
        impact(0,cy,part(p,.23,.51),p>.23&&p<.51?1:0);
        if(p>.32&&p<.46){const t=part(p,.32,.46);chromaticSeal(px,cy+py,s,Math.sin(t*Math.PI)*13);}
        if(p>.58&&p<.82){const t=part(p,.58,.82),x=mix(-W*.7,W*.7,t);ctx.save();ctx.transform(1,0,-.36,1,0,0);rect(x-14,-H/2,28,H,C.ink,.42*Math.sin(t*Math.PI));rect(x-72,-H/2,120,H,C.cool,.10*Math.sin(t*Math.PI));ctx.restore();}
        if(p>.82){const t=ease(part(p,.82,1));rect(-W/2,-H/2,W,H,state.light!==false?'#eeeae2':'#0b1116',t);}
      }
    }
    function permission() {
      paper();
      const show=ease(part(p,0,.42));
      emblem(px,wy(.60)+py,.80*pulse,part(p,0,.44),1,C.ink,.95);
      const y=wy(.72);for(let i=0;i<13;i++)rect(-73+i*12,y,5,1.8,C.ink,i<Math.floor(13*show)?.8:.18);
      scan(.55);flightLines(.38);shutters();
    }
    function verification() {paper();reticle(.94);dataFall(.42);flightLines(.3);shutters();}
    function capabilityField(complete=false) {
      paper();
      // The moving cuts are decoration, not progress. The host overlays the
      // complete, immediately updated server inventory in these two columns.
      const top=wy(.36),bottom=wy(.86),left=wx(.105),right=wx(.895);
      line(0,top+18,0,bottom-35,C.line,.65,1);
      for(const side of [-1,1]){
        const x=side<0?left:right;
        line(x,top,x+side*18,top-26,C.ink,.6,2);
        line(x,bottom,x+side*18,bottom+26,C.ink,.6,2);
        rect(x-side*5,top+44,3,44,C.accent,.55);
        for(let i=0;i<8;i++)line(x+side*22,top+82+i*25,x+side*(i%3===0?32:26),top+82+i*25,C.soft,.36,.9);
      }
      if(!reduced){
        const sweep=fract(ambient*.72),x=mix(left-75,right+75,sweep);
        ctx.save();ctx.beginPath();ctx.rect(left-24,top-24,right-left+48,bottom-top+48);ctx.clip();
        ctx.transform(1,0,-.30,1,0,0);
        rect(x-24,top-140,50,bottom-top+260,C.cool,complete?.02:.045);
        rect(x,top-140,2.5,bottom-top+260,C.cool,complete?.16:.46);ctx.restore();
        for(let i=0;i<8;i++){
          const t=fract(ambient*1.7+i*.139),side=i%2?-1:1,y=wy(.11)+i*19;
          const x=side*(W*.52-t*W*.24);line(x,y,x+side*(24+62*(1-t)),y,C.accent,(1-t)*.35,2);
        }
      }
      const flash=reduced?0:Math.sin(part(p,0,.23)*Math.PI)*(1-part(p,0,.23));
      if(flash>0){
        for(const side of [-1,1])poly([[side*W*.58,top-105],[side*W*.37,top-105],[side*W*.22,bottom+100],[side*W*.40,bottom+100]],C.ink,flash*.08);
      }
      if(complete){
        const known=inventory.status==='complete',issue=inventory.issues.length>0;
        const label=known?(issue?'RESULT / CHECK ISSUES':'RESULT / LOCAL'): 'RESULT / NOT READ';
        text(label,0,wy(.88),10,issue?C.accent:C.soft,.7,2);
      }
    }
    function requestRead() {capabilityField(false);}
    function readComplete() {capabilityField(true);}
    function welcome() {
      paper();
      const enter=ease(part(p,.015,.24)),cy=wy(.425),s=1.42*(reduced?1:1+.18*(1-ease(part(p,0,.35))));
      if(p<.25)reticle((1-part(p,0,.25))*.8,.4);
      text('WELCOME TO',0,cy-190+(1-enter)*-45,18,C.ink,enter,7);
      emblem(px*.4,cy+py*.4,s,part(p,0,.32),1,C.ink,1);
      const boxY=cy+151;rect(-116,boxY,232,72,C.ink,enter);
      wordmark(0,boxY+16,40,1.1,light?'#fffcf5':C.bg,enter);
      text('HARNESS / INTELLIGENCE DIV.',0,cy+244,10,C.soft,enter,2.2);
      text('欢迎 '+identity+' 访问',0,cy+280,21,C.ink,enter,2);
      if(!reduced){
        if(p>.22&&p<.42)chromaticSeal(px*.4,cy+py*.4,s,Math.sin(part(p,.22,.42)*Math.PI)*18);
        impact(0,cy,part(p,.24,.63),p>.24&&p<.63?1:0);
        if(p>.36&&p<.68){const t=part(p,.36,.68),a=Math.sin(t*Math.PI);streak(0,cy,W*1.5*ease(t),a);rect(-W/2,cy-2,W,4,C.ink,a*.45);}
      }
      flightLines(.7*(1-enter));shutters();
    }
    function chromaticSeal(x,y,s,amount) {
      if(reduced||amount<.1)return;
      [-1,1].forEach(side=>{
        ctx.save();ctx.globalCompositeOperation=light?'multiply':'screen';ctx.beginPath();
        for(let i=0;i<4;i++)ctx.rect(x-220,y-100+i*59+Math.sin(time*35+i)*12,440,8+i*3);
        ctx.clip();emblem(x+side*amount,y,s,1,1,side<0?C.ghostA:C.ghostB,.70);ctx.restore();
      });
    }
    function impact(x,y,t,strength) {
      if(reduced||!strength||t<=0||t>=1)return;
      const a=(1-t)*strength,r=90+ease(t)*Math.min(W,H)*.62;
      arc(x,y,r,0,TAU,C.cool,a*.65,2+8*a);
      arc(x,y,r*.78,spin,spin+Math.PI*1.6,C.ink,a*.35,1.5);
      for(let i=0;i<24;i++){
        const angle=i*TAU/24+.12,rr=r*(.82+hash(i)*.23),len=(30+hash(i+90)*80)*a;
        line(x+Math.cos(angle)*rr,y+Math.sin(angle)*rr,x+Math.cos(angle)*(rr+len),y+Math.sin(angle)*(rr+len),i%3?C.ink:C.cool,a*.65,i%3?1:3);
      }
    }
    function flightLines(strength) {
      if(reduced||waiting)return;
      // Long radial streaks stay in the outer field, leaving reading areas clean.
      for(let i=0;i<24;i++){
        const a=hash(i+804)*TAU,t=fract(time*1.7+hash(i+90)),r=260+t*Math.max(W,H)*.58,l=(26+hash(i+10)*95)*(1-t);
        line(Math.cos(a)*r,wy(.47)+Math.sin(a)*r,Math.cos(a)*(r+l),wy(.47)+Math.sin(a)*(r+l),i%4?C.soft:C.accent,strength*(1-t)*.52,i%4?.8:2);
      }
    }
    function shutters() {
      if(reduced||waiting||p>.16)return;
      const t=ease(part(p,0,.16));ctx.save();ctx.transform(1,0,-.32,1,0,0);
      for(const side of [-1,1]){const x=side*(W*.29+t*W*.58);rect(x-W*.18,-H*.55,W*.36,H*1.1,C.ink,(1-t)*.17);rect(x-W*.18,-H*.55,5,H*1.1,C.accent,(1-t)*.7);}
      ctx.restore();
    }
    function clickResponse() {
      if(reduced||impulse<.001)return;const x=(nx-.5)*W,y=(ny-.5)*H,t=1-impulse;
      arc(x,y,10+ease(t)*410,0,TAU,C.accent,impulse*.82,1+impulse*3);arc(x,y,4+ease(t)*270,-.4,4.7,C.cool,impulse*.52,1.4);
      for(let i=0;i<12;i++){const a=i*TAU/12,r=22+ease(t)*310;line(x+Math.cos(a)*r,y+Math.sin(a)*r,x+Math.cos(a)*(r+32*impulse),y+Math.sin(a)*(r+32*impulse),C.cool,impulse*.65,1.5);}
    }

    ctx.save();
    try {
      ctx.globalAlpha=1;ctx.globalCompositeOperation='source-over';ctx.lineCap='butt';ctx.lineJoin='miter';ctx.setLineDash([]);ctx.fillStyle=C.bg;ctx.fillRect(0,0,width,height);
      ctx.translate(width/2,height/2);ctx.scale(unit,unit);
      [blackIntro,permission,verification,requestRead,readComplete,welcome][phase]();
      clickResponse();
    } finally {ctx.restore();}
  }
  window.DSHVisuals=Object.freeze({draw,phaseCount:6});
})();
