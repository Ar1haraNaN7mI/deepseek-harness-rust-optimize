# DSH Harness Web

React、TypeScript 与 Vite 构建的本机 Harness 界面，复用 Rust Runtime 的会话、模型、工具、审批与事件存储。页面不生成示例会话或模拟回复。原启动动画通过 iframe 隔离，与终端共用同一份 DELTA 徽标和音频资源。

## 构建与启动

需要 Node.js 22.12+（已在 22.13.1 验证）及仓库 Rust 工具链。

日常使用推荐在仓库执行 `python scripts/install_dsh.py`，一次安装 CLI 与网页资源。安装后在任何项目目录直接运行 `dsh web --startup`；当前目录作为工作区，网页资源从安装目录的 `share/dsh/web` 读取。

只在源码目录开发时，可手动构建并指定资源：

```powershell
cd web
npm ci
npm run build
cd ..
cargo run -p dsh-cli -- web --assets web/dist
```

打开 `http://127.0.0.1:8770/`。`web/dist` 为本机生成目录，不提交到 Git。后端需要同时提供原动画与 `/api/harness/*`；`vite preview` 只能预览静态产物，不能独立运行 Harness。

不传 `--assets` 时依次查找已安装资源、当前目录 `web/dist`。安装版的网页不会随工作目录切换；`--assets <路径>` 可以显式选择开发构建，指定路径无效会报错。

若 npm 10.9.2 在安装时抛出 Arborist `edgesOut` 内部错误，可使用 `npx --yes npm@11.6.2 ci`。此操作不改系统全局 npm。

开发热更新：先运行同一 Rust 服务，再在 `web` 中执行 `npm run dev`，访问 `http://127.0.0.1:5173`。Vite 将 `/api`、`/startup-*` 和 `/assets/voice` 代理至 8770，并在代理端把 Host、Origin 改为后端地址，以保持后端的同源验证。生产入口由 Rust 直接同源提供。

## 启动偏好

- CLI 显式 `--startup` / `--no-startup` 最优先；否则依次使用网页一次性设置、已保存的网页偏好、服务端默认配置。
- 网页偏好保存在当前浏览器。设置中的“下一次网页开场”只消耗一次；StrictMode 不会重复消耗。
- “下一次终端开场”调用 `/api/startup-next` 保存 CLI 的一次性标志，不会在打开网页时消耗。
- “现在体验开场”覆盖工作台，工作台仍挂载，保留草稿、选中会话和事件连接。完成或跳过后卸载 iframe。
- 自定义名称、档案编号写入 `/api/profile`，只有服务器保存成功才提示成功。

## 真实协议

首次 `GET /api/harness/bootstrap` 获取本机身份、模型就绪状态、已挂载 skills/plugins、会话列表与事件游标。所有修改和 RPC 带 `X-DSH-Token`，令牌只在内存中。

`POST /api/harness/rpc` 使用 `{method,params}`。会话通过 `sessions/list|get|create`，发送通过 `agent/turn`，流式更新通过 `events/wait`，审批通过 `approvals/list|resolve`，暂停通过 `tasks/pause`。事件游标只推进至实际消费的记录，不跳过批次外的事件。缺少模型凭据、工具失败、网络错误均展示真实错误。

刷新页面会恢复已持久化的消息、任务运行状态和待审批工具，并接收之后的事件。尚未结束回复的早期文本片段不会逐字回放；任务结束后，页面会重新读取完整回复。

`StartupGate.tsx` 可复用，校验消息的 origin、source 和随机 channel；仅接受 `{source:'dsh-startup',channel,type:'ready'|'complete'|'skip'}`。默认 URL 为独立动画服务 8769，工作台传同源 `/startup-preview.html`。父页面不会自动绕过动画的点击交互。

## 验证

```powershell
cd web
npm test
npm run build
```

测试覆盖事件顺序、批次游标、快速完成早于请求确认、恢复运行状态、启动开关优先级、一次性消费、iframe 消息隔离与清理。回复通过 `react-markdown` 渲染，不启用 HTML。

实现参考：[React Effect 生命周期](https://react.dev/reference/react/useEffect)、[StrictMode](https://react.dev/reference/react/StrictMode)、[Vite 构建](https://vite.dev/guide/build.html)、[Vite 代理](https://vite.dev/config/server-options.html#server-proxy)。
