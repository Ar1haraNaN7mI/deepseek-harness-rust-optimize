/* Same-origin bridge to a DSH startup host. No simulated inventory or storage. */
(() => {
  'use strict';
  const messageOf = error => error instanceof Error ? error.message : String(error);
  const text = value => typeof value === 'string' ? value : '';
  const emptyInventory = (status = 'idle', message = '') => ({
    status, message, skills: [], plugins: [], issues: [], elapsed_ms: null,
    stages: { skills: 'idle', plugins: 'idle' },
  });
  class Client {
    constructor({ onChange = () => {}, fetch: request = globalThis.fetch?.bind(globalThis) } = {}) {
      this.request = request; this.onChange = onChange;
      this.connection = 'connecting'; this.connectionMessage = '正在连接本地 DSH';
      this.profile = { username: 'OPERATOR', badge_id: 'DSH-0001', workspace: '', sound: true };
      this.inventory = emptyInventory(); this.saving = false; this.saveMessage = '';
      this.token = ''; this._loadGeneration = 0; this._saveGeneration = 0;
      this._connectGeneration = 0; this._connecting = null;
    }
    emit() { this.onChange(this); }
    async failure(response, fallback) {
      let detail = '';
      try { const body = await response.json(); detail = text(body.message) || text(body.error); } catch (_) { /* An HTML error is not an API response. */ }
      return new Error(detail || `${fallback}（HTTP ${response.status}）`);
    }
    connect(force = false) {
      if (this._connecting) return this._connecting;
      if (!force && this.connection === 'local') return Promise.resolve(true);
      if (!force && this.connection === 'unavailable') return Promise.resolve(false);
      const generation = ++this._connectGeneration;
      const controller = new AbortController(); this._profileAbort = controller;
      this.connection = 'connecting'; this.connectionMessage = '正在连接本地 DSH'; this.emit();
      this._connecting = (async () => {
        await Promise.resolve();
        const timer = setTimeout(() => controller.abort(), 8000);
        try {
          if (!this.request) throw new Error('本地接口不可用');
          const response = await this.request('/api/profile', { signal: controller.signal, cache: 'no-store', credentials: 'same-origin' });
          if (!response.ok) throw await this.failure(response, '本地接口不可用');
          const result = await response.json();
          if (result.mode !== 'local' || !text(result.token) || !text(result.username)) throw new Error('本地接口返回了无效资料');
          if (generation !== this._connectGeneration) return false;
          this.token = result.token;
          this.profile = { username: result.username, badge_id: text(result.badge_id) || 'DSH-0001', workspace: text(result.workspace), sound: result.sound !== false, runtime: result.runtime === 'native' ? 'native' : 'rust', inventory_mode: result.inventory_mode === 'mounted' ? 'mounted' : 'discover' };
          this.connection = 'local'; this.connectionMessage = '已连接本地 DSH'; return true;
        } catch (error) {
          if (generation !== this._connectGeneration) return false;
          this.connection = 'unavailable'; this.token = '';
          this.connectionMessage = '未连接 · 请用 dsh startup web 启动'; return false;
        } finally {
          clearTimeout(timer);
          if (generation === this._connectGeneration) { this._connecting = null; this.emit(); }
        }
      })();
      return this._connecting;
    }
    async save(username, badge_id) {
      username = text(username).trim(); badge_id = text(badge_id).trim();
      if (!username || !badge_id) { this.saveMessage = '用户名和编号不能为空'; this.emit(); return false; }
      const generation = ++this._saveGeneration;
      this._saveAbort?.abort(); const controller = new AbortController(); this._saveAbort = controller;
      this.saving = true; this.saveMessage = '正在保存到本地…'; this.emit();
      const timer = setTimeout(() => controller.abort(), 10000);
      try {
        if (!await this.connect()) throw new Error('未连接本地 DSH，资料未保存');
        if (generation !== this._saveGeneration) return false;
        const response = await this.request('/api/profile', { method: 'POST', signal: controller.signal, credentials: 'same-origin', headers: { 'Content-Type': 'application/json', 'X-DSH-Token': this.token }, body: JSON.stringify({ username, badge_id }) });
        if (!response.ok) throw await this.failure(response, '保存失败');
        const result = await response.json();
        if (!text(result.username) || !text(result.badge_id)) throw new Error('保存接口未确认资料');
        if (generation !== this._saveGeneration) return false;
        this.profile = { ...this.profile, username: result.username, badge_id: result.badge_id };
        this.saveMessage = '已保存到本地，下次启动生效'; return true;
      } catch (error) {
        if (generation === this._saveGeneration) this.saveMessage = '保存失败 · ' + (error?.name === 'AbortError' ? '请求超时，请重试' : messageOf(error));
        return false;
      } finally {
        clearTimeout(timer);
        if (generation === this._saveGeneration) { this.saving = false; this.emit(); }
      }
    }
    cancelLoad(reset = true) {
      this._loadGeneration++; this._loadAbort?.abort(); this._loadAbort = null;
      if (reset) this.inventory = emptyInventory();
      else if (this.inventory.status === 'loading') this.inventory = { ...this.inventory, status: 'cancelled', message: '读取已取消，尚未取得完整结果' };
      this.emit();
    }
    async load() {
      this.cancelLoad();
      const generation = this._loadGeneration;
      const controller = new AbortController(); this._loadAbort = controller;
      this.inventory = emptyInventory('loading', '正在读取本地 skills / plugins'); this.emit();
      const items = { skill: new Map(), plugin: new Map() }, issues = new Map();
      const current = () => generation === this._loadGeneration && !controller.signal.aborted;
      const publish = event => {
        if (!current() || !event || typeof event !== 'object') return;
        if (this.inventory.status === 'complete') return;
        if (event.type === 'stage' && ['skills', 'plugins'].includes(event.stage) && ['loading', 'complete'].includes(event.status)) {
          this.inventory = { ...this.inventory, stages: { ...this.inventory.stages, [event.stage]: event.status } };
        } else if (event.type === 'item' && ['skill', 'plugin'].includes(event.kind) && text(event.name) && ['loaded', 'error', 'skipped'].includes(event.status)) {
          const entry = { name: event.name, source: text(event.source), status: event.status, message: text(event.message) };
          items[event.kind].set(entry.source + '\0' + entry.name, entry);
          if (entry.status === 'error') issues.set(event.kind + '\0' + entry.name, { kind: event.kind, name: entry.name, message: entry.message || '读取失败' });
          this.inventory = { ...this.inventory, skills: [...items.skill.values()], plugins: [...items.plugin.values()], issues: [...issues.values()] };
        } else if (event.type === 'complete' && Array.isArray(event.skills) && Array.isArray(event.plugins) && Array.isArray(event.issues)) {
          const skills = event.skills.filter(item => item && text(item.name) && item.status === 'loaded').map(item => ({ name: item.name, source: text(item.source), status: 'loaded' }));
          const plugins = event.plugins.filter(item => item && text(item.name) && item.status === 'loaded').map(item => ({ id: text(item.id), name: item.name, source: text(item.source), tool_count: Number.isFinite(item.tool_count) ? Math.max(0, item.tool_count) : null, status: 'loaded' }));
          const reported = event.issues.filter(item => item && text(item.name)).map(item => ({ kind: text(item.kind), name: item.name, message: text(item.message) || '读取失败' }));
          this.inventory = { ...this.inventory, status: 'complete', message: reported.length ? '读取完成，部分项目需要检查' : '本地读取完成', skills, plugins, issues: reported, elapsed_ms: Number.isFinite(event.elapsed_ms) ? Math.max(0, event.elapsed_ms) : null };
        } else return;
        this.emit();
      };
      const timer = setTimeout(() => controller.abort(), 120000);
      try {
        if (!await this.connect()) {
          if (current()) { this.inventory = emptyInventory('unavailable', '未读取真实数据 · 请用 dsh startup web 启动'); this.emit(); }
          return false;
        }
        if (!current()) return false;
        const response = await this.request('/api/load', { method: 'POST', signal: controller.signal, cache: 'no-store', credentials: 'same-origin', headers: { 'X-DSH-Token': this.token } });
        if (!response.ok) throw await this.failure(response, '读取失败');
        if (!response.body?.getReader) throw new Error('浏览器无法读取实时数据流');
        const reader = response.body.getReader(), decoder = new TextDecoder(); let buffer = '';
        try {
          while (current() && this.inventory.status !== 'complete') {
            const { value, done } = await reader.read();
            if (!current()) break;
            buffer += done ? decoder.decode() : decoder.decode(value, { stream: true });
            let end;
            while (this.inventory.status !== 'complete' && (end = buffer.indexOf('\n')) >= 0) { const row = buffer.slice(0, end).trim(); buffer = buffer.slice(end + 1); if (row) publish(JSON.parse(row)); }
            if (this.inventory.status === 'complete') break;
            if (done) { if (buffer.trim()) publish(JSON.parse(buffer)); break; }
          }
        } finally {
          // The final event is authoritative. Closing the transport afterwards
          // cannot invalidate the loaded catalogs or delay the next chapter.
          if (!current() || this.inventory.status === 'complete') { try { await reader.cancel(); } catch (_) { /* A completed/aborted stream may already be gone. */ } }
          reader.releaseLock();
        }
        if (!current()) return false;
        if (this.inventory.status !== 'complete') throw new Error('连接已结束，但没有收到完整加载结果');
        return true;
      } catch (error) {
        if (generation === this._loadGeneration) {
          this.inventory = { ...this.inventory, status: 'error', message: error?.name === 'AbortError' ? '本地读取超时，请重播后重试' : messageOf(error) }; this.emit();
        }
        return false;
      } finally { clearTimeout(timer); if (generation === this._loadGeneration) this._loadAbort = null; }
    }
    dispose() {
      this.cancelLoad(false); this._connectGeneration++; this._saveGeneration++;
      this._profileAbort?.abort(); this._saveAbort?.abort(); this._connecting = null; this.saving = false;
    }
  }
  globalThis.DSHLocal = Object.freeze({ Client });
})();
