# Signing in

Search, playback, radio and lyrics all work without an account. Sign in to
get your library, liked songs, history, and the ♥ next to songs you've liked.

pova signs in the same way your browser does: with your YouTube Music
cookies. There's no OAuth app and no password prompt. You just let pova borrow
the login your browser already has.

### Inside pova (easiest)

1. Open the **Library** tab (`b`), or click **○ signed out** in the player border.
2. Pick your browser (e.g. **Sign in with Firefox**) and press **Enter** (or click it).

If you're already signed in to <https://music.youtube.com> in that browser,
pova signs in right away in a couple of seconds. If not, pova opens
<https://music.youtube.com> in your browser so you can sign in with your Google
account normally, and finishes signing in automatically once you're done.

Under the hood, pova reads only the `youtube.com` cookies through `yt-dlp` and
saves them to `~/.config/pova/cookie.txt` (mode `600`). On KDE/GNOME, your
desktop keyring may ask to unlock Chromium/Chrome cookies.

Need a specific browser profile? Pick **Other browser / profile…** and enter
something like `chrome:Profile 2` or `firefox:work`.

To sign out again, open the Esc menu and choose **Sign out** (or click
**● signed in**).

### From the terminal

```sh
pova login                        # auto-detects your browser
pova login chrome                 # or firefox, chromium, brave, edge, vivaldi, opera
pova login "chrome:Profile 2"     # a specific browser profile
pova logout                       # remove the saved cookie
```

If pova is already running, it picks up the sign-in (or sign-out) within a
couple of seconds, no restart needed. (The standalone `pova-import-cookie` /
`./scripts/import-cookie.sh` script also still works.)

### Manual paste

If your browser isn't supported by `yt-dlp`:

1. Open <https://music.youtube.com> in your browser while signed in.
2. Open DevTools, go to **Network**, and click any request to `music.youtube.com`.
3. Copy the full value of the **Cookie** request header.
4. In pova's **Library** tab, pick **Paste cookie manually…** and paste it
   (pova masks the value and saves `~/.config/pova/cookie.txt` with mode `600`).

**Treat `cookie.txt` like a password.** It grants access to your Google
account. It stays on your machine and pova only sends it to YouTube Music.
Google replaces these cookies now and then. When that happens pova switches to
"○ signed out" and tells you: pick your browser in the Library tab (or run
`pova login`) to sign back in.

Back to the [README](../README.md).
