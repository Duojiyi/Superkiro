# Portal / Pencil 对照验收

只修改 portal。原稿只读，未部署。

## 设计来源

通过 Pencil read_skill（execute / schema）、Get 与 TakeScreenshot 读取：
- 桌面 kZqme，1440px；移动 DPM4O，375px。
- 首屏套餐 upf4Z：624×432，内边距32，圆角24，背景#232529；PRO 60px Archivo 600，价格40px，积分22px #E5ACBF；次级档位17/14/18px三列。
- 底色#ECEEEF，正文#232529，次级#61666E，线#CDD1D5；Noto Sans SC + Archivo。
- 桌面导航96，首屏576，边距64，列间距64；流程48、套餐80、下载72、页脚48垂直内边距。
- 移动边距24，导航72；首屏42px，不展示桌面套餐速览；套餐单列纵向信息，不改成横向小卡。

## 有意保留的差异

- 发布信息、版本、哈希、真实系统要求、不可用/失败状态、重试按钮，以及 macOS 未签名提示均由现有逻辑驱动，不用静态设计中的“即将开放”覆盖真实运营状态。这些内容会增加下载区及首屏状态行高度。
- 文档链接、公告/发布提示、许可与计费说明保留；不以逐像素截图为由隐藏必要安全信息。
- 换绑页没有给定 Pencil 页面，在同一色彩、字体、圆角体系内重新设计；查询、二次确认、解绑结果均为真实状态，不模拟账户。
- 字体为当前文案子集，内嵌在 HTML，访问时不连接字体服务；新增运营文字不在子集时使用系统字体回退。不同系统字体栅格化可能有抗锯齿差异，不能宣称整页零像素误差。

## 已测桌面偏差（1440px，无可用发布测试数据）

| 区域 | Pencil 高度 | 浏览器高度 | 差值 |
| --- | ---: | ---: | ---: |
| 导航 | 96 | 96 | 0 |
| 首屏 | 576 | 576 | 0 |
| 三步流程 | 398 | 397.77 | -0.23 |
| 积分套餐 | 615 | 628.47 | +13.47 |
| 下载区 | 475 | 722.06 | +247.06 |
| 页脚 | 180 | 191.39 | +11.39 |

页面高度约 2612px，设计为2340px。套餐偏差来自浏览器行盒及保留的额外计费说明链接；下载区保留真实发布失败、重试、签名和许可提示；页脚保留44px链接点击高度。这些是实际偏差，不是全页逐像素一致。

首屏卡片实测 x=752、y=164、width=624、height=432；大PRO字号60、价格40。次级三行采用自然宽度的三列 space-between，不等分固定列。移动首屏已补充文案 x=24 与主按钮 width=327 断言。

## 换绑页复核修正

- 动态余额明确带“积分”单位；0显示“0 积分”，缺失值显示“未提供”。
- 查询成功隐藏验证表单，焦点移到授权结果；“更换卡密”清理旧结果、显示表单并聚焦输入。解绑失败也恢复重新验证入口；成功后不显示空验证按钮。
- 换绑页和确认弹窗优先使用完整系统中文无衬线字体：Microsoft YaHei → PingFang SC → Noto Sans CJK SC → Noto Sans SC → sans-serif，避免动态文字依赖首页字体子集。
- 专属测试通过 Chromium CSS.getPlatformFontsForNode 检查动态标题及各个 dt/dd 的真实渲染字体，不只检查 computed font-family。Windows 实测字体报告位于 test-artifacts/pencil-device-fonts.json；其他系统仍需相应平台验收，不宣称跨系统字体逐像素一致。
- 更新桌面/移动 verified、确认、成功和错误截图；新增更换卡密重新验证、解绑失败恢复表单、余额单位及实际字体断言。

## 本地验收

运行：python -m unittest test_superkiro_portal_ui -v

专属视觉断言：python apps/portal-ui/test_pencil_alignment.py -v

截图在 test-artifacts/pencil-*.png。仅使用本地 HTTP 服务及虚构卡密，请求全部 mock，无真实解绑、下载或部署。

## 字体许可

Google Fonts 的 Archivo 与 Noto Sans SC 使用 SIL Open Font License 1.1。下列许可随内嵌子集一起分发；子集不单独销售，不更改字体的版权声明。

### archivo

