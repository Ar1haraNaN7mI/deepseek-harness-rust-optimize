/* Deterministic playback state. No DOM, timers, sound, or inference calls. */
(() => {
  'use strict';
  const durations = Object.freeze([2.1, 1.8, 2.2, 3.4, 1.8, 2.5]);
  const checkpoints = Object.freeze([0, 2, 4]);
  const total = Math.round(durations.reduce((sum, n) => sum + n, 0) * 1000) / 1000;
  class Sequence {
    constructor(interactive = true) {
      this.interactive = interactive;
      this.choices = [0, 0, 0, 0, 0];
      this.restart();
    }
    restart() { this.phase = 0; this.elapsed = 0; this.playing = false; this.completed = false; this.barriers = new Set(); }
    hold(phase) { if (Number.isInteger(phase) && phase >= 0 && phase <= 5) this.barriers.add(phase); }
    release(phase) { this.barriers.delete(phase); }
    get held() { return this.playing && this.barriers.has(this.phase) && this.elapsed >= durations[this.phase]; }
    get progress() { return Math.min(1, this.elapsed / durations[this.phase]); }
    get waiting() { return this.interactive && !this.playing && !this.completed && this.elapsed === 0 && checkpoints.includes(this.phase); }
    get time() { return Math.min(total, durations.slice(0, this.phase).reduce((s, n) => s + n, 0) + this.elapsed); }
    choose(index) {
      if (this.phase === 5 || !Number.isInteger(index) || index < 0 || index > 2) return false;
      this.choices[this.phase] = index; return true;
    }
    play() { if (this.completed) this.restart(); this.playing = true; }
    pause() { this.playing = false; }
    jump(phase) {
      if (!Number.isInteger(phase) || phase < 0 || phase > 5) return;
      this.phase = phase; this.elapsed = 0;
      this.completed = false; this.playing = false;
    }
    seek(time) {
      if (!Number.isFinite(time)) return;
      time = Math.max(0, Math.min(total, time));
      this.phase = 0;
      while (this.phase < 5 && time >= durations[this.phase] - 1e-9) { time -= durations[this.phase]; this.phase++; }
      this.elapsed = Math.max(0, Math.min(durations[this.phase], time));
      this.completed = this.phase === 5 && this.elapsed >= durations[5] - 1e-9;
      this.playing = false;
    }
    tick(delta) {
      if (!this.playing || !Number.isFinite(delta) || delta <= 0) return;
      this.elapsed += delta;
      while (this.elapsed >= durations[this.phase] - 1e-9) {
        if (this.barriers.has(this.phase)) { this.elapsed = durations[this.phase]; break; }
        const rest = Math.max(0, this.elapsed - durations[this.phase]);
        if (this.phase === 5) { this.elapsed = durations[5]; this.completed = true; this.playing = false; break; }
        this.phase++;
        this.elapsed = 0;
        if (this.interactive && checkpoints.includes(this.phase)) { this.playing = false; break; }
        this.elapsed = rest;
      }
    }
  }
  globalThis.DSHBoot = Object.freeze({ Sequence, durations, total, checkpoints });
})();
