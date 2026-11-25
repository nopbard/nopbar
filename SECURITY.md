# Security policy

NopBar runs as your user, hooks into the Windows shell (taskbar, tray,
window events), launches apps, reads the clipboard when you use the
clipboard history, and checks one HTTPS URL for updates. Bugs in those
areas can matter, so please report them privately.

## Reporting a vulnerability

Email **nopbard@gmail.com** with:

- what the problem is and what an attacker could do with it;
- steps to reproduce (a small proof of concept if you have one);
- the NopBar version (Settings > About) and your Windows version;
- relevant lines from `%APPDATA%\NopBar\dock.log`, if any.

Please do **not** open a public issue or pull request for a security
problem, and give us a reasonable time to fix it before disclosing it.

You will get a reply within 7 days. We will tell you when a fix is
released and credit you in the release notes if you want.

## Supported versions

Only the latest release gets security fixes. NopBar updates are offered
inside the app and on [GitHub Releases](https://github.com/nopbard/nopbar-releases/releases/latest).

## Scope

In scope: the `nopbar.exe` app and its installer and uninstaller, the
update check, and the [NopBar website](https://nopbard.com/nopbar).

Out of scope: problems that need an attacker who already runs code as the
same user with the same rights (they can do anything NopBar can), and
issues in Windows or third-party apps NopBar launches.
