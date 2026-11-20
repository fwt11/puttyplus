; PuTTYPlus 的 Windows 安装包脚本（Inno Setup 6，阶段 121）。
;
; ⚠️ 本文件是 **UTF-8 带 BOM**（Inno 6 的 Unicode 编译器靠 BOM 识别 UTF-8，
;    没 BOM 的中文会按系统代码页解读 ⇒ CI 的英文 runner 上必乱码）。
;    编辑本文件时别把开头的 BOM 抹了。
;
; ⚠️ **装出来的目录层级必须跟 gvsbuild 一致**（bin\ + share\ 两级）：
;    GTK 运行库按「自己 DLL 所在目录的名字是 bin ⇒ 上一级是安装前缀」
;    找图标主题 ⇒ share\icons 必须在 bin\ 旁边。全平铺的话图标主题找不到，
;    工具栏小图标全变「缺图占位符」（用户 2026-11-20 报的现象）。
;
; ⚠️ **路径要传绝对路径**：ISCC 解析 Source:/OutputDir 是相对**脚本所在
;    目录**（tools\），不是相对当前工作目录 ⇒ StageDir 必须从命令行传
;    绝对路径（tools\win_installer.ps1 与 ci-windows.yml 都是这么传的）。
;    用法：iscc /DAppVersion=0.8.0 /DStageDir=<绝对路径> tools\windows-installer.iss
;    产物：{StageDir}\puttyplus-windows-x86_64-setup.exe
;
; 设计口径（用户 2026-11-20 拍板）：
;   - 每用户安装，免管理员：PrivilegesRequired=lowest ⇒ {autopf} 落在
;     %LOCALAPPDATA%\Programs，装/卸都不弹 UAC；
;   - zip 绿色版退场，Release 里 Windows 只留这一个安装包。
;
; ⚠️ 每个条目写一行，**别用行尾 \ 续行**（那是 NSIS 的写法，Inno 不认）。

#ifndef AppVersion
#define AppVersion "0.0.0"
#endif

#ifndef StageDir
#define StageDir "release-windows"
#endif

[Setup]
; AppId 用固定 GUID ⇒ 升级/卸载认得出来是同一个程序。
; ⚠️ 别换这个 GUID：换了之后老安装装不进新版本（互相视为不同程序）。
AppId={{3BBC686E-DF08-4F54-A62E-831881D4F3C0}
AppName=PuTTYPlus
AppVersion={#AppVersion}
DefaultDirName={autopf}\PuTTYPlus
PrivilegesRequired=lowest
DisableProgramGroupPage=yes
; 升级时程序开着 ⇒ 老的 exe/DLL 被占用装不进去 ⇒ 让安装器先试着关掉它
CloseApplications=yes
LicenseFile=..\LICENSE
SetupIconFile=..\assets\puttyplus.ico
UninstallDisplayIcon={app}\bin\puttyplus.exe
OutputDir={#StageDir}
OutputBaseFilename=puttyplus-windows-x86_64-setup
; ~130MB 的 GTK DLL 用 lzma2/solid 压（SVG 图标主题压缩率尤其好）
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "chinese"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: checkedonce

[Files]
Source: "{#StageDir}\bin\puttyplus.exe"; DestDir: "{app}\bin"; Flags: ignoreversion
Source: "{#StageDir}\bin\*.dll"; DestDir: "{app}\bin"; Flags: ignoreversion
Source: "{#StageDir}\share\icons\*"; DestDir: "{app}\share\icons"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\PuTTYPlus"; Filename: "{app}\bin\puttyplus.exe"
Name: "{group}\{cm:UninstallProgram,PuTTYPlus}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\PuTTYPlus"; Filename: "{app}\bin\puttyplus.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\bin\puttyplus.exe"; Description: "{cm:LaunchProgram,PuTTYPlus}"; Flags: nowait postinstall skipifsilent unchecked
