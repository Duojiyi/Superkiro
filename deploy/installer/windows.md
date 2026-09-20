# Windows 用户级免 UAC 安装与代码签名发布规范 (P3-10)

**规范章节**：Spec §9, §14.6, P0-3  
**适用范围**：Kiro BYOK Desktop Client 桌面端打包、安装包生成与分发

---

## 1. Windows 用户级安装免 UAC (Per-User Installation)

### 1.1 安装路径选择
为避免触发 Windows UAC（用户账户控制）提权弹窗并防止写入受保护的 `C:\Program Files` 触发文件虚拟化或防病毒拦截，客户端必须严格采用 **当前用户级安装（Per-User）** 模式：
- **目标安装路径**：`%LOCALAPPDATA%\Programs\KiroBYOK\`
- **用户数据与快照目录**：`%APPDATA%\KiroBYOK\`
- **快捷方式入口**：
  - 开始菜单：`%APPDATA%\Microsoft\Windows\Start Menu\Programs\Kiro BYOK.lnk`
  - 桌面（可选）：`%USERPROFILE%\Desktop\Kiro BYOK.lnk`

### 1.2 NSIS 配置参数 (Tauri / NSIS)
在 `tauri.conf.json` 或 NSIS 脚本中配置：
```json
{
  "bundle": {
    "windows": {
      "nsis": {
        "installMode": "currentUser",
        "displayLanguageSelector": false,
        "languages": ["SimpChinese", "English"]
      }
    }
  }
}
```
对应底层 NSIS 脚本指令：
```nsis
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\KiroBYOK"
```
**优势**：普通标准受限账户即可双击一键静默或快速安装，无需管理员密码，无任何 UAC 提示。

---

## 2. Windows Authenticode 二进制代码签名 (Code Signing)

### 2.1 签名目标与时机
针对 Windows Defender / SmartScreen 误报问题，在 CI/CD 发布构建产物后执行自动化签名：
1. `kiro-byok-client.exe` 主程序；
2. 安装包 `Kiro-BYOK-Setup-x64.exe`；
3. 任何自解压与辅助更新二进制。

### 2.2 签名脚本示例 (`deploy/installer/sign_windows.ps1`)
```powershell
param(
    [Parameter(Mandatory=$true)][string]$FilePath,
    [Parameter(Mandatory=$false)][string]$CertThumbprint,
    [Parameter(Mandatory=$false)][string]$TimestampUrl = "http://timestamp.digicert.com"
)

Write-Host "Signing $FilePath with SHA256..."
signtool.exe sign `
    /sha1 $CertThumbprint `
    /fd SHA256 `
    /tr $TimestampUrl `
    /td SHA256 `
    /d "Kiro BYOK Desktop Manager" `
    $FilePath

signtool.exe verify /pa /v $FilePath
Write-Host "Signing and verification completed successfully."
```

---

## 3. macOS 边界控制（只碰未签名扩展）

对照 P0-3 决策与 Spec §9：
- macOS 系统对 `.app` 主程序 Bundle 施加严格的 Gatekeeper 与公证（Notarization）检查。篡改主程序签名必定导致无法打开。
- **本方案严格禁止修改 macOS `Kiro.app/Contents/MacOS/Kiro` 主程序**。
- **改道与补丁边界**：
  1. 优先采用启动器环境变量注入（`LSEnvironment` 或 wrapper）；
  2. 补丁只碰用户级扩展脚本（`~/.kiro/extensions/kiro.kiro-agent/` 或扩展目录下的未签名 JS 资源），绕开苹果沙盒签名校验，保障 100% 稳定性。

---

## 4. 首启免责提示（精简一句话规范）

根据 Spec §14.6 用户明确要求（不做冗长法务条款留痕与强制复选框）：
- **展示形式**：首启时在主界面底部以常驻提示条或轻量横幅展示：
> **免责提示**：本客户端仅用于开发者自有模型接口转接及本地开发环境辅助，严格保持本地操作可逆，随时可一键还原官方状态。
- **技术实现**：在 `ClientPreferences` 中记录 `disclaimer_acknowledged: true`，界面永久保留一键回滚官方入口。
