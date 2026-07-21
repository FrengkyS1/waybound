# Microsoft sign-in

Waybound launches Minecraft with **your own** Microsoft account. Sign-in works
exactly like the CurseForge app — **no setup, no Azure registration**.

Under the hood it uses Microsoft's device-code flow with the public Xbox client
ID (`00000000402b5328`) that the official Minecraft launcher uses. Your password
never touches Waybound; you approve access on Microsoft's own page.

> **Caveat:** reusing the official launcher's client ID is common among
> third-party launchers but is not a Microsoft-sanctioned integration. The
> sanctioned path is registering a dedicated Azure application and obtaining
> Microsoft's approval for the Minecraft API scopes (as Prism Launcher and the
> Modrinth App did); Waybound may move to its own client ID in a future
> release. Sign-in behavior is identical either way.

## Signing in

1. Click **Sign in** (top-right) or **Settings → Account & Launch → Sign in with
   Microsoft**, then **Get device code & sign in**.
2. Waybound shows an 8-character code and opens
   `https://login.live.com/oauth20_remoteconnect.srf`.
3. Enter the code, sign in with the Microsoft account that owns Minecraft: Java
   Edition, and approve. When the page says *"All done!"* you can close it.
4. Waybound completes the Xbox Live → Minecraft services handshake and stores a
   refresh token locally so you stay signed in.

## Troubleshooting

- **"This account has no Xbox profile"** — sign in once at
  [minecraft.net](https://www.minecraft.net) to create the Xbox profile, then
  retry.
- **"Does not own Minecraft: Java Edition"** — the account must own Java Edition
  (Bedrock-only accounts can't launch Java).
- Tokens are stored in `config.toml` under your app config dir
  (`%APPDATA%\dev.waybound` on Windows), **encrypted with Windows DPAPI** and
  bound to your Windows user — the file cannot be decrypted by another user or
  machine. Use **Sign out** to remove the stored account.
