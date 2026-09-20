# 原生 Windows 发布门禁

先本地 Debug 验收，通过后才发布。`publish_native_windows.py` 不再自动选择固定版本或 target 目录。它要求显式 `--source`、`--version`、`--acceptance`，并在建立 SSH 连接前核对验收记录与 EXE 字节摘要、大小、版本和目标平台。

验收记录位于被忽略的 `.acceptance/`，由完成验收的操作员批准，结构如下（值必须来自真正验收的产物，不能把示例当凭据）：

```json
{"approvedForPublication":true,"version":"1.2.3","platform":"windows","arch":"x64","size":123,"sha256":"<SHA-256 of accepted EXE>"}
```

记录是本地操作员的发布授权，不是代码签名或远程可信证明。任何重新编译都必须重新核对和验收，不能只修改记录中的摘要绕过验收。

发布使用已有部署锁，异常时保留锁供人工核查；不要在未知远程命令状态下删锁重试。下载名称包含版本及完整 SHA256，已有文件不覆盖；同时在部署锁内检查历史下载文件，拒绝历史版本号换包。历史产物应保留，不得手工删除以复用版本号；其他平台条目保留，仅原子更新清单。SSH 凭据仍只从标准输入读取，不写参数或日志。

本轮没有执行发布，也未声称 Windows 代码签名、Mac 公证已经完成。

旧 `publish_downloads.py` 仅保留历史离线打包准备，远程发布入口已禁用，不能绕过原生产物验收。

## CI 源码与制品映射

Windows x64、macOS arm64/x64 构建 job 将 `*.provenance.json` 与制品一起上传，其中记录实际 checkout 的 HEAD（PR 构建可能是合并提交）、CI run/attempt、平台、制品字节大小与 SHA256。`version=ci-<run>.<attempt>` 仅为构建标识，不是应用内版本或已批准公开版本。工具拒绝有已暂存、未暂存或未跟踪源码的工作树；不会替当前脏工作树生成虚假提交映射。

provenance 是未签名元数据，不是 SLSA/可信证明、Authenticode、Developer ID、公证或真实 Kiro 验收。`signature_verification=not_performed`，`publication_approved=false`。现有发布批准仍须针对最终字节，签名后摘要变化必须重新生成映射并重新验收。当前 CI 仍只产出未签名候选，不自动对外发布，也未配置任何签名凭据。

发布负责人还须把该映射与公开版本、最终签名验证记录、下载摘要、服务端候选 release-state/image digest、配置 revision 和验收记录关联存档；本次不追认历史 EXE 的源码，不声称已统一生产前后端版本。
