; NopBar direct-download installer (nopbard.com). Inno Setup 6.7 or newer
; (wizard background colours, hidebevels).
;
; Built by scripts\package.ps1 -Installer, which passes the version and paths:
;   ISCC.exe /DAppVersion=0.1.0 /DSourceExe=<...\nopbar.exe> /DOutputDir=<...> packaging\inno\nopbar.iss
; Run by hand from this folder after `cargo build --release -p dock`, the
; defaults below pick up target\release\nopbar.exe and its version.
;
; Per user, no admin: %LOCALAPPDATA%\Programs\NopBar. There is no Inno
; uninstaller: Apps & features runs `nopbar.exe --uninstall`
; (dock/src/release.rs), which removes everything listed here. Keep the two
; in step: the [Files] DestNames are release::INSTALLED_FILES, the shortcut
; is release::SHORTCUT, the AppUserModelID is updates::AUMID, the key is
; release::UNINSTALL_KEY (a test in release.rs reads this file).
;
; User data (%APPDATA%\NopBar, %LOCALAPPDATA%\NopBar) is never touched.

#if VER < EncodeVer(6,7,0)
  #error Inno Setup 6.7 or newer is needed (WizardBackColor, hidebevels)
#endif
#ifndef SourceExe
  #define SourceExe AddBackslash(SourcePath) + "..\..\target\release\nopbar.exe"
#endif
#if !FileExists(SourceExe)
  #error nopbar.exe not found: build it first (cargo build --release -p dock) or pass /DSourceExe=
#endif
#ifndef SourceMcp
  #define SourceMcp ExtractFilePath(SourceExe) + "nopbar-mcp.exe"
#endif
#if !FileExists(SourceMcp)
  #error nopbar-mcp.exe not found: build the nopbar-mcp workspace package
#endif
#ifndef AppVersion
  ; build.rs puts the crate version into ProductVersion.
  #define AppVersion GetStringFileInfo(SourceExe, "ProductVersion")
#endif
#ifndef OutputDir
  #define OutputDir AddBackslash(SourcePath) + "..\..\dist\installer"
#endif
#define RepoRoot AddBackslash(SourcePath) + "..\.."
; package.ps1 passes the combined notices (LICENSE + assets/LICENSE +
; THIRD_PARTY_NOTICES.md); by hand it falls back to the plain LICENSE.
#ifndef LicenseFile
  #define LicenseFile RepoRoot + "\LICENSE"
#endif
#define UninstallKey "Software\Microsoft\Windows\CurrentVersion\Uninstall\NopBar"
#define RunKey "Software\Microsoft\Windows\CurrentVersion\Run"
; Apps & features "Size" (KB): what [Files] installs.
#define EstimatedKB (FileSize(SourceExe) + FileSize(SourceMcp) + FileSize(LicenseFile)) / 1024 + 1

