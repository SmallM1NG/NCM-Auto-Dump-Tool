# 开发相关

## 技术栈

- React 19
- Vite 8
- Tauri 2
- Rust 2021
- Node.js / npm

## 环境要求

- Node.js：[待填写版本]
- Rust：[待填写版本]
- Tauri CLI 2
- Windows 开发环境：[待填写]

## 安装依赖

```bash
npm install
```

## 启动开发模式

```bash
npm run tauri dev
```

仅启动前端：

```bash
npm run dev
```

## 构建前端

```bash
npm run build
```

## 构建发行包

```bash
npm run tauri build
```

## 项目结构

```text
src/                 React 前端
src-tauri/src/       Rust 后端、文件监控和转换流程
src-tauri/tauri.conf.json  Tauri 配置
Docs/md/             使用与开发文档
Docs/imgs/           README 截图和图片
```

## 贡献代码

提交 Pull Request 前请确保项目可以正常构建，并说明修改内容、测试方式及可能的兼容性影响。
