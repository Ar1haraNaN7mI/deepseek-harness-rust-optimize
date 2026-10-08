/* Runs only in the startup iframe. Keep native routes away from official APIs. */
(() => {
  const request = window.fetch.bind(window);
  window.fetch = (input, init) => request(typeof input === 'string' && (['/api/profile', '/api/load', '/api/access'].includes(input) || input.startsWith('/api/access/') || input.startsWith('/assets/voice/')) ? '/__dsh_startup' + input : input, init);
  document.addEventListener('DOMContentLoaded', () => {
    document.documentElement.dataset.runtime = 'native';
    const badge = document.querySelector('.brand-bottom em');
    if (badge) badge.textContent = 'NATIVE';
    const help = document.querySelector('.control-panel .help');
    if (help) help.textContent = 'Enter / Space · 继续当前阶段　Esc · 返回官方 Harness　C · 收起设置　M · 静音。真实数据来自官方 DSH 的默认预设与当前工作区；使用 dsh-native startup next on 控制下一次开启，dsh-native startup profile 修改身份。';
  });
})();