```text
Copyright 2020 The Archivo Project Authors (https://github.com/Omnibus-Type/Archivo)

This Font Software is licensed under the SIL Open Font License, Version 1.1.
This license is copied below, and is also available with a FAQ at:
http://scripts.sil.org/OFL


-----------------------------------------------------------
SIL OPEN FONT LICENSE Version 1.1 - 26 February 2007
-----------------------------------------------------------

PREAMBLE
The goals of the Open Font License (OFL) are to stimulate worldwide
development of collaborative font projects, to support the font creation
efforts of academic and linguistic communities, and to provide a free and
open framework in which fonts may be shared and improved in partnership
with others.

The OFL allows the licensed fonts to be used, studied, modified and
redistributed freely as long as they are not sold by themselves. The
fonts, including any derivative works, can be bundled, embedded,
redistributed and/or sold with any software provided that any reserved
names are not used by derivative works. The fonts and derivatives,
however, cannot be released under any other type of license. The
requirement for fonts to remain under this license does not apply
to any document created using the fonts or their derivatives.

DEFINITIONS
"Font Software" refers to the set of files released by the Copyright
Holder(s) under this license and clearly marked as such. This may
include source files, build scripts and documentation.

"Reserved Font Name" refers to any names specified as such after the
copyright statement(s).

"Original Version" refers to the collection of Font Software components as
distributed by the Copyright Holder(s).

"Modified Version" refers to any derivative made by adding to, deleting,
or substituting -- in part or in whole -- any of the components of the
Original Version, by changing formats or by porting the Font Software to a
new environment.

"Author" refers to any designer, engineer, programmer, technical
writer or other person who contributed to the Font Software.

PERMISSION & CONDITIONS
Permission is hereby granted, free of charge, to any person obtaining
a copy of the Font Software, to use, study, copy, merge, embed, modify,
redistribute, and sell modified and unmodified copies of the Font
Software, subject to the following conditions:

1) Neither the Font Software nor any of its individual components,
in Original or Modified Versions, may be sold by itself.

2) Original or Modified Versions of the Font Software may be bundled,
redistributed and/or sold with any software, provided that each copy
contains the above copyright notice and this license. These can be
included either as stand-alone text files, human-readable headers or
in the appropriate machine-readable metadata fields within text or
binary files as long as those fields can be easily viewed by the user.

3) No Modified Version of the Font Software may use the Reserved Font
Name(s) unless explicit written permission is granted by the corresponding
Copyright Holder. This restriction only applies to the primary font name as
presented to the users.

4) The name(s) of the Copyright Holder(s) or the Author(s) of the Font
Software shall not be used to promote, endorse or advertise any
Modified Version, except to acknowledge the contribution(s) of the
Copyright Holder(s) and the Author(s) or with their explicit written
permission.

5) The Font Software, modified or unmodified, in part or in whole,
must be distributed entirely under this license, and must not be
distributed under any other license. The requirement for fonts to
remain under this license does not apply to any document created
using the Font Software.

TERMINATION
This license becomes null and void if any of the above conditions are
not met.

DISCLAIMER
THE FONT SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO ANY WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT
OF COPYRIGHT, PATENT, TRADEMARK, OR OTHER RIGHT. IN NO EVENT SHALL THE
COPYRIGHT HOLDER BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
INCLUDING ANY GENERAL, SPECIAL, INDIRECT, INCIDENTAL, OR CONSEQUENTIAL
DAMAGES, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF THE USE OR INABILITY TO USE THE FONT SOFTWARE OR FROM
OTHER DEALINGS IN THE FONT SOFTWARE.

```

### notosanssc

```text
Copyright 2014-2021 Adobe (http://www.adobe.com/), with Reserved Font Name 'Source'

This Font Software is licensed under the SIL Open Font License, Version 1.1.
This license is copied below, and is also available with a FAQ at:
https://scripts.sil.org/OFL


-----------------------------------------------------------
SIL OPEN FONT LICENSE Version 1.1 - 26 February 2007
-----------------------------------------------------------

PREAMBLE
The goals of the Open Font License (OFL) are to stimulate worldwide
development of collaborative font projects, to support the font creation
efforts of academic and linguistic communities, and to provide a free and
open framework in which fonts may be shared and improved in partnership
with others.

The OFL allows the licensed fonts to be used, studied, modified and
redistributed freely as long as they are not sold by themselves. The
fonts, including any derivative works, can be bundled, embedded,
redistributed and/or sold with any software provided that any reserved
names are not used by derivative works. The fonts and derivatives,
however, cannot be released under any other type of license. The
requirement for fonts to remain under this license does not apply
to any document created using the fonts or their derivatives.

DEFINITIONS
"Font Software" refers to the set of files released by the Copyright
Holder(s) under this license and clearly marked as such. This may
include source files, build scripts and documentation.

"Reserved Font Name" refers to any names specified as such after the
copyright statement(s).

"Original Version" refers to the collection of Font Software components as
distributed by the Copyright Holder(s).

"Modified Version" refers to any derivative made by adding to, deleting,
or substituting -- in part or in whole -- any of the components of the
Original Version, by changing formats or by porting the Font Software to a
new environment.

"Author" refers to any designer, engineer, programmer, technical
writer or other person who contributed to the Font Software.

PERMISSION & CONDITIONS
Permission is hereby granted, free of charge, to any person obtaining
a copy of the Font Software, to use, study, copy, merge, embed, modify,
redistribute, and sell modified and unmodified copies of the Font
Software, subject to the following conditions:

1) Neither the Font Software nor any of its individual components,
in Original or Modified Versions, may be sold by itself.

2) Original or Modified Versions of the Font Software may be bundled,
redistributed and/or sold with any software, provided that each copy
contains the above copyright notice and this license. These can be
included either as stand-alone text files, human-readable headers or
in the appropriate machine-readable metadata fields within text or
binary files as long as those fields can be easily viewed by the user.

3) No Modified Version of the Font Software may use the Reserved Font
Name(s) unless explicit written permission is granted by the corresponding
Copyright Holder. This restriction only applies to the primary font name as
presented to the users.

4) The name(s) of the Copyright Holder(s) or the Author(s) of the Font
Software shall not be used to promote, endorse or advertise any
Modified Version, except to acknowledge the contribution(s) of the
Copyright Holder(s) and the Author(s) or with their explicit written
permission.

5) The Font Software, modified or unmodified, in part or in whole,
must be distributed entirely under this license, and must not be
distributed under any other license. The requirement for fonts to
remain under this license does not apply to any document created
using the Font Software.

TERMINATION
This license becomes null and void if any of the above conditions are
not met.

DISCLAIMER
THE FONT SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO ANY WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT
OF COPYRIGHT, PATENT, TRADEMARK, OR OTHER RIGHT. IN NO EVENT SHALL THE
COPYRIGHT HOLDER BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY,
INCLUDING ANY GENERAL, SPECIAL, INDIRECT, INCIDENTAL, OR CONSEQUENTIAL
DAMAGES, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
FROM, OUT OF THE USE OR INABILITY TO USE THE FONT SOFTWARE OR FROM
OTHER DEALINGS IN THE FONT SOFTWARE.

```
