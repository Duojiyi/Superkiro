> 历史版本：新设计与当前桌面实现见 ../portal-pencil-v2/README.md。本页 Chrome App 说明已被 WebView2 原生窗口替代。

# Pencil 门户设计与桌面还原

2026-09-18。`portal-and-desktop.pen` 是本次活动 Pencil 文档的可移植副本，原始桌面五张图未修改。PNG 素材与该文件同目录。

## 云端自助门户设计

- J3qlw：桌面卡密查询。
- wMOmA：云端激活。明确不替代桌面本地接管。
- C5RTfE：设备换绑。
- mqSN7：主卡充值。
- C0RPFY：移动端查询。
- pYG09：换绑二次确认。

这些是 Pencil 可编辑设计图层和导出预览，不是已部署的网站。生产 `/portal` 未在本次修改。

## 桌面实现

`apps/desktop-ui` 使用原始五张图片的天然尺寸与原始纹理，覆盖真实 HTML 控件。五个原图尺寸分别为 441×541、427×546、442×544、691×358、690×357。小窗口等比缩放；新增辅助导航位于画面外。

素材逐字节与 Pencil 原文件相同，SHA-256 见 desktop-assets.json。原稿是扁平位图，不是组件稿；卡密、实时状态、统计数值及未实现功能说明需要覆盖，因此不能把整屏截图声称为逐像素完全一致。动态字段不复用原稿示例值。模型偏好、离线保护和自动清理未实现，明确标记未接入；不提供虚假开关。

网关配置移至弹窗。激活、恢复、解绑保留真实接口和二次确认；卡密不写浏览器存储。已授权展示加速态，未授权展示待机态，不通过预览参数伪造授权。

## 验证

`python -m unittest test_desktop_bridge_regression test_desktop_ui`：8 项通过。测试采用真实本地桥接 HTTP 和 Chromium DOM，但 CLI 响应被模拟；包括失败激活、成功激活、取消、解绑、诊断结果转义、会话刷新与 320/441/680/1280 宽度检查。

`.acceptance/desktop-pencil-*.png` 是五屏浏览器截图。真实 debug CLI status 只读检查：Kiro 1.1.14 Running，authenticated=false，has_snapshot=false。本次未执行真实卡密激活、解绑、生产部署或完整 IDE 对话验收。

启动入口：根目录 `启动Debug客户端.bat`。桌面是 Python 本地安全桥接和 Chrome App 窗口，调用 `target/debug/patch-cli.exe`，不是新增的 Rust 原生 GUI。

