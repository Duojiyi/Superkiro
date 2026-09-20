> 更新：门户 V2 已于 2026-09-18 代码落地并上线，见 `../PORTAL-V2-DEPLOYMENT.md`。以下为设计交付时的历史记录。

# 门户 V2 与客户桌面 UI 修订

日期：2026-09-18

## 交付范围

- `portal-v2.pen`：Pencil 可编辑源稿，包含原有桌面设计与独立的门户 V2 画板；原稿保留。
- `previews/KAdik.png`：授权查询；`phNkc.png`：激活；`RFhrb.png`：换绑；`qpnpq.png`：充值。
- `previews/Ktpkq.png`：375px 手机查询布局。
- `previews/nFOQl.png`：提交中、连接失败、换绑二次确认。
- 这是门户设计交付，本轮没有修改或部署云端门户页面。反馈文案中的行为是实现要求，不代表线上已实现。

## 设计依据

按 `D:\Desktop\atelier` 的 Product register：variance=4、motion=2、density=5。主要动作优先于装饰；中文正文优先可读；字体为 Noto Sans SC，品牌为 Manrope。

实际访问参考：
- https://www.cssdesignawards.com/ ：查看 2026-09-18 的获奖展示和视觉焦点；首页列出的 Davide Cattaneo 是当日获奖作品。未把其他参考冒称获奖作品。
- https://linear.app ：学习清晰的标题层次、克制的暗色材质和产品内容主导布局。
- https://www.raycast.com ：学习高对比动作入口、紧凑的工具产品表达。
- Awwwards 本机访问失败，没有据此编造调研结论。

访问截图与文字在项目 `.acceptance/design-research/`。借鉴信息层级，不复制品牌、插画或营销动效。

## 视觉与交互

- 底色 #101113，面板 #191B1F，正文 #F4F6FA，次要文字 #A3AAB7，主操作 #476BEB。
- 原有金色授权卡作为识别元素，不使用虚构人数、成功率或在线设备指标。
- 桌面左侧任务表单，右侧授权卡；手机去掉辅助展示，使主操作在首屏内。
- 四个服务入口：查询、激活、换绑、充值。
- 实现须保留：密码默认遮蔽、可见键盘焦点、错误保留输入、提交时禁用重复操作、换绑二次确认、真实响应成功后才显示完成。
- 六张 V2 画板已经检查，无 Pencil 裁切问题。移动端目前交付查询画板，其余操作沿用同一字段堆叠规则，尚未逐页制作独立手机稿。

## 桌面修复

- Debug 与 Release 复用同一份客户界面，仅 Rust CLI 编译配置不同。
- `run_desktop.py` 改为无系统标题栏的 WebView2 原生窗口，不再通过 Chrome App 展示。
- 删除测试导航与 Debug 页脚，去掉浏览器大画布留白。
- 按原设计尺寸打开：激活 441×541、待机 442×544、接管 427×546、诊断 691×358、设置 690×357。
- 原始图片保留；`*-surface.png` 去除写死的示例值，叠加真实输入、按钮和状态。不能将其描述成所有元素均已矢量重建或已完成像素级差异验收。
- 原生最小化、关闭、窗口拖动、页面切换与尺寸调整接入；窗口控制使用当前会话校验。
- `requirements-desktop.txt` 记录 pywebview 依赖。当前是 Python/WebView2 客户端壳配合 Rust Debug CLI，不是独立安装包交付。

## 验证与边界

- `cargo build --locked -p patch-engine --bin patch-cli`：dev profile 编译通过。
- `python -m unittest test_desktop_bridge_regression test_desktop_ui`：8 项通过。
- `python test_desktop_native.py`：真实 WebView2 窗口 441×541 → 设置 690×357 → 返回通过；无底部导航及 Debug 标题。
- 自动化使用模拟 CLI，未激活真实卡密、未修改 Kiro、未证明云端和 IDE 全链路通过。
- 真实 Kiro 交互、生产门户落地及安装包验收仍需单独执行。
