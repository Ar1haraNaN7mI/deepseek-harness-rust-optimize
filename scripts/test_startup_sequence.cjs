// Deterministic timing and checkpoint contract shared by the cinema preview.
const assert=require('node:assert/strict');
require('../docs/startup-sequence.js');
const {Sequence,durations,total,checkpoints}=globalThis.DSHBoot;
assert.equal(total,13.8);
assert.deepEqual(durations,[2.1,1.8,2.2,3.4,1.8,2.5]);
assert.deepEqual(checkpoints,[0,2,4]);
const s=new Sequence();
assert.equal(s.waiting,true);
for(const phase of checkpoints){
 assert.equal(s.phase,phase);
 assert.equal(s.playing,false);
 s.choose(2);s.play();
 s.tick(durations[phase]/2);
 s.play(); // Repeated activation never changes elapsed time or advances a phase.
 assert.equal(s.phase,phase);
 assert.equal(s.choose(1),true); // Channel changes are live, not form submission.
 s.tick(durations[phase]/2);
 assert.equal(s.phase,phase+1);assert.equal(s.playing,true);
 s.tick(durations[phase+1]);
 if(phase<4){assert.equal(s.phase,phase+2);assert.equal(s.waiting,true);assert.equal(s.elapsed,0);}
}
assert.equal(s.completed,true);assert.equal(s.time,total);
s.restart();assert.equal(s.waiting,true);assert.equal(s.completed,false);
const auto=new Sequence(false);auto.play();auto.tick(total);
assert.equal(auto.completed,true);
auto.seek(3.9);assert.equal(auto.phase,2);assert.equal(auto.elapsed,0);assert.equal(auto.playing,false);
auto.seek(total);assert.equal(auto.completed,true);
auto.seek(-4);assert.equal(auto.phase,0);assert.equal(auto.elapsed,0);
auto.tick(NaN);auto.jump(99);assert.equal(auto.choose(-1),false);assert.equal(auto.phase,0);
const delayed=new Sequence();delayed.play();delayed.tick(100);
assert.equal(delayed.phase,2);assert.equal(delayed.waiting,true);assert.equal(delayed.elapsed,0);
delayed.jump(5);assert.equal(delayed.completed,false);delayed.play();delayed.tick(durations[5]);assert.equal(delayed.completed,true);
const loading=new Sequence(false);loading.hold(3);loading.play();loading.tick(100);
assert.equal(loading.phase,3);assert.equal(loading.elapsed,durations[3]);assert.equal(loading.held,true);assert.equal(loading.playing,true);
loading.tick(50);assert.equal(loading.time,9.5,'a pending real load must not accumulate hidden time');
loading.release(3);loading.tick(.001);assert.equal(loading.phase,4);assert.equal(loading.held,false);
loading.hold(5);loading.tick(100);assert.equal(loading.phase,5);assert.equal(loading.completed,false,'the final voice can also hold completion');
loading.release(5);loading.tick(.001);assert.equal(loading.completed,true);
loading.restart();assert.equal(loading.barriers.size,0);
console.log('PASS: three checkpoints, real-load and voice barriers, paired scenes, live channel input, no repeated-activation skip, 13.8s auto, seek, replay, and delayed-frame boundaries.');
