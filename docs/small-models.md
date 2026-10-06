# 小于 70B 的模型与开源后端

dsh-rust 现在把“小模型优化”放在 harness 层，而不是绑死某个推理服务。`llm.optimization.mode = "auto"` 会在模型 ID 含有 `32b`、`14b`、`8x7b`、`mini`、`flash` 等信号时启用紧凑策略；`70b` 及以上保持标准策略。别名模型可以显式设置：

```toml
[llm.optimization]
mode = "auto"
parameter_count_b = 32.0
```

也可以临时使用 `DSH_MODEL_SIZE_B=32`。策略会同时收紧工具 schema 数量、系统提示/上下文消息字符预算、工具结果长度和输出 token，并关闭 thinking（均可在 `config/default.toml` 覆盖）。完整会话仍保存在 durable session log 中，不会被破坏。

需要强制分支时可设置 `DSH_MODEL_OPTIMIZATION=small|standard|off`。

## 可替换的开源方案

这些服务都通过同一个 OpenAI-compatible `/v1/chat/completions` 接口接入，因此 MCP 之外不需要为每个运行时增加一套工具协议：

| 后端 | 默认地址 | 适合场景 |
|---|---|---|
| Ollama | `http://127.0.0.1:11434/v1` | 最简单的桌面/单机模型管理，适合 7B–32B 量化模型 |
| LM Studio | `http://127.0.0.1:1234/v1` | 桌面 GUI、模型下载/加载与本地 OpenAI-compatible 服务 |
| llama.cpp server | `http://127.0.0.1:8080/v1` | CPU/GPU 混合、GGUF、低显存部署 |
| vLLM | `http://127.0.0.1:8000/v1` | GPU 服务、连续 batching、高并发 |
| SGLang | `http://127.0.0.1:30000/v1` | 高吞吐推理和结构化生成 |
| LiteLLM | `http://127.0.0.1:4000/v1` | 统一多个本地/远端 provider，做 fallback 与路由 |
| LocalAI | `http://127.0.0.1:8080/v1` | 自托管模型网关、模型别名和多后端统一 API |
| TGI | `http://127.0.0.1:8080/v1` | Hugging Face GPU 推理服务与连续 batching |
| MLX-LM | `http://127.0.0.1:8080/v1` | Apple Silicon 上的低内存本地推理 |

配置示例：

```toml
[llm]
backend = "ollama"
base_url = "" # 空值使用后端默认地址
model = "qwen2.5:14b"

[llm.optimization]
mode = "auto"
```

LM Studio 启动 Local Server 后会监听默认的 `http://127.0.0.1:1234/v1`；因此只写
这个地址而不写 `backend` 时，dsh-rust 也会自动识别为 `lm_studio`。如果主服务偶尔不可用，可以在同一个配置中声明有序 fallback。请求只有在
收到首个流式 delta 之前才会切换，不会重复已经展示给用户的输出：

```toml
[[llm.fallbacks]]
backend = "llama_cpp"
base_url = "http://127.0.0.1:8080/v1"
model = "qwen2.5-14b-instruct"

[[llm.fallbacks]]
backend = "ollama"
base_url = "http://127.0.0.1:11434/v1"
model = "qwen2.5:14b"
```

也可以用 `DSH_LLM_FALLBACK_BASE_URLS` 传入逗号分隔的地址。每个 fallback 默认
复用主模型、凭据和协议；需要不同模型或 backend 时使用上面的 TOML 形式。
远端 fallback 不要把密钥直接写入配置，可设置 `api_key_env = "MY_PROVIDER_KEY"`。
为避免把 DeepSeek/OpenAI 密钥误发给本机 Ollama/llama.cpp，loopback 地址默认不带
`Authorization`；本地网关确实启用鉴权时再设置 `DSH_LLM_SEND_API_KEY=1`。

## Provider-specific 调优

OpenAI-compatible 服务器之间的非标准字段不应硬编码到 agent loop。可通过
`[llm].extra_body` 或 `DSH_LLM_EXTRA_BODY` 透传 JSON；核心字段（model、messages、
stream、tools 等）会被保护，不能被覆盖。例如 llama.cpp 的 prompt cache：

