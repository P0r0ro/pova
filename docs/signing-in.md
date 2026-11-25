# Signing in

Search, playback, radio and lyrics all work without an account. Sign in to
get your library, liked songs, history, and the ♥ next to songs you've liked.

pova signs in the same way your browser does: with your YouTube Music
cookies. There's no OAuth app and no password prompt. You just let pova borrow
the cookie your browser already has.

### The easy way

If you're signed in to <https://music.youtube.com> in your browser:

```sh
./scripts/import-cookie.sh chrome     # or firefox, chromium, brave, edge, vivaldi, opera
```

It asks yt-dlp to read your browser's cookie store (on KDE, Chrome's cookies
are unlocked through KWallet), keeps only the youtube.com cookies, and writes
them as a single line to `~/.config/pova/cookie.txt` with permissions `600`.
It never prints the cookie values. To use a specific browser profile, write
`chrome:Profile 2`.

### The manual way

1. Open <https://music.youtube.com> in your browser while signed in.
2. Open DevTools, go to **Network**, and click any request to `music.youtube.com`.
3. Copy the full value of the **Cookie** request header.
4. Paste it on one line into `~/.config/pova/cookie.txt`, then run
   `chmod 600 ~/.config/pova/cookie.txt`.

Within a couple of seconds the bottom of the player box says **● signed in**.
pova notices the new cookie by itself, even while it's running.

### Brand Accounts and multiple Google accounts

If your playlists and liked songs live on a YouTube Brand Account (channel) or
a secondary Google account in the same browser session, press `u` in the
**Library** tab (`b`) or choose **Switch account** in the `esc` menu. pova lists
the accounts and Brand channels available in your session, shows the active one
in the player border (`● <name>`), and remembers your choice between runs.
Nothing extra is queried on startup unless you open the switcher.

**Treat `cookie.txt` like a password.** It grants access to your Google
account. It stays on your machine and pova only sends it to YouTube Music. Google
replaces these cookies now and then. When that happens pova switches to
"○ signed out" and shows a message saying so: run the script again and pova
signs back in on its own, no restart needed.

Back to the [README](../README.md).
