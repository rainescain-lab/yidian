# 译点 · YiDian

> 一款简洁的 Windows 桌面翻译器，基于 **Tauri v2**（Rust + WebView2）。
> 在线翻译 · 截图翻译（译文按原文位置覆盖）· 划词翻译 · `.mdx` 词典 · 深色模式。
>
> *A clean desktop translator for Windows, built with Tauri v2. Online translation, in-place screenshot translation, selection translation, `.mdx` dictionaries, dark mode.*

## ✨ 功能

- **在线翻译**：微软 Bing → 谷歌 gtx，**免密钥、免注册**，任意能上网的电脑装完即用；网络不通时自动回退本地模型。
- **截图翻译（Alt+Q）**：拖框选区 → 本地 OCR → **译文直接盖在原文的位置上**（STranslate 式原位覆盖），可一键把原文/译文送进主界面编辑。
- **划词翻译（Alt+W）**：选中任意文字 → 光标旁弹出译文卡片。
- **翻译历史**：SQLite 本地存储，按原文去重置顶、可搜索。
- **`.mdx` 词典**：导入自备的 `.mdx` 词典查词，支持多本管理、启用/排序。
- **本地离线翻译（可选）**：接本机 [Ollama](https://ollama.com/) 的 `qwen2.5` 模型，断网也能翻。
- **深色模式 + 设置页**、**托盘常驻 + 开机自启**。

一切翻译/OCR/查词均走本地或免密钥公开接口，**不需要任何 API Key**。

## ⌨️ 快捷键

| 快捷键 | 功能 |
|---|---|
| `Alt+Q` | 截图翻译（拖框选区，译文原位覆盖） |
| `Alt+W` | 划词翻译（选中文字，光标旁弹卡） |

## 📦 依赖与环境

- **Windows 10 / 11**（x64）。WebView2 运行库缺失时安装包会自动安装。
- **截图 OCR** 需要 [PaddleOCR-json](https://github.com/hiroi-sora/PaddleOCR-json)（本地 OCR，一次输出文字 + 像素框）。见下方「构建/配置」。
- **本地离线翻译（可选）**：[Ollama](https://ollama.com/) + `qwen2.5:7b-instruct` 模型。不装则仅用在线翻译。

## 🔧 从源码构建

前置：[Node.js](https://nodejs.org/) + [pnpm](https://pnpm.io/)、[Rust](https://rustup.rs/)（stable）。

```bash
pnpm install
pnpm tauri dev        # 开发运行
pnpm tauri build      # 打包 NSIS 安装包（输出在 app/src-tauri/target/release/bundle/nsis/）
```

> 首次编译内存吃紧的机器可加 `CARGO_BUILD_JOBS=2` 防 OOM。

### 配置 PaddleOCR（截图 OCR 必需）

1. 从 [PaddleOCR-json releases](https://github.com/hiroi-sora/PaddleOCR-json/releases) 下载（建议 v1.4.1）并解压。
2. **开发时**：设环境变量 `YIDIAN_PADDLE_EXE` 指向解压出的 `PaddleOCR-json.exe`。
3. **打包时**：把整个 PaddleOCR-json 目录放到 `app/src-tauri/resources/paddleocr/`（会随安装包一起分发，装到哪都能用）。
   > ⚠️ 路径请保持全 ASCII（PaddleOCR 对中文路径不友好）。

### 配置 Ollama（可选，离线翻译兜底）

```bash
ollama pull qwen2.5:7b-instruct
```

## 🧱 技术栈

- **前端**：React 19 + Vite（多窗口：主界面 / 截图覆盖层 / 划词卡片 / 结果窗），手写 CSS。
- **后端**：Rust + Tauri v2；`rusqlite`（历史/设置/词典）、`reqwest`（在线引擎）、`rs-mdict`（.mdx）、`xcap`（截屏）。
- **OCR**：PaddleOCR-json（本地子进程）。**在线翻译**：微软 Bing / 谷歌 gtx（均免密钥）。**本地翻译**：Ollama。

## 📄 许可

[MIT](./LICENSE)。

## 🙏 致谢

[Tauri](https://tauri.app/) · [PaddleOCR-json](https://github.com/hiroi-sora/PaddleOCR-json) · [Ollama](https://ollama.com/) · [rs-mdict](https://crates.io/crates/rs-mdict)。