```toml
[llm]
backend = "llama_cpp"
base_url = "http://127.0.0.1:8080/v1"
model = "qwen2.5-14b-instruct"
extra_body = { cache_prompt = true }
```

对于 Qwen 等支持模板开关的服务，也可以透传关闭 thinking：

```toml
extra_body = { chat_template_kwargs = { enable_thinking = false } }
```

同一入口也可透传服务端的 speculative decoding 参数（例如 vLLM 的
`speculative_model` 或 llama.cpp 的 draft-model 选项）；不同运行时的字段保持在
`extra_body` 中，避免污染通用协议。

小模型分支还会把每个工具的描述和 JSON Schema 做确定性裁剪（保留 `type`、
`properties`、`required`、枚举与约束），完整 schema 仍在 core 中用于 preflight
校验，并默认请求服务端串行返回工具调用（`parallel_tool_calls = false`）。可用
`small_tool_schema_chars`、`compact_tool_schemas` 和 `small_parallel_tool_calls`
调整或关闭。

服务端启动示例（任选其一）：

```bash
ollama run qwen2.5:14b
./llama-server -m ./qwen2.5-14b-instruct-q4_k_m.gguf --host 127.0.0.1 --port 8080
python -m vllm.entrypoints.openai.api_server --model Qwen/Qwen2.5-14B-Instruct --port 8000
python -m sglang.launch_server --model-path Qwen/Qwen2.5-14B-Instruct --port 30000
text-generation-launcher --model-id Qwen/Qwen2.5-14B-Instruct --port 8080
python -m mlx_lm server --model Qwen/Qwen2.5-7B-Instruct --port 8080
# LM Studio: 在 GUI 中加载模型并打开 Developer > Local Server（默认 1234）
```

vLLM/SGLang 的工具调用需要在服务端启用对应的 tool-calling/chat-template 选项；
llama.cpp 和 LM Studio 需要使用支持 function calling 的 chat template。dsh-rust 侧不依赖这些
运行时的 Python 包，未启用时会把服务端返回的能力错误原样记录到事件流。

或在单次运行中覆盖：

```powershell
$env:DSH_MODEL_SIZE_B = "14"
$env:DSH_LLM_BASE_URL = "http://127.0.0.1:11434/v1" # 可选
cargo run -p dsh-cli -- --backend ollama -m qwen2.5:14b "检查这个项目的测试"
cargo run -p dsh-cli -- debug backends
cargo run -p dsh-cli -- debug model-profile
```

远程 app-server 也提供只读 `llm/status`，返回 backend、ready 状态和完整的
optimization profile，便于桌面端或编排器在提交任务前选择本地/远端模型。

组合建议：Ollama 或 LM Studio 负责桌面/单机模型生命周期，llama.cpp 负责 GGUF/量化，vLLM 或
SGLang/TGI 负责 GPU batching/高吞吐，LocalAI 负责多后端统一入口，LiteLLM 负责
多模型路由与 fallback，MLX-LM 适合 Apple Silicon；dsh-rust
负责小模型的工具/上下文裁剪、JSON Schema 工具契约和可审计执行。这样不需要把
LangChain、AutoGen 等重量级编排器嵌进内核，也能按需在外层通过插件或 app-server
接入它们。

MCP 仍然适合跨进程工具发现；上述后端解决的是模型推理、量化、batching 和 fallback。
二者可以同时使用，权限、审批、PathGuard 与审计仍由 dsh-rust core 负责。TGI、MLX-LM、
llama.cpp 和 LM Studio 的工具调用依赖 chat template，`dsh debug backends` 与 app-server 的
`llm/status` 会标记这种兼容性风险。若服务端忽略 `stream=true` 而返回单个 JSON
completion，dsh-rust 也会把它投影成相同的事件流。

如果需要更高层的开源编排，可把 dsh-rust app-server 当作稳定的 JSON-RPC 执行面，
在外层接入 LangGraph、LlamaIndex、Haystack 或 AutoGen；工具仍以 OpenAI function
calling/JSON Schema 形式传递，不必把这些重量级依赖放进 Rust 核心。这样形成三层
组合：MCP/OpenAPI 负责发现，dsh-rust 负责权限与审计，Ollama/LM Studio/llama.cpp/vLLM 等
负责推理。