[Setup]
AppId=NopBar
AppName=NopBar
AppVersion={#AppVersion}
AppVerName=NopBar {#AppVersion}
AppPublisher=nopbard
AppPublisherURL=https://nopbard.com/nopbar
AppSupportURL=https://nopbard.com/nopbar
AppUpdatesURL=https://nopbard.com/nopbar/download
AppCopyright=(c) nopbard
VersionInfoVersion={#GetVersionNumbersString(SourceExe)}
VersionInfoProductName=NopBar
; The app checks the setup exe's ProductVersion against the update manifest
; before it runs it (dock/src/autoupdate/trust.rs): the full semver string.
VersionInfoProductTextVersion={#AppVersion}
VersionInfoDescription=NopBar Setup
; Per user, never asks for admin.
PrivilegesRequired=lowest
DefaultDirName={code:InstallDir}
UsePreviousAppDir=no
DisableDirPage=yes
DisableProgramGroupPage=yes
; The Apps & features entry is written below and points at nopbar.exe --uninstall.
Uninstallable=no
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Same floor as version.json min_os (docs/updates.md).
MinVersion=10.0.19045
; Look: follows the system light/dark setting, Windows 11 controls, no
; horizontal lines, plain neutral backgrounds. The pages are Welcome ->
; Installing -> Finished (no Ready page; the Welcome button says Install,
; see CurPageChanged). Silent installs show none of this.
WizardStyle=modern dynamic windows11 hidebevels
WizardBackColor=#f9f9f9
WizardBackColorDynamicDark=#202020
DisableWelcomePage=no
DisableReadyPage=yes
; The gnome mark (packaging\inno\wizard\make-images.py). Transparent PNGs,
; one per DPI step (Setup picks the closest), so no box shows on the
; background above.
WizardImageFile=wizard\large-light-*.png
WizardImageFileDynamicDark=wizard\large-dark-*.png
WizardImageBackColor=none
WizardImageBackColorDynamicDark=none
WizardSmallImageFile=wizard\small-light-*.png
WizardSmallImageFileDynamicDark=wizard\small-dark-*.png
WizardSmallImageBackColor=none
WizardSmallImageBackColorDynamicDark=none
SetupIconFile={#RepoRoot}\assets\brand\nopbar.ico
; The language is the user's Windows display language when this Inno
; Setup has it ([Languages]), else English: never a "Select Language" box.
; (The setup window around this installer picks its own words the way the
; app does: setup/src/strings.rs.)
ShowLanguageDialog=no
; Upgrades: PrepareToInstall asks the dock to quit (nopbar.exe --quit);
; whatever still holds a file (guardian, settings window) is closed here.
CloseApplications=force
CloseApplicationsFilter=nopbar.exe
RestartApplications=no
SetupMutex=NopBarSetup
OutputDir={#OutputDir}
OutputBaseFilename=NopBar-{#AppVersion}-setup
Compression=lzma2/max
SolidCompression=yes

[Languages]
; English always; every other language whose .isl this Inno Setup has
; (official ones ship in its Languages folder; unofficial ones, e.g.
; ChineseSimplified, can be dropped in there from jrsoftware.org/files/istrans).
Name: "en"; MessagesFile: "compiler:Default.isl"
#define HasLang(File) FileExists(AddBackslash(CompilerPath) + AddBackslash("Languages") + File + ".isl")
#define Lang(Name, File) HasLang(File) ? "Name: " + Name + "; MessagesFile: compiler:" + AddBackslash("Languages") + File + ".isl" : "; " + File + ".isl: not installed"
{#Lang("ko", "Korean")}
{#Lang("ja", "Japanese")}
{#Lang("zh_cn", "ChineseSimplified")}
{#Lang("zh_tw", "ChineseTraditional")}
{#Lang("de", "German")}
{#Lang("fr", "French")}
{#Lang("es", "Spanish")}
{#Lang("it", "Italian")}
{#Lang("pt_br", "BrazilianPortuguese")}
{#Lang("pt", "Portuguese")}
{#Lang("ru", "Russian")}
{#Lang("uk", "Ukrainian")}
{#Lang("pl", "Polish")}
{#Lang("cs", "Czech")}
{#Lang("sk", "Slovak")}
{#Lang("sl", "Slovenian")}
{#Lang("hu", "Hungarian")}
{#Lang("bg", "Bulgarian")}
{#Lang("nl", "Dutch")}
{#Lang("da", "Danish")}
{#Lang("fi", "Finnish")}
{#Lang("nb", "Norwegian")}
{#Lang("sv", "Swedish")}
{#Lang("tr", "Turkish")}
{#Lang("he", "Hebrew")}
{#Lang("ar", "Arabic")}
{#Lang("el", "Greek")}
{#Lang("ro", "Romanian")}
{#Lang("hr", "Croatian")}
{#Lang("sr", "SerbianLatin")}
{#Lang("lt", "Lithuanian")}
{#Lang("lv", "Latvian")}
{#Lang("et", "Estonian")}
{#Lang("fa", "Farsi")}
{#Lang("hi", "Hindi")}
{#Lang("bn", "Bengali")}
{#Lang("id", "Indonesian")}
{#Lang("ms", "Malaysian")}
{#Lang("th", "Thai")}
{#Lang("vi", "Vietnamese")}
{#Lang("fil", "Filipino")}

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "nopbar.exe"; Flags: ignoreversion
Source: "{#SourceMcp}"; DestDir: "{app}"; DestName: "nopbar-mcp.exe"; Flags: ignoreversion
Source: "{#LicenseFile}"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
; %APPDATA%\Microsoft\Windows\Start Menu\Programs\NopBar.lnk. The
; AppUserModelID is the one update toasts are shown under.
Name: "{userprograms}\NopBar"; Filename: "{app}\nopbar.exe"; WorkingDir: "{app}"; AppUserModelID: "NopBar.NopBar"

[Registry]
; Apps & features. Rewritten whole on every install (an upgrade keeps no stale value).
Root: HKCU; Subkey: "{#UninstallKey}"; Flags: deletekey
Root: HKCU; Subkey: "{#UninstallKey}"; ValueType: string; ValueName: "DisplayName"; ValueData: "NopBar"
Root: HKCU; Subkey: "{#UninstallKey}"; ValueType: string; ValueName: "DisplayVersion"; ValueData: "{#AppVersion}"
Root: HKCU; Subkey: "{#UninstallKey}"; ValueType: string; ValueName: "Publisher"; ValueData: "nopbard"
Root: HKCU; Subkey: "{#UninstallKey}"; ValueType: string; ValueName: "DisplayIcon"; ValueData: "{app}\nopbar.exe,0"
Root: HKCU; Subkey: "{#UninstallKey}"; ValueType: string; ValueName: "InstallLocation"; ValueData: "{app}"
Root: HKCU; Subkey: "{#UninstallKey}"; ValueType: string; ValueName: "UninstallString"; ValueData: """{app}\nopbar.exe"" --uninstall"
Root: HKCU; Subkey: "{#UninstallKey}"; ValueType: string; ValueName: "QuietUninstallString"; ValueData: """{app}\nopbar.exe"" --uninstall --quiet"
Root: HKCU; Subkey: "{#UninstallKey}"; ValueType: string; ValueName: "URLInfoAbout"; ValueData: "https://nopbard.com/nopbar"
Root: HKCU; Subkey: "{#UninstallKey}"; ValueType: string; ValueName: "InstallDate"; ValueData: "{code:Today}"
Root: HKCU; Subkey: "{#UninstallKey}"; ValueType: dword; ValueName: "EstimatedSize"; ValueData: "{#EstimatedKB}"
Root: HKCU; Subkey: "{#UninstallKey}"; ValueType: dword; ValueName: "NoModify"; ValueData: "1"
Root: HKCU; Subkey: "{#UninstallKey}"; ValueType: dword; ValueName: "NoRepair"; ValueData: "1"
; Start at login, same value the app writes (release::run_key); when, see WantRunAtLogin.
Root: HKCU; Subkey: "{#RunKey}"; ValueType: string; ValueName: "NopBar"; ValueData: """{app}\nopbar.exe"""; Check: WantRunAtLogin

[Run]
; First run shows the Welcome page (config.welcomed). Runs in silent mode too,
; so an unattended upgrade restarts the dock.
Filename: "{app}\nopbar.exe"; WorkingDir: "{app}"; Description: "{cm:LaunchProgram,NopBar}"; Flags: nowait postinstall

[Messages]
; Wizard copy, English and Korean only: other languages keep their .isl text.
; (A [Messages] line without a language prefix would override every language.)
; WelcomeLabel2 is followed by ClickNext, FinishedLabel by ClickFinish.
en.WelcomeLabel1=Install NopBar
en.WelcomeLabel2=A Dock and top bar for Windows.
en.ClickNext=Click Install to begin.
en.InstallingLabel=This takes a moment.
en.FinishedHeadingLabel=NopBar is installed
en.FinishedLabel=You can open it again from the Start menu anytime.
en.FinishedLabelNoIcons=You can open it again from the Start menu anytime.
en.ClickFinish=
#if FileExists(AddBackslash(CompilerPath) + AddBackslash("Languages") + "Korean.isl")
ko.WelcomeLabel1=NopBar를 설치해요
ko.WelcomeLabel2=Windows에서 쓰는 독과 상단바예요.
ko.ClickNext=설치를 누르면 바로 시작해요.
ko.InstallingLabel=잠시만 기다려 주세요.
ko.FinishedHeadingLabel=설치가 끝났어요
ko.FinishedLabel=시작 메뉴에서 언제든 다시 열 수 있어요.
ko.FinishedLabelNoIcons=시작 메뉴에서 언제든 다시 열 수 있어요.
ko.ClickFinish=
#endif

[CustomMessages]
; PrepareToInstall's refusal (an install in another folder). Silent installs
; only log it (the setup window says it in its own words); the wizard shows
; it. Other languages: the setup window's sentence (setup.failed.moved in
; dock/locales, a test keeps them equal), then the folder.
MovedInstall=NopBar is already installed in %1. To install it somewhere else, uninstall it first (Settings > Apps).
#if HasLang("Korean")
ko.MovedInstall=NopBar가 이미 %1에 설치되어 있습니다. 다른 위치에 설치하려면 먼저 제거하세요(설정 > 앱).
#endif
#if HasLang("Japanese")
ja.MovedInstall=別のフォルダーにあります。設定 > アプリで先に削除してください。%n%1
#endif
#if HasLang("ChineseSimplified")
zh_cn.MovedInstall=已安装在其他文件夹。请先在“设置 > 应用”中卸载。%n%1
#endif
#if HasLang("ChineseTraditional")
zh_tw.MovedInstall=已安裝在其他資料夾。請先在「設定 > 應用程式」中移除。%n%1
#endif
#if HasLang("German")
de.MovedInstall=In anderem Ordner installiert. Erst unter Einstellungen > Apps entfernen.%n%1
#endif
#if HasLang("French")
fr.MovedInstall=Installé ailleurs. Supprimez-le dans Paramètres > Applications.%n%1
#endif
#if HasLang("Spanish")
es.MovedInstall=Ya está en otra carpeta. Quítalo en Configuración > Aplicaciones.%n%1
#endif
#if HasLang("Italian")
it.MovedInstall=Installato in un'altra cartella. Rimuovilo da Impostazioni > App.%n%1
#endif
#if HasLang("BrazilianPortuguese")
pt_br.MovedInstall=Instalado em outra pasta. Remova em Configurações > Aplicativos.%n%1
#endif
#if HasLang("Portuguese")
pt.MovedInstall=Instalado noutra pasta. Remova-o em Definições > Aplicações.%n%1
#endif
#if HasLang("Russian")
ru.MovedInstall=Установлен в другой папке. Удалите его: Параметры > Приложения.%n%1
#endif
#if HasLang("Ukrainian")
uk.MovedInstall=Встановлено в іншій папці. Видаліть: Настройки > Програми.%n%1
#endif
#if HasLang("Polish")
pl.MovedInstall=Zainstalowano w innym folderze. Usuń w Ustawienia > Aplikacje.%n%1
#endif
#if HasLang("Czech")
cs.MovedInstall=Je nainstalován v jiné složce. Odeberte ho v Nastavení > Aplikace.%n%1
#endif
#if HasLang("Slovak")
sk.MovedInstall=Je v inom priečinku. Odstráňte ho v Nastavenia > Aplikácie.%n%1
#endif
#if HasLang("Slovenian")
sl.MovedInstall=Nameščen je v drugi mapi. Odstrani ga v Nastavitve > Aplikacije.%n%1
#endif
#if HasLang("Hungarian")
hu.MovedInstall=Másik mappába van telepítve. Távolítsa el: Gépház > Alkalmazások.%n%1
#endif
#if HasLang("Bulgarian")
bg.MovedInstall=Инсталиран е в друга папка. Премахнете го от Настройки > Приложения.%n%1
#endif
#if HasLang("Dutch")
nl.MovedInstall=Staat in een andere map. Verwijder het via Instellingen > Apps.%n%1
#endif
#if HasLang("Danish")
da.MovedInstall=Installeret i en anden mappe. Fjern den under Indstillinger > Apps.%n%1
#endif
#if HasLang("Finnish")
fi.MovedInstall=Asennettu toiseen kansioon. Poista se: Asetukset > Sovellukset.%n%1
#endif
#if HasLang("Norwegian")
nb.MovedInstall=Installert i en annen mappe. Fjern den i Innstillinger > Apper.%n%1
#endif
#if HasLang("Swedish")
sv.MovedInstall=Installerad i en annan mapp. Ta bort den i Inställningar > Appar.%n%1
#endif
#if HasLang("Turkish")
tr.MovedInstall=Başka bir klasörde yüklü. Önce Ayarlar > Uygulamalar'dan kaldırın.%n%1
#endif
#if HasLang("Hebrew")
he.MovedInstall=מותקן בתיקייה אחרת. הסר אותו קודם בהגדרות > אפליקציות.%n%1
#endif
#if HasLang("Arabic")
ar.MovedInstall=مثبّت في مجلد آخر. أزِله أولاً من الإعدادات > التطبيقات.%n%1
#endif
#if HasLang("Greek")
el.MovedInstall=Είναι σε άλλο φάκελο. Καταργήστε το από Ρυθμίσεις > Εφαρμογές.%n%1
#endif
#if HasLang("Romanian")
ro.MovedInstall=Instalat în alt folder. Eliminați-l din Setări > Aplicații.%n%1
#endif
#if HasLang("Croatian")
hr.MovedInstall=Instaliran je u drugoj mapi. Ukloni ga u Postavke > Aplikacije.%n%1
#endif
#if HasLang("SerbianLatin")
sr.MovedInstall=Instaliran je u drugoj fascikli. Ukloni ga u Postavke > Aplikacije.%n%1
#endif
#if HasLang("Lithuanian")
lt.MovedInstall=Įdiegta kitame aplanke. Pašalinkite: Parametrai > Programos.%n%1
#endif
#if HasLang("Latvian")
lv.MovedInstall=Instalēts citā mapē. Vispirms noņem to: Iestatījumi > Programmas.%n%1
#endif
#if HasLang("Estonian")
et.MovedInstall=Paigaldatud teise kausta. Eemalda see: Sätted > Rakendused.%n%1
#endif
#if HasLang("Farsi")
fa.MovedInstall=در پوشهٔ دیگری نصب شده است. اول از تنظیمات > برنامه‌ها حذفش کنید.%n%1
#endif
#if HasLang("Hindi")
hi.MovedInstall=दूसरे फ़ोल्डर में इंस्टॉल है। पहले सेटिंग्स > ऐप्स से हटाएँ।%n%1
#endif
#if HasLang("Bengali")
bn.MovedInstall=অন্য ফোল্ডারে ইনস্টল করা আছে। আগে সেটিংস > অ্যাপ থেকে সরান।%n%1
#endif
#if HasLang("Indonesian")
id.MovedInstall=Terpasang di folder lain. Hapus dulu di Pengaturan > Aplikasi.%n%1
#endif
#if HasLang("Malaysian")
ms.MovedInstall=Dipasang dalam folder lain. Alih keluar dahulu di Tetapan > Apl.%n%1
#endif
#if HasLang("Thai")
th.MovedInstall=ติดตั้งไว้ในโฟลเดอร์อื่น ให้ลบออกก่อนที่ การตั้งค่า > แอป%n%1
#endif
#if HasLang("Vietnamese")
vi.MovedInstall=Đã cài ở thư mục khác. Hãy gỡ trước trong Cài đặt > Ứng dụng.%n%1
#endif
#if HasLang("Filipino")
fil.MovedInstall=Naka-install sa ibang folder. Alisin muna sa Settings > Apps.%n%1
#endif

[Code]
var
  WasInstalled: Boolean;
  OldInstallLoc: String;
  OldRunValue: String;

function InitializeSetup(): Boolean;
begin
  WasInstalled := RegKeyExists(HKCU, '{#UninstallKey}');
  if not RegQueryStringValue(HKCU, '{#UninstallKey}', 'InstallLocation', OldInstallLoc) then
    OldInstallLoc := '';
  OldInstallLoc := Trim(OldInstallLoc);
  if OldInstallLoc <> '' then
    OldInstallLoc := RemoveBackslashUnlessRoot(OldInstallLoc);
  if not RegQueryStringValue(HKCU, '{#RunKey}', 'NopBar', OldRunValue) then
    OldRunValue := '';
  Result := True;
end;

// Upgrades go where the last install went (e.g. a /DIR= install).
function InstallDir(Param: String): String;
begin
  Result := ExpandConstant('{userpf}\NopBar');
  if OldInstallLoc <> '' then
    Result := OldInstallLoc;
end;

// The exe a Run value starts: "C:\x\nopbar.exe" [args], or unquoted up to the first space.
function RunValueExe(V: String): String;
var
  P: Integer;
begin
  V := Trim(V);
  if (Length(V) > 0) and (V[1] = '"') then begin
    Delete(V, 1, 1);
    P := Pos('"', V);
    if P > 0 then
      V := Copy(V, 1, P - 1);
  end else begin
    P := Pos(' ', V);
    if P > 0 then
      V := Copy(V, 1, P - 1);
  end;
  Result := V;
end;

function SamePath(A, B: String): Boolean;
begin
  Result := CompareText(RemoveBackslashUnlessRoot(Trim(A)), RemoveBackslashUnlessRoot(Trim(B))) = 0;
end;

// Fresh install: on (config default start_with_windows = true). Upgrade:
// only when the Run value started the previous install's exe (exactly),
// so a user who switched it off, or points it at another build, keeps that.
function WantRunAtLogin(): Boolean;
var
  Exe: String;
begin
  if not WasInstalled then begin
    Result := True;
    exit;
  end;
  Exe := RunValueExe(OldRunValue);
  Result := (Exe <> '') and
    (((OldInstallLoc <> '') and SamePath(Exe, AddBackslash(OldInstallLoc) + 'nopbar.exe')) or
     SamePath(Exe, ExpandConstant('{app}\nopbar.exe')));
end;

// No Ready page (DisableReadyPage): Welcome is the last page before the
// install starts, so its button says Install. Setup sets the other pages'
// captions itself (Finish on the last one).
procedure CurPageChanged(CurPageID: Integer);
begin
  if CurPageID = wpWelcome then
    WizardForm.NextButton.Caption := SetupMessage(msgButtonInstall);
end;

function Today(Param: String): String;
begin
  Result := GetDateTimeString('yyyymmdd', #0, #0);
end;

// An upgrade must replace the installed copy, not leave it behind in its
// old folder (its Run value and shortcut would point at the old one):
// moving an install (/DIR= elsewhere) is refused; uninstall first.
// Then ask a running dock to quit gracefully (it gives the taskbar back)
// before its files are replaced. The new exe does it from the temp folder:
// older builds have no --quit. Exit code 1 = still running after 5 s;
// CloseApplications then closes what is left.
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Result := '';
  if WasInstalled and (OldInstallLoc <> '') and FileExists(AddBackslash(OldInstallLoc) + 'nopbar.exe')
     and not SamePath(OldInstallLoc, ExpandConstant('{app}')) then begin
    Result := FmtMessage(CustomMessage('MovedInstall'), [OldInstallLoc]);
    exit;
  end;
  ExtractTemporaryFile('nopbar.exe');
  if Exec(ExpandConstant('{tmp}\nopbar.exe'), '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code) then
    Log(Format('nopbar.exe --quit: exit code %d', [Code]))
  else
    Log('nopbar.exe --quit did not start: ' + SysErrorMessage(Code));
end;