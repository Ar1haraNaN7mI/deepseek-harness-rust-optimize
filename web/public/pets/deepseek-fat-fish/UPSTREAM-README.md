# DeepSeek 大肥鱼 Codex Pet

> A fan-made DeepSeek Fat Fish maid desktop pet for Codex.  
> DeepSeek 蓝色大肥鱼女仆主题的非官方 Codex 动画桌宠。

<p align="center">
  <img src="assets/idle.gif" alt="deepseek-fat-fish idle animation" width="170">
  <img src="assets/hover.gif" alt="deepseek-fat-fish hover animation" width="170">
  <img src="assets/working.gif" alt="deepseek-fat-fish working animation" width="170">
  <img src="assets/success.gif" alt="deepseek-fat-fish success animation" width="170">
</p>

## Preview / 效果预览

| Idle / 待机 | Hover / 鼠标悬停 | Working / 思考工作 | Success / 任务成功庆祝 |
|:---:|:---:|:---:|:---:|
| ![Idle](assets/idle.gif) | ![Hover](assets/hover.gif) | ![Working](assets/working.gif) | ![Success](assets/success.gif) |

## Installation / 安装

1. Download this repository and locate the `deepseek-fat-fish` folder.  
   下载本仓库，并找到 `deepseek-fat-fish` 文件夹。
2. Copy the whole folder into `.codex/pets`.  
   将整个文件夹复制到 `.codex/pets`。
3. Restart Codex, then select **DeepSeek 大肥鱼** from the pet settings.  
   重启 Codex，然后在桌宠设置中选择 **DeepSeek 大肥鱼**。

Windows 的完整安装路径通常是：

```text
C:\Users\<YourName>\.codex\pets\deepseek-fat-fish
```

安装完成后的目录结构：

```text
.codex/pets/deepseek-fat-fish/
├── pet.json
└── spritesheet.webp
```

如果同一 ID 的旧版本已经安装，请替换整个 `deepseek-fat-fish` 文件夹，然后完全退出并重新打开 Codex，以清除图片缓存。

## Animations / 动作

| State / 状态 | Trigger / 触发 | Design / 动作设计 |
|---|---|---|
| `idle` | 无操作 | 呼吸、眨眼、整理女仆头饰，完整鲸尾自然轻摆 |
| `running-right` / `running-left` | 拖动桌宠 | 独立绘制的 8 帧左右小跑循环，头发、裙摆和鲸尾随步态摆动 |
| `waving` | 该桌宠首次显示 | 敬礼停顿后挥手眨眼 |
| `jumping` | 鼠标悬停 | 双手贴胸思考、歪头疑问、举指恍然大悟 |
| `failed` | 任务失败 | 惊住停顿、后仰倒地、叉叉眼“大肥鱼阵亡” |
| `waiting` | 等待用户输入 | 打哈欠，把鲸尾卷成软垫并抱着鲸鱼玩偶睡觉 |
| `running` | Codex 思考/工作 | 坐在小桌前用钢笔写笔记，停笔思考后继续 |
| `review` | 任务成功完成 | 敲两下红金小鼓，举起双鼓槌庆祝 |
| `look` | 鼠标方向变化 | 16 个方向的视线与头部跟随 |

## Package / 文件说明

- `deepseek-fat-fish/pet.json`：桌宠元数据。
- `deepseek-fat-fish/spritesheet.webp`：Codex V2 无损 RGBA 动画图集。
- `assets/*.gif`：从最终图集直接导出的 README 预览，不参与安装。
- 图集规格：1536×2288、8 列×11 行、单格 192×208、`spriteVersionNumber: 2`。
- 最终图集已经通过 Codex V2 格式校验：0 errors、0 warnings、无色键残留、无透明 RGB 残留。
- 本仓库不包含生成原图、废片、过程帧、参考图、提示词或历史版本。

## Fan Work Notice / 同人作品声明

This is an unofficial, non-commercial fan-made desktop-pet package. It is not affiliated with DeepSeek, OpenAI, Codex, or other rights holders. Names, logos, and recognizable designs belong to their respective owners. This repository is intended for personal desktop customization and technical learning; it does not grant commercial rights to any underlying intellectual property.

本项目为非官方、非商业同人桌宠，与 DeepSeek、OpenAI、Codex 及其他权利方无隶属或授权关系。名称、标识及可识别形象的相关权利归各自权利方所有。本仓库仅用于个人桌面定制与技术学习，不授予对任何底层知识产权的商业使用权。
